//! VDD engine: orchestrates the adversarial review loop (advisory + blocking modes).

use std::time::Duration;

use chrono::Utc;
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::{AppConfig, VddConfig, VddMode};
use crate::providers::ApiKey;
use crate::proxy::ChatCompletionRequest;
use crate::session::TokenUsage;

use crate::vdd::confabulation::ConfabulationTracker;
use crate::vdd::error::{VddAdvisoryResult, VddBlockingResult, VddError, VddResult};
use crate::vdd::finding::{Finding, FindingStatus};
use crate::vdd::helpers::{extract_user_task, format_findings_for_injection};
use crate::vdd::parsing::extract_response_text;
use crate::vdd::prompts::{build_adversary_request, build_revision_request};
use crate::vdd::review::{AdversaryReview, VddIteration, VddSession};
use crate::vdd::sink::{create_chainlink_issues, persist_session};
use crate::vdd::static_analysis::{run_shell_command, StaticAnalysisResult};
use crate::vdd::transport::{send_to_adversary, send_to_builder};
use crate::vdd::triage::{parse_findings, triage_findings, TriageContext};

/// The core VDD engine that orchestrates adversarial review loops.
pub struct VddEngine {
    pub(crate) config: VddConfig,
    pub(crate) app_config: AppConfig,
    pub(crate) client: Client,
}

/// Per-iteration inputs for the blocking loop. Bundled into a struct so
/// `run_iteration` can take a single argument without tripping the
/// `too_many_arguments` lint.
struct IterationContext<'a> {
    builder_text: &'a str,
    original_task: &'a str,
    static_results: &'a [StaticAnalysisResult],
    iteration: u32,
    previous_fps: &'a [String],
    builder_provider: &'a str,
    builder_api_key: Option<&'a ApiKey>,
}

impl VddEngine {
    #[must_use]
    pub fn new(config: &VddConfig, app_config: &AppConfig, client: Client) -> Self {
        Self {
            config: config.clone(),
            app_config: app_config.clone(),
            client,
        }
    }

    /// Simplified entry point for chat loop integration.
    /// Takes the builder text and user task, plus builder auth for the
    /// AI verification agent (which uses the builder's provider, not the
    /// adversary's, to avoid correlated confabulation).
    ///
    /// # Errors
    /// Returns an error if the adversary request fails or the response cannot be parsed.
    pub async fn review_text(
        &self,
        builder_text: &str,
        user_task: &str,
        builder_provider: &str,
        builder_api_key: Option<&ApiKey>,
    ) -> Result<VddAdvisoryResult, VddError> {
        if !self.config.enabled {
            return Ok(VddAdvisoryResult {
                findings: vec![],
                context_injection: String::new(),
                static_analysis: vec![],
                tokens_used: TokenUsage::default(),
            });
        }

        // Skip VDD for very short responses (likely simple answers, not code)
        if builder_text.len() < 100 {
            return Ok(VddAdvisoryResult {
                findings: vec![],
                context_injection: String::new(),
                static_analysis: vec![],
                tokens_used: TokenUsage::default(),
            });
        }

        info!(
            mode = %self.config.mode,
            adversary = %self.config.adversary.provider,
            "VDD: Starting adversarial review"
        );

        // Run static analysis
        let static_results = self.run_static_analysis().await;

        // Build and send adversary request
        let adversary_request = build_adversary_request(
            &self.config,
            &self.app_config,
            builder_text,
            user_task,
            &static_results,
            1,
        );

        let (adversary_text, tokens_used) = send_to_adversary(
            &self.client,
            &self.config,
            &self.app_config,
            &adversary_request,
        )
        .await?;

        // Parse and triage findings (AI verifier uses builder's provider)
        let mut findings = parse_findings(&adversary_text, 1);
        let triage_ctx = TriageContext {
            client: &self.client,
            config: &self.config,
            app_config: &self.app_config,
            previous_fps: &[],
            builder_code: builder_text,
            builder_provider,
            builder_api_key,
        };
        triage_findings(&mut findings, &triage_ctx).await;

        // Build context injection string
        let context_injection = format_findings_for_injection(&findings, &static_results);

        let genuine_count = findings
            .iter()
            .filter(|f| f.status == FindingStatus::Genuine)
            .count();

        info!(
            total = findings.len(),
            genuine = genuine_count,
            "VDD advisory: review complete"
        );

        Ok(VddAdvisoryResult {
            findings,
            context_injection,
            static_analysis: static_results,
            tokens_used,
        })
    }

    /// Main entry point — called by proxy after builder responds.
    /// Routes to advisory or blocking mode based on config.
    ///
    /// # Errors
    /// Returns an error if the adversary request or builder revision fails.
    pub async fn process_response(
        &self,
        builder_response: &Value,
        original_request: &ChatCompletionRequest,
        builder_provider: &str,
        builder_api_key: Option<&ApiKey>,
    ) -> Result<VddResult, VddError> {
        if !self.config.enabled {
            return Ok(VddResult::Skipped("VDD disabled".to_string()));
        }

        // Extract text content from builder response
        let builder_text = extract_response_text(builder_response);
        if builder_text.is_empty() {
            return Ok(VddResult::Skipped(
                "Builder response has no text content".to_string(),
            ));
        }

        // Skip VDD for very short responses (likely simple answers, not code)
        if builder_text.len() < 100 {
            return Ok(VddResult::Skipped(
                "Response too short for adversarial review".to_string(),
            ));
        }

        info!(
            mode = %self.config.mode,
            adversary = %self.config.adversary.provider,
            "VDD: Starting adversarial review"
        );

        match self.config.mode {
            VddMode::Advisory => {
                let result = self
                    .advisory_review(
                        &builder_text,
                        original_request,
                        builder_provider,
                        builder_api_key,
                    )
                    .await?;
                Ok(VddResult::Advisory(result))
            }
            VddMode::Blocking => {
                let result = self
                    .blocking_loop(
                        builder_response,
                        &builder_text,
                        original_request,
                        builder_provider,
                        builder_api_key,
                    )
                    .await?;
                Ok(VddResult::Blocking(result))
            }
        }
    }

    /// Advisory mode: single adversary pass, return findings for context injection.
    async fn advisory_review(
        &self,
        builder_text: &str,
        original_request: &ChatCompletionRequest,
        builder_provider: &str,
        builder_api_key: Option<&ApiKey>,
    ) -> Result<VddAdvisoryResult, VddError> {
        // Run static analysis
        let static_results = self.run_static_analysis().await;

        // Extract original task from request
        let original_task = extract_user_task(original_request);

        // Build and send adversary request
        let adversary_request = build_adversary_request(
            &self.config,
            &self.app_config,
            builder_text,
            &original_task,
            &static_results,
            1,
        );

        let (adversary_text, tokens_used) = send_to_adversary(
            &self.client,
            &self.config,
            &self.app_config,
            &adversary_request,
        )
        .await?;

        // Parse and triage findings (AI verifier uses builder's provider)
        let mut findings = parse_findings(&adversary_text, 1);
        let triage_ctx = TriageContext {
            client: &self.client,
            config: &self.config,
            app_config: &self.app_config,
            previous_fps: &[],
            builder_code: builder_text,
            builder_provider,
            builder_api_key,
        };
        triage_findings(&mut findings, &triage_ctx).await;

        // Build context injection string
        let context_injection = format_findings_for_injection(&findings, &static_results);

        let genuine_count = findings
            .iter()
            .filter(|f| f.status == FindingStatus::Genuine)
            .count();

        info!(
            total = findings.len(),
            genuine = genuine_count,
            "VDD advisory: review complete"
        );

        Ok(VddAdvisoryResult {
            findings,
            context_injection,
            static_analysis: static_results,
            tokens_used,
        })
    }

    /// Blocking mode: full adversarial loop until convergence.
    async fn blocking_loop(
        &self,
        initial_builder_response: &Value,
        initial_builder_text: &str,
        original_request: &ChatCompletionRequest,
        builder_provider: &str,
        builder_api_key: Option<&ApiKey>,
    ) -> Result<VddBlockingResult, VddError> {
        let mut session = VddSession::new(VddMode::Blocking);
        let mut tracker = ConfabulationTracker::new(
            f64::from(self.config.thresholds.false_positive_rate),
            self.config.thresholds.min_iterations,
        );

        let original_task = extract_user_task(original_request);
        let mut current_builder_text = initial_builder_text.to_string();
        let mut current_builder_response = initial_builder_response.clone();
        let mut previous_fps: Vec<String> = Vec::new();

        for iteration in 1..=self.config.thresholds.max_iterations {
            info!(
                iteration,
                max = self.config.thresholds.max_iterations,
                "VDD blocking: iteration"
            );

            let static_results = self.run_static_analysis().await;

            let iteration_ctx = IterationContext {
                builder_text: &current_builder_text,
                original_task: &original_task,
                static_results: &static_results,
                iteration,
                previous_fps: &previous_fps,
                builder_provider,
                builder_api_key,
            };
            let (genuine_count, fp_count, findings) =
                self.run_iteration(&iteration_ctx, &mut session).await?;

            tracker.record_iteration(genuine_count, fp_count);
            collect_false_positives(&findings, &mut previous_fps);

            info!(
                iteration,
                genuine = genuine_count,
                false_positives = fp_count,
                fp_rate = tracker.latest_rate().map_or_else(
                    || "n/a (no findings)".to_owned(),
                    |r| format!("{:.1}%", r * 100.0)
                ),
                "VDD blocking: iteration complete"
            );

            if self.check_convergence(&mut session, &tracker, iteration, genuine_count) {
                break;
            }

            // Step 5: If genuine findings, feed back to builder for revision.
            if genuine_count == 0 {
                debug!(
                    iteration,
                    min = self.config.thresholds.min_iterations,
                    "VDD blocking: no findings but below min iterations, continuing"
                );
                continue;
            }
            match self
                .revise_builder_response(
                    original_request,
                    &findings,
                    iteration,
                    builder_provider,
                    builder_api_key,
                    &mut session,
                )
                .await
            {
                Ok(Some((revised_text, revised_response))) => {
                    current_builder_text = revised_text;
                    current_builder_response = revised_response;
                }
                Ok(None) => break, // Revision recorded a failure and asked us to stop
                Err(e) => return Err(e),
            }
        }

        self.finalize_unconverged_session(&mut session);

        // Create Chainlink issues for genuine findings from all iterations
        let all_genuine: Vec<&Finding> = session
            .iterations
            .iter()
            .flat_map(|i| &i.adversary_review.findings)
            .filter(|f| f.status == FindingStatus::Genuine)
            .collect();

        let chainlink_issues = if all_genuine.is_empty() {
            Vec::new()
        } else {
            match create_chainlink_issues(&all_genuine).await {
                Ok(ids) => ids,
                Err(e) => {
                    warn!("VDD: Chainlink issue creation failed: {}", e);
                    Vec::new()
                }
            }
        };

        // Persist session if configured
        if self.config.tracking.persist {
            if let Err(e) = persist_session(&self.config.tracking.path, &session) {
                warn!("VDD: Session persistence failed: {}", e);
            }
        }

        Ok(VddBlockingResult {
            final_response: current_builder_response,
            session,
            chainlink_issues,
        })
    }

    /// Check the blocking-loop convergence criteria after an iteration is
    /// recorded. Returns `true` when the loop should stop, finalizing the
    /// session with the appropriate termination reason.
    fn check_convergence(
        &self,
        session: &mut VddSession,
        tracker: &ConfabulationTracker,
        iteration: u32,
        genuine_count: u32,
    ) -> bool {
        if tracker.should_terminate() {
            let rate_pct = tracker
                .latest_rate()
                .map_or_else(|| "n/a".to_owned(), |r| format!("{:.1}%", r * 100.0));
            session.finalize(
                true,
                &format!(
                    "Confabulation threshold reached: {} FP rate (threshold: {:.1}%)",
                    rate_pct,
                    self.config.thresholds.false_positive_rate * 100.0
                ),
            );
            info!(
                iterations = session.iterations.len(),
                fp_rate = rate_pct,
                "VDD blocking: converged (confabulation threshold)"
            );
            return true;
        }

        if genuine_count == 0 && iteration >= self.config.thresholds.min_iterations {
            session.finalize(true, "No genuine findings — clean pass");
            info!(
                iterations = session.iterations.len(),
                "VDD blocking: converged (clean pass)"
            );
            return true;
        }

        false
    }

    /// Send the genuine findings back to the builder for a revision pass.
    ///
    /// Returns:
    /// * `Ok(Some((text, json)))` — revision succeeded, caller should use these
    ///   as the new builder output and continue the loop.
    /// * `Ok(None)` — revision failed; the failure has been recorded on the
    ///   session and the caller should break out of the loop.
    /// * `Err(_)` — unrecoverable error.
    async fn revise_builder_response(
        &self,
        original_request: &ChatCompletionRequest,
        findings: &[Finding],
        iteration: u32,
        builder_provider: &str,
        builder_api_key: Option<&ApiKey>,
        session: &mut VddSession,
    ) -> Result<Option<(String, Value)>, VddError> {
        let genuine_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.status == FindingStatus::Genuine)
            .collect();

        let revision_request =
            build_revision_request(original_request, &genuine_findings, iteration);

        match send_to_builder(
            &self.client,
            &self.app_config,
            &revision_request,
            builder_provider,
            builder_api_key,
        )
        .await
        {
            Ok((revised_text, revised_response, builder_tokens)) => {
                session.builder_tokens.accumulate(&builder_tokens);
                Ok(Some((revised_text, revised_response)))
            }
            Err(e) => {
                warn!(
                    "VDD blocking: builder revision failed: {}, stopping loop",
                    e
                );
                session.finalize(false, &format!("Builder revision failed: {e}"));
                Ok(None)
            }
        }
    }

    /// Finalize the session when the loop exhausted `max_iterations`
    /// without hitting a convergence condition.
    fn finalize_unconverged_session(&self, session: &mut VddSession) {
        if session.termination_reason.is_some() {
            return;
        }
        session.finalize(
            false,
            &format!(
                "Max iterations ({}) reached without convergence",
                self.config.thresholds.max_iterations
            ),
        );
        warn!(
            max = self.config.thresholds.max_iterations,
            "VDD blocking: max iterations reached"
        );
    }

    /// Run a single iteration of the blocking loop: adversary request,
    /// parsing, triage, and recording into the session.
    ///
    /// Returns `(genuine_count, false_positive_count, findings)`.
    async fn run_iteration(
        &self,
        ctx: &IterationContext<'_>,
        session: &mut VddSession,
    ) -> Result<(u32, u32, Vec<Finding>), VddError> {
        // Step 1: Build and send adversary request (fresh context every time)
        let adversary_request = build_adversary_request(
            &self.config,
            &self.app_config,
            ctx.builder_text,
            ctx.original_task,
            ctx.static_results,
            ctx.iteration,
        );
        let (adversary_text, adversary_tokens) = send_to_adversary(
            &self.client,
            &self.config,
            &self.app_config,
            &adversary_request,
        )
        .await?;

        // Step 2: Parse and triage findings (including AI verification)
        let mut findings = parse_findings(&adversary_text, ctx.iteration);
        let triage_ctx = TriageContext {
            client: &self.client,
            config: &self.config,
            app_config: &self.app_config,
            previous_fps: ctx.previous_fps,
            builder_code: ctx.builder_text,
            builder_provider: ctx.builder_provider,
            builder_api_key: ctx.builder_api_key,
        };
        triage_findings(&mut findings, &triage_ctx).await;

        let genuine_count = u32::try_from(
            findings
                .iter()
                .filter(|f| f.status == FindingStatus::Genuine)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let fp_count = u32::try_from(
            findings
                .iter()
                .filter(|f| f.status == FindingStatus::FalsePositive)
                .count(),
        )
        .unwrap_or(u32::MAX);

        // Record iteration
        let review = AdversaryReview {
            iteration: ctx.iteration,
            findings: findings.clone(),
            raw_response: adversary_text,
            tokens_used: adversary_tokens,
            timestamp: Utc::now(),
        };

        let vdd_iteration = VddIteration {
            number: ctx.iteration,
            builder_response: ctx.builder_text.to_string(),
            static_analysis: ctx.static_results.to_vec(),
            adversary_review: review,
            genuine_count,
            false_positive_count: fp_count,
        };

        session.record_iteration(vdd_iteration);

        Ok((genuine_count, fp_count, findings))
    }

    /// Run configured static analysis commands.
    async fn run_static_analysis(&self) -> Vec<StaticAnalysisResult> {
        if !self.config.static_analysis.enabled {
            return Vec::new();
        }

        // Determine commands: use explicit config, or auto-detect if enabled
        let commands: Vec<String> = if !self.config.static_analysis.commands.is_empty() {
            self.config.static_analysis.commands.clone()
        } else if self.config.static_analysis.auto_detect {
            let detected = crate::guardrails::get_auto_detected_commands();
            if detected.is_empty() {
                debug!("VDD: No static analysis commands configured or auto-detected");
                return Vec::new();
            }
            detected
        } else {
            return Vec::new();
        };

        let mut results = Vec::new();
        let timeout = Duration::from_secs(self.config.static_analysis.timeout_seconds);

        for command in &commands {
            debug!(command = %command, "VDD: Running static analysis");

            let result = run_shell_command(command, timeout).await;
            info!(
                command = %command,
                passed = result.passed,
                exit_code = result.exit_code,
                "VDD: Static analysis complete"
            );
            results.push(result);
        }

        results
    }
}

/// Append false-positive descriptions from this iteration to the running
/// list used by the next iteration's duplicate-detection layer.
fn collect_false_positives(findings: &[Finding], previous_fps: &mut Vec<String>) {
    for f in findings {
        if f.status == FindingStatus::FalsePositive {
            previous_fps.push(f.description.clone());
        }
    }
}
