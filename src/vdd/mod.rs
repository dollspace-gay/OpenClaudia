//! Verification-Driven Development (VDD) Engine
//!
//! Implements the adversarial loop methodology where a Builder AI's output is reviewed
//! by a separate Adversary AI on a different provider with fresh context. The loop
//! continues until the adversary reaches the confabulation threshold (producing mostly
//! false positives), indicating exhaustion of genuine findings.
//!
//! Two modes:
//! - Advisory: Single adversary pass, findings injected into next turn context
//! - Blocking: Full adversarial loop until convergence, response held until clean
//!
//! Based on the VDD methodology: <https://github.com/dollspace-gay/Tesseract-Vault>

pub mod confabulation;
pub mod finding;
pub mod parsing;
pub mod review;
pub mod static_analysis;

// Re-exports for public API
pub use confabulation::ConfabulationTracker;
pub use finding::{Finding, FindingStatus, Severity};
pub use review::{AdversaryReview, VddIteration, VddSession};
pub use static_analysis::StaticAnalysisResult;

use chrono::Utc;
use reqwest::Client;
use serde_json::Value;
use std::fmt::Write;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{AppConfig, VddConfig, VddMode};
use crate::providers::get_adapter;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use crate::session::TokenUsage;

use confabulation::{is_common_false_positive, string_similarity};
use parsing::{
    extract_json_from_response, extract_response_text, extract_token_usage, parse_severity,
    try_parse_relaxed,
};
use review::AdversaryResponse;
use static_analysis::{run_chainlink_create, run_shell_command};

// ==========================================================================
// Constants
// ==========================================================================

/// System prompt for the verification agent. This is a separate step from
/// the adversary — it evaluates the adversary's findings against the actual
/// code to detect confabulated (hallucinated) findings.
const VERIFIER_SYSTEM_PROMPT: &str = r#"You are a verification agent in a Verification-Driven Development (VDD) loop. Your job is to evaluate whether adversary findings about code are GENUINE or CONFABULATED (hallucinated).

For each finding, you will see:
- The finding's severity, description, CWE, and the adversary's reasoning
- The actual code that was reviewed

Your task: determine whether each finding is real by checking the adversary's claims against the actual code. Adversary models frequently hallucinate issues that don't exist — they may reference lines that don't contain the claimed pattern, invent APIs or functions that aren't called, or describe vulnerabilities in code paths that aren't reachable.

Rules:
1. Check EVERY claim against the actual code. Does the line the adversary cited actually contain the pattern they describe?
2. If the adversary claims a function is called unsafely, verify the function exists and is actually called that way.
3. If the adversary claims user input reaches a dangerous sink, trace the data flow in the actual code.
4. Standard language/framework patterns are NOT vulnerabilities (e.g., mutex unwrap in Rust, test fixtures with hardcoded values).
5. Be precise. A finding is genuine ONLY if the described issue actually exists in the code as written.

You MUST respond with valid JSON in this exact format:
{
  "verdicts": [
    {
      "finding_id": "the-finding-id",
      "verdict": "genuine",
      "reasoning": "The SQL query on line 45 does concatenate user input directly, as the adversary described."
    },
    {
      "finding_id": "another-finding-id",
      "verdict": "confabulated",
      "reasoning": "The adversary claims line 23 uses eval(), but line 23 is actually a comment. The function described does not exist in this code."
    }
  ]
}

The verdict field MUST be exactly "genuine" or "confabulated". No other values."#;

/// System prompt for the adversary model. Establishes the adversarial role
/// with structured JSON output format.
const ADVERSARY_SYSTEM_PROMPT: &str = r#"You are an adversarial code reviewer operating in a Verification-Driven Development (VDD) loop. Your role is to find genuine bugs, security vulnerabilities, logic errors, and correctness issues in the code changes presented to you.

Rules:
1. Be hyper-critical. Assume the code is wrong until proven correct.
2. Classify each finding by severity: CRITICAL, HIGH, MEDIUM, LOW, or INFO.
3. Include CWE classification where applicable (e.g., CWE-89 for SQL injection).
4. Cite specific line numbers and code snippets when possible.
5. Do NOT critique style, formatting, or naming conventions unless they cause bugs.
6. Do NOT report issues that are standard patterns for the language/framework in use.
7. If you find no genuine issues, respond with exactly: {"findings": [], "assessment": "NO_FINDINGS"}

You MUST respond with valid JSON in this exact format:
{
  "findings": [
    {
      "severity": "HIGH",
      "cwe": "CWE-89",
      "description": "SQL injection via string concatenation in query builder",
      "file": "src/db.rs",
      "lines": [45, 52],
      "reasoning": "The user input from the request body is interpolated directly into the SQL query string without parameterization, allowing an attacker to inject arbitrary SQL."
    }
  ],
  "assessment": "FINDINGS_PRESENT"
}

When static analysis results are provided, use them as additional signal but form your own independent assessment. Do not merely repeat what the static analyzer found."#;

// ==========================================================================
// Error Types
// ==========================================================================

#[derive(Error, Debug)]
pub enum VddError {
    #[error("Adversary provider request failed: {0}")]
    AdversaryRequestFailed(String),

    #[error("Builder revision request failed: {0}")]
    BuilderRevisionFailed(String),

    #[error("Failed to parse adversary response as findings: {0}")]
    ParseError(String),

    #[error("Static analysis command failed: {command} (timeout: {timeout}s)")]
    StaticAnalysisTimeout { command: String, timeout: u64 },

    #[error("Chainlink issue creation failed: {0}")]
    ChainlinkError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("HTTP client error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON serialization error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

// ==========================================================================
// VDD Results
// ==========================================================================

/// Top-level result from VDD processing
pub enum VddResult {
    /// Advisory mode: single pass, findings for context injection
    Advisory(VddAdvisoryResult),
    /// Blocking mode: full loop, revised response
    Blocking(VddBlockingResult),
    /// VDD was skipped (disabled, not applicable, etc.)
    Skipped(String),
}

/// Advisory mode result
pub struct VddAdvisoryResult {
    pub findings: Vec<Finding>,
    pub context_injection: String,
    pub static_analysis: Vec<StaticAnalysisResult>,
    pub tokens_used: TokenUsage,
}

/// Blocking mode result
pub struct VddBlockingResult {
    pub final_response: Value,
    pub session: VddSession,
    pub chainlink_issues: Vec<String>,
}

// ==========================================================================
// VDD Engine
// ==========================================================================

/// The core VDD engine that orchestrates adversarial review loops.
pub struct VddEngine {
    config: VddConfig,
    app_config: AppConfig,
    client: Client,
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
        builder_api_key: Option<&crate::providers::ApiKey>,
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
        let adversary_request =
            self.build_adversary_request(builder_text, user_task, &static_results, 1);

        let (adversary_text, tokens_used) = self.send_to_adversary(&adversary_request).await?;

        // Parse and triage findings (AI verifier uses builder's provider)
        let mut findings = self.parse_findings(&adversary_text, 1);
        self.triage_findings(
            &mut findings,
            &[],
            builder_text,
            builder_provider,
            builder_api_key,
        )
        .await;

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
        builder_api_key: Option<&crate::providers::ApiKey>,
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
        builder_api_key: Option<&crate::providers::ApiKey>,
    ) -> Result<VddAdvisoryResult, VddError> {
        // Run static analysis
        let static_results = self.run_static_analysis().await;

        // Extract original task from request
        let original_task = extract_user_task(original_request);

        // Build and send adversary request
        let adversary_request =
            self.build_adversary_request(builder_text, &original_task, &static_results, 1);

        let (adversary_text, tokens_used) = self.send_to_adversary(&adversary_request).await?;

        // Parse and triage findings (AI verifier uses builder's provider)
        let mut findings = self.parse_findings(&adversary_text, 1);
        self.triage_findings(
            &mut findings,
            &[],
            builder_text,
            builder_provider,
            builder_api_key,
        )
        .await;

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
    #[allow(clippy::too_many_lines)]
    async fn blocking_loop(
        &self,
        initial_builder_response: &Value,
        initial_builder_text: &str,
        original_request: &ChatCompletionRequest,
        builder_provider: &str,
        builder_api_key: Option<&crate::providers::ApiKey>,
    ) -> Result<VddBlockingResult, VddError> {
        let mut session = VddSession::new(VddMode::Blocking);
        let mut tracker = ConfabulationTracker::new(
            self.config.thresholds.false_positive_rate,
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

            // Step 1: Run static analysis
            let static_results = self.run_static_analysis().await;

            // Step 2: Build and send adversary request (fresh context every time)
            let adversary_request = self.build_adversary_request(
                &current_builder_text,
                &original_task,
                &static_results,
                iteration,
            );
            let (adversary_text, adversary_tokens) =
                self.send_to_adversary(&adversary_request).await?;

            // Step 3: Parse and triage findings (including AI verification)
            let mut findings = self.parse_findings(&adversary_text, iteration);
            self.triage_findings(
                &mut findings,
                &previous_fps,
                &current_builder_text,
                builder_provider,
                builder_api_key,
            )
            .await;

            #[allow(clippy::cast_possible_truncation)]
            let genuine_count = findings
                .iter()
                .filter(|f| f.status == FindingStatus::Genuine)
                .count() as u32;
            #[allow(clippy::cast_possible_truncation)]
            let fp_count = findings
                .iter()
                .filter(|f| f.status == FindingStatus::FalsePositive)
                .count() as u32;

            // Record iteration
            let review = AdversaryReview {
                iteration,
                findings: findings.clone(),
                raw_response: adversary_text.clone(),
                tokens_used: adversary_tokens,
                timestamp: Utc::now(),
            };

            let vdd_iteration = VddIteration {
                number: iteration,
                builder_response: current_builder_text.clone(),
                static_analysis: static_results,
                adversary_review: review,
                genuine_count,
                false_positive_count: fp_count,
            };

            session.record_iteration(vdd_iteration);
            tracker.record_iteration(genuine_count, fp_count);

            // Collect FP descriptions to avoid re-reporting
            for f in &findings {
                if f.status == FindingStatus::FalsePositive {
                    previous_fps.push(f.description.clone());
                }
            }

            info!(
                iteration,
                genuine = genuine_count,
                false_positives = fp_count,
                fp_rate = format!("{:.1}%", tracker.latest_rate() * 100.0),
                "VDD blocking: iteration complete"
            );

            // Step 4: Check convergence
            if tracker.should_terminate() {
                session.finalize(
                    true,
                    &format!(
                        "Confabulation threshold reached: {:.1}% FP rate (threshold: {:.1}%)",
                        tracker.latest_rate() * 100.0,
                        self.config.thresholds.false_positive_rate * 100.0
                    ),
                );
                info!(
                    iterations = session.iterations.len(),
                    fp_rate = format!("{:.1}%", tracker.latest_rate() * 100.0),
                    "VDD blocking: converged (confabulation threshold)"
                );
                break;
            }

            // No genuine findings and past minimum iterations = clean pass
            if genuine_count == 0 && iteration >= self.config.thresholds.min_iterations {
                session.finalize(true, "No genuine findings — clean pass");
                info!(
                    iterations = session.iterations.len(),
                    "VDD blocking: converged (clean pass)"
                );
                break;
            }

            // Step 5: If genuine findings, feed back to builder for revision
            if genuine_count > 0 {
                let genuine_findings: Vec<&Finding> = findings
                    .iter()
                    .filter(|f| f.status == FindingStatus::Genuine)
                    .collect();

                let revision_request =
                    self.build_revision_request(original_request, &genuine_findings, iteration);

                match self
                    .send_to_builder(&revision_request, builder_provider, builder_api_key)
                    .await
                {
                    Ok((revised_text, revised_response, builder_tokens)) => {
                        current_builder_text = revised_text;
                        current_builder_response = revised_response;
                        session.builder_tokens.accumulate(&builder_tokens);
                    }
                    Err(e) => {
                        warn!(
                            "VDD blocking: builder revision failed: {}, stopping loop",
                            e
                        );
                        session.finalize(false, &format!("Builder revision failed: {e}"));
                        break;
                    }
                }
            } else {
                // No genuine findings but haven't hit min_iterations yet
                // Continue loop to build confidence
                debug!(
                    iteration,
                    min = self.config.thresholds.min_iterations,
                    "VDD blocking: no findings but below min iterations, continuing"
                );
            }
        }

        // If we exhausted max iterations without converging
        if session.termination_reason.is_none() {
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
            match self.create_chainlink_issues(&all_genuine).await {
                Ok(ids) => ids,
                Err(e) => {
                    warn!("VDD: Chainlink issue creation failed: {}", e);
                    Vec::new()
                }
            }
        };

        // Persist session if configured
        if self.config.tracking.persist {
            if let Err(e) = self.persist_session(&session) {
                warn!("VDD: Session persistence failed: {}", e);
            }
        }

        Ok(VddBlockingResult {
            final_response: current_builder_response,
            session,
            chainlink_issues,
        })
    }

    /// Build a fresh adversary request with complete context isolation.
    /// The adversary sees ONLY: its system prompt, the builder's output,
    /// the original task description, and static analysis results.
    fn build_adversary_request(
        &self,
        builder_output: &str,
        original_task: &str,
        static_analysis_results: &[StaticAnalysisResult],
        iteration: u32,
    ) -> ChatCompletionRequest {
        let mut user_content = format!(
            "## Original Task\n{original_task}\n\n## Builder Output (Iteration {iteration})\n{builder_output}"
        );

        // Append static analysis results if any
        if !static_analysis_results.is_empty() {
            user_content.push_str("\n\n## Static Analysis Results\n");
            for result in static_analysis_results {
                let _ = write!(
                    user_content,
                    "\n### `{}`\n**Exit code:** {} ({})\n",
                    result.command,
                    result.exit_code,
                    if result.passed { "PASSED" } else { "FAILED" }
                );
                if !result.stdout.is_empty() {
                    let truncated = truncate_output(&result.stdout, 2000);
                    let _ = write!(user_content, "**stdout:**\n```\n{truncated}\n```\n");
                }
                if !result.stderr.is_empty() {
                    let truncated = truncate_output(&result.stderr, 2000);
                    let _ = write!(user_content, "**stderr:**\n```\n{truncated}\n```\n");
                }
            }
        }

        // Determine model for adversary
        let model = self.config.adversary.model.clone().unwrap_or_else(|| {
            self.app_config
                .providers
                .get(&self.config.adversary.provider)
                .and_then(|p| p.model.clone())
                .unwrap_or_else(|| "default".to_string())
        });

        ChatCompletionRequest {
            model,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text(ADVERSARY_SYSTEM_PROMPT.to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text(user_content),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: Some(self.config.adversary.temperature),
            max_tokens: Some(self.config.adversary.max_tokens),
            stream: Some(false), // Always non-streaming for VDD
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        }
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

    /// Parse adversary response text into structured findings.
    #[allow(clippy::unused_self)]
    fn parse_findings(&self, adversary_response: &str, iteration: u32) -> Vec<Finding> {
        // Try to parse as JSON first
        let parsed: Option<AdversaryResponse> = serde_json::from_str(adversary_response)
            .ok()
            .or_else(|| {
                // Try to extract JSON from markdown code blocks
                extract_json_from_response(adversary_response)
                    .and_then(|json| serde_json::from_str(&json).ok())
            })
            .or_else(|| {
                // Try relaxed parsing for natural language responses
                try_parse_relaxed(adversary_response)
            });

        let raw_findings = if let Some(response) = parsed {
            if response.assessment.as_deref() == Some("NO_FINDINGS") {
                info!("VDD: Adversary reported no findings");
                return Vec::new();
            }
            if let Some(findings) = response.findings {
                findings
            } else {
                warn!("VDD: Adversary response has no 'findings' field or it is not an array");
                return Vec::new();
            }
        } else {
            warn!("VDD: Could not parse adversary response as JSON, treating as no findings");
            info!(
                "VDD: Unparseable response preview: {}",
                truncate_output(adversary_response, 500)
            );
            return Vec::new();
        };

        raw_findings
            .into_iter()
            .map(|raw| {
                let severity = parse_severity(raw.severity.as_deref().unwrap_or("INFO"));
                let line_range = raw.lines.and_then(|lines| {
                    if lines.len() >= 2 {
                        Some((lines[0], lines[1]))
                    } else if lines.len() == 1 {
                        Some((lines[0], lines[0]))
                    } else {
                        None
                    }
                });

                Finding {
                    id: Uuid::new_v4().to_string(),
                    severity,
                    cwe: raw.cwe,
                    description: raw
                        .description
                        .unwrap_or_else(|| "No description".to_string()),
                    file_path: raw.file,
                    line_range,
                    status: FindingStatus::Genuine, // Default; triage will reclassify
                    adversary_reasoning: raw.reasoning.unwrap_or_default(),
                    iteration,
                }
            })
            .collect()
    }

    /// Triage findings using three layers:
    /// 1. Duplicate detection (string similarity against previous FPs)
    /// 2. Common false positive patterns (hardcoded Rust patterns)
    /// 3. AI-powered verification agent (sends findings + code to the
    ///    **builder's** provider to check each claim against actual code)
    ///
    /// The verifier deliberately uses the builder's provider (not the
    /// adversary's) to avoid correlated confabulation — if the adversary
    /// hallucinates, asking the same model to verify would produce the
    /// same hallucination.
    async fn triage_findings(
        &self,
        findings: &mut [Finding],
        previous_fps: &[String],
        builder_code: &str,
        builder_provider: &str,
        builder_api_key: Option<&crate::providers::ApiKey>,
    ) {
        // Layer 1: Duplicate detection — cheap, catches re-reported FPs
        for finding in findings.iter_mut() {
            let desc_lower = finding.description.to_lowercase();
            for fp_desc in previous_fps {
                if string_similarity(&desc_lower, &fp_desc.to_lowercase()) > 0.7 {
                    finding.status = FindingStatus::FalsePositive;
                    break;
                }
            }
        }

        // Layer 2: Common false positive patterns — cheap, catches known patterns
        for finding in findings.iter_mut() {
            if finding.status == FindingStatus::Genuine {
                let desc = &finding.description.to_lowercase();
                if is_common_false_positive(desc, &finding.adversary_reasoning.to_lowercase()) {
                    finding.status = FindingStatus::FalsePositive;
                }
            }
        }

        // Layer 3: AI-powered verification — expensive but catches novel confabulations.
        // Only verify findings that survived layers 1 and 2.
        let surviving_genuine: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.status == FindingStatus::Genuine)
            .collect();

        if surviving_genuine.is_empty() {
            return;
        }

        match self
            .verify_findings(
                &surviving_genuine,
                builder_code,
                builder_provider,
                builder_api_key,
            )
            .await
        {
            Ok(verdicts) => {
                for finding in findings.iter_mut() {
                    if finding.status != FindingStatus::Genuine {
                        continue; // Already classified by layers 1-2
                    }
                    if let Some(verdict) = verdicts.get(&finding.id) {
                        if verdict == "confabulated" {
                            eprintln!(
                                "\x1b[33m⚠ VDD verifier: adversary finding is confabulated — \"{}\"\x1b[0m",
                                truncate_output(&finding.description, 80)
                            );
                            finding.status = FindingStatus::FalsePositive;
                        }
                    }
                }
            }
            Err(e) => {
                // Tell the user verification couldn't run — they're operating
                // with weaker confabulation detection (pattern-matching only).
                eprintln!("\x1b[33m⚠ VDD verification agent failed: {e}\x1b[0m");
                eprintln!(
                    "\x1b[33m  Triage is using pattern-matching only — novel confabulations may not be caught.\x1b[0m"
                );
                warn!("VDD verifier request failed: {e}");
            }
        }
    }

    /// Send findings to a verification agent that checks each adversary claim
    /// against the actual code.  Uses the **builder's** provider (not the
    /// adversary's) to get an independent second opinion.
    ///
    /// Returns a map of finding_id → "genuine" | "confabulated".
    async fn verify_findings(
        &self,
        findings: &[&Finding],
        builder_code: &str,
        builder_provider: &str,
        builder_api_key: Option<&crate::providers::ApiKey>,
    ) -> Result<std::collections::HashMap<String, String>, VddError> {
        if findings.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        eprintln!(
            "\x1b[36m🔍 VDD verifier: checking {} finding(s) against code via {}\x1b[0m",
            findings.len(),
            builder_provider
        );

        // Build the verification request content
        let mut user_content = format!(
            "## Code Under Review\n```\n{}\n```\n\n## Adversary Findings to Verify\n",
            truncate_output(builder_code, 12000)
        );

        for (i, finding) in findings.iter().enumerate() {
            let _ = write!(
                user_content,
                "\n### Finding {} (ID: {})\n\
                 - **Severity:** {:?}\n\
                 - **CWE:** {}\n\
                 - **File:** {}\n\
                 - **Lines:** {}\n\
                 - **Description:** {}\n\
                 - **Adversary reasoning:** {}\n",
                i + 1,
                finding.id,
                finding.severity,
                finding.cwe.as_deref().unwrap_or("none"),
                finding.file_path.as_deref().unwrap_or("unknown"),
                finding
                    .line_range
                    .map(|(a, b)| format!("{a}-{b}"))
                    .unwrap_or_else(|| "unknown".to_string()),
                finding.description,
                finding.adversary_reasoning,
            );
        }

        // Use the builder's provider model for verification — independent
        // from the adversary to avoid correlated confabulation.
        let model = self
            .app_config
            .providers
            .get(builder_provider)
            .and_then(|p| p.model.clone())
            .unwrap_or_else(|| "default".to_string());

        let request = ChatCompletionRequest {
            model,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text(VERIFIER_SYSTEM_PROMPT.to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text(user_content),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: Some(0.0), // Maximally deterministic for verification
            max_tokens: Some(self.config.adversary.max_tokens),
            stream: Some(false),
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };

        // Route through the builder's provider, not the adversary's
        let (response_text, tokens) = self
            .send_to_builder_for_verification(&request, builder_provider, builder_api_key)
            .await?;

        eprintln!(
            "\x1b[36m  Verifier used {} input + {} output tokens\x1b[0m",
            tokens.input_tokens, tokens.output_tokens
        );

        // Parse verdicts from the response
        self.parse_verification_verdicts(&response_text)
    }

    /// Send a verification request through the builder's provider.
    /// Reuses the same HTTP plumbing as `send_to_builder` but with a
    /// simpler interface (no revision response needed).
    async fn send_to_builder_for_verification(
        &self,
        request: &ChatCompletionRequest,
        provider_name: &str,
        api_key: Option<&crate::providers::ApiKey>,
    ) -> Result<(String, TokenUsage), VddError> {
        let provider_config = self
            .app_config
            .providers
            .get(provider_name)
            .ok_or_else(|| {
                VddError::ConfigError(format!(
                    "Builder provider '{provider_name}' not configured — \
                     cannot run verification agent"
                ))
            })?;

        let adapter = get_adapter(provider_name);
        let transformed = adapter
            .transform_request(request)
            .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier transform: {e}")))?;

        let headers = api_key
            .map(|k| adapter.get_headers(k))
            .unwrap_or_default();
        let endpoint = adapter.chat_endpoint(&request.model);

        let response = forward_request(
            &self.client,
            provider_config,
            provider_name,
            &request.model,
            &endpoint,
            &transformed,
            headers,
        )
        .await
        .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier request: {e}")))?;

        let response_json: Value = response
            .json()
            .await
            .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier response: {e}")))?;

        let text = extract_response_text(&response_json);
        let tokens = extract_token_usage(&response_json);

        Ok((text, tokens))
    }

    /// Parse the verification agent's response into a finding_id → verdict map.
    fn parse_verification_verdicts(
        &self,
        response: &str,
    ) -> Result<std::collections::HashMap<String, String>, VddError> {
        let mut verdicts = std::collections::HashMap::new();

        // Try to extract JSON from the response
        let json_str = extract_json_from_response(response).unwrap_or(response.to_string());

        if let Ok(value) = serde_json::from_str::<Value>(&json_str) {
            if let Some(arr) = value.get("verdicts").and_then(|v| v.as_array()) {
                for item in arr {
                    if let (Some(id), Some(verdict)) = (
                        item.get("finding_id").and_then(|v| v.as_str()),
                        item.get("verdict").and_then(|v| v.as_str()),
                    ) {
                        let normalized = verdict.to_lowercase();
                        if normalized == "genuine" || normalized == "confabulated" {
                            verdicts.insert(id.to_string(), normalized);
                        }
                    }
                }
            }
        }

        if verdicts.is_empty() && !response.is_empty() {
            // Fallback: try to extract verdicts from natural language
            // If the verifier says something like "all findings are confabulated"
            let lower = response.to_lowercase();
            if lower.contains("all") && lower.contains("confabulated") {
                debug!("VDD verifier: bulk confabulation detected in natural language response");
                // Can't map to specific IDs without parsing, so return empty
                // and let the convergence math handle it next iteration
            }
            warn!(
                "VDD verifier: could not parse structured verdicts from response ({} chars)",
                response.len()
            );
        }

        Ok(verdicts)
    }

    /// Create Chainlink issues for genuine findings.
    async fn create_chainlink_issues(
        &self,
        findings: &[&Finding],
    ) -> Result<Vec<String>, VddError> {
        let mut issue_ids = Vec::new();

        for finding in findings {
            let label = if finding.cwe.is_some() {
                "security"
            } else {
                "bug"
            };

            let title = format!(
                "Fix {} VDD finding: {}",
                finding.severity,
                truncate_output(&finding.description, 60)
            );

            let comment = format!(
                "**Severity:** {}\n**CWE:** {}\n**File:** {}\n**Lines:** {}\n\n**Description:**\n{}\n\n**Reasoning:**\n{}",
                finding.severity,
                finding.cwe.as_deref().unwrap_or("N/A"),
                finding.file_path.as_deref().unwrap_or("N/A"),
                finding.line_range.map_or_else(|| "N/A".to_string(), |(s, e)| format!("{s}-{e}")),
                finding.description,
                finding.adversary_reasoning,
            );

            match run_chainlink_create(&title, label, &comment).await {
                Ok(id) => {
                    info!(issue_id = %id, severity = %finding.severity, "VDD: Created Chainlink issue");
                    issue_ids.push(id);
                }
                Err(e) => {
                    warn!(error = %e, "VDD: Failed to create Chainlink issue");
                }
            }
        }

        Ok(issue_ids)
    }

    /// Build a revision request to send back to the builder with genuine findings.
    #[allow(clippy::unused_self)]
    fn build_revision_request(
        &self,
        original_request: &ChatCompletionRequest,
        genuine_findings: &[&Finding],
        iteration: u32,
    ) -> ChatCompletionRequest {
        let mut findings_text = String::from(
            "The following genuine issues were found by adversarial review. \
             Fix ALL of them in your revised response:\n\n",
        );

        for (i, finding) in genuine_findings.iter().enumerate() {
            let _ =
                write!(findings_text,
                "### Finding {} [{}] {}\n**File:** {}\n**Lines:** {}\n{}\n\n**Reasoning:** {}\n\n",
                i + 1,
                finding.severity,
                finding.cwe.as_deref().unwrap_or(""),
                finding.file_path.as_deref().unwrap_or("N/A"),
                finding
                    .line_range
                    .map_or_else(|| "N/A".to_string(), |(s, e)| format!("{s}-{e}")),
                finding.description,
                finding.adversary_reasoning,
            );
        }

        // Clone original messages and append the revision request
        let mut messages = original_request.messages.clone();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text(format!(
                "<vdd-revision iteration=\"{iteration}\">\n{findings_text}</vdd-revision>"
            )),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });

        ChatCompletionRequest {
            model: original_request.model.clone(),
            messages,
            temperature: original_request.temperature,
            max_tokens: original_request.max_tokens,
            stream: Some(false), // Always non-streaming for VDD revisions
            tools: original_request.tools.clone(),
            tool_choice: original_request.tool_choice.clone(),
            extra: original_request.extra.clone(),
        }
    }

    /// Send a request to the adversary provider. Returns (`response_text`, `token_usage`).
    async fn send_to_adversary(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<(String, TokenUsage), VddError> {
        let provider_config = self
            .app_config
            .providers
            .get(&self.config.adversary.provider)
            .ok_or_else(|| {
                VddError::ConfigError(format!(
                    "Adversary provider '{}' not configured in providers section",
                    self.config.adversary.provider
                ))
            })?;

        let api_key = self
            .config
            .adversary
            .api_key
            .as_ref()
            .or(provider_config.api_key.as_ref())
            .ok_or_else(|| {
                VddError::ConfigError(format!(
                    "No API key for adversary provider '{}'",
                    self.config.adversary.provider
                ))
            })?;

        let adapter = get_adapter(&self.config.adversary.provider);
        let transformed = adapter
            .transform_request(request)
            .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

        let headers = adapter.get_headers(api_key);
        let endpoint = adapter.chat_endpoint(&request.model);

        // Per-request timeout — guards against a hung adversary blocking
        // the whole VDD loop. See crosslink #496.
        let timeout_secs = self.config.adversary.request_timeout_seconds;
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let response = tokio::time::timeout(
            timeout,
            forward_request(
                &self.client,
                provider_config,
                &self.config.adversary.provider,
                &request.model,
                &endpoint,
                &transformed,
                headers,
            ),
        )
        .await
        .map_err(|_| {
            VddError::AdversaryRequestFailed(format!(
                "adversary request timed out after {timeout_secs}s"
            ))
        })?
        .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

        // Same timeout wraps the body-read to prevent a slow-drip
        // payload from exceeding the total budget.
        let response_json: Value = tokio::time::timeout(timeout, response.json())
            .await
            .map_err(|_| {
                VddError::AdversaryRequestFailed(format!(
                    "adversary response body read timed out after {timeout_secs}s"
                ))
            })?
            .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

        let text = extract_response_text(&response_json);
        let tokens = extract_token_usage(&response_json);

        // Always log at INFO level for debugging, truncated
        info!(
            response_length = text.len(),
            "VDD: Received adversary response ({} chars)",
            text.len()
        );

        if self.config.tracking.log_adversary_responses {
            // Log first 1000 chars to see what we're getting
            info!(
                "VDD: Adversary response preview: {}",
                truncate_output(&text, 1000)
            );
        }

        Ok((text, tokens))
    }

    /// Send a revision request back to the builder provider.
    async fn send_to_builder(
        &self,
        request: &ChatCompletionRequest,
        provider_name: &str,
        api_key: Option<&crate::providers::ApiKey>,
    ) -> Result<(String, Value, TokenUsage), VddError> {
        let provider_config = self
            .app_config
            .providers
            .get(provider_name)
            .ok_or_else(|| {
                VddError::BuilderRevisionFailed(format!(
                    "Builder provider '{provider_name}' not configured"
                ))
            })?;

        let adapter = get_adapter(provider_name);
        let transformed = adapter
            .transform_request(request)
            .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

        let headers = api_key
            .map(|k| adapter.get_headers(k))
            .unwrap_or_default();
        let endpoint = adapter.chat_endpoint(&request.model);

        let response = forward_request(
            &self.client,
            provider_config,
            provider_name,
            &request.model,
            &endpoint,
            &transformed,
            headers,
        )
        .await
        .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

        let response_json: Value = response
            .json()
            .await
            .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

        let text = extract_response_text(&response_json);
        let tokens = extract_token_usage(&response_json);

        Ok((text, response_json, tokens))
    }

    /// Persist VDD session to disk.
    fn persist_session(&self, session: &VddSession) -> Result<(), VddError> {
        let path = &self.config.tracking.path;
        std::fs::create_dir_all(path)?;

        let filename = format!("vdd-session-{}.json", session.id);
        let filepath = path.join(filename);

        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(&filepath, json)?;

        info!(path = %filepath.display(), "VDD: Session persisted");
        Ok(())
    }
}

// ==========================================================================
// Helper Functions
// ==========================================================================

/// Forward a request to a provider and return the raw reqwest response.
async fn forward_request(
    client: &Client,
    provider: &crate::config::ProviderConfig,
    provider_name: &str,
    model: &str,
    endpoint: &str,
    body: &Value,
    headers: Vec<(String, String)>,
) -> Result<reqwest::Response, reqwest::Error> {
    let base_url = provider
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');

    // Google/Gemini requires model name in the URL path
    let url = if provider_name == "google" {
        format!("{base_url}/v1beta/models/{model}:generateContent")
    } else {
        format!("{base_url}{endpoint}")
    };

    // Validate the constructed URL before sending the request
    if let Err(e) = reqwest::Url::parse(&url) {
        warn!("VDD: Invalid provider URL '{}': {}", url, e);
    }

    debug!("VDD: Sending request to {}", url);

    let mut req = client.post(&url).json(body);
    for (key, value) in headers {
        req = req.header(key.as_str(), value.as_str());
    }
    for (key, value) in &provider.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    req.send().await
}

/// Extract the user's task/request from the original conversation.
fn extract_user_task(request: &ChatCompletionRequest) -> String {
    // Find the last user message (the actual task)
    for message in request.messages.iter().rev() {
        if message.role == "user" {
            match &message.content {
                MessageContent::Text(text) => return text.clone(),
                MessageContent::Parts(parts) => {
                    let texts: Vec<&str> = parts.iter().filter_map(|p| p.text.as_deref()).collect();
                    return texts.join("\n");
                }
            }
        }
    }
    "No task description available".to_string()
}

/// Truncate output to a maximum length with an indicator.
fn truncate_output(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        format!(
            "{}... [truncated, {} total chars]",
            &text[..max_len],
            text.len()
        )
    }
}

/// Format findings for injection into the next turn's context (advisory mode).
fn format_findings_for_injection(
    findings: &[Finding],
    static_analysis: &[StaticAnalysisResult],
) -> String {
    let genuine: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.status == FindingStatus::Genuine)
        .collect();

    if genuine.is_empty() && static_analysis.iter().all(|r| r.passed) {
        return String::new(); // No context needed
    }

    let mut output = String::from("<vdd-advisory>\n");

    if !genuine.is_empty() {
        output.push_str(
            "Adversarial review identified the following issues in your previous response:\n\n",
        );
        for (i, finding) in genuine.iter().enumerate() {
            let _ = writeln!(
                output,
                "{}. [{}] {}{}: {}",
                i + 1,
                finding.severity,
                finding
                    .cwe
                    .as_deref()
                    .map(|c| format!("{c} "))
                    .unwrap_or_default(),
                finding
                    .file_path
                    .as_deref()
                    .map(|f| format!(" in {f}"))
                    .unwrap_or_default(),
                finding.description
            );
        }
        output.push_str("\nAddress these issues in your next response.\n");
    }

    let failed_analysis: Vec<&StaticAnalysisResult> =
        static_analysis.iter().filter(|r| !r.passed).collect();
    if !failed_analysis.is_empty() {
        output.push_str("\nStatic analysis failures:\n");
        for result in failed_analysis {
            let _ = writeln!(
                output,
                "- `{}` (exit code {})",
                result.command, result.exit_code
            );
        }
    }

    output.push_str("</vdd-advisory>");
    output
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VddTracking;

    #[test]
    fn test_truncate_output_short() {
        assert_eq!(truncate_output("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_output_long() {
        let result = truncate_output("hello world this is long", 10);
        assert!(result.starts_with("hello worl"));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_format_findings_for_injection_empty() {
        let findings: Vec<Finding> = Vec::new();
        let analysis: Vec<StaticAnalysisResult> = Vec::new();
        assert_eq!(format_findings_for_injection(&findings, &analysis), "");
    }

    #[test]
    fn test_format_findings_for_injection_with_genuine() {
        let findings = vec![Finding {
            id: "test-id".to_string(),
            severity: Severity::High,
            cwe: Some("CWE-89".to_string()),
            description: "SQL injection".to_string(),
            file_path: Some("src/db.rs".to_string()),
            line_range: Some((10, 20)),
            status: FindingStatus::Genuine,
            adversary_reasoning: "User input concatenated".to_string(),
            iteration: 1,
        }];
        let result = format_findings_for_injection(&findings, &[]);
        assert!(result.contains("<vdd-advisory>"));
        assert!(result.contains("CWE-89"));
        assert!(result.contains("SQL injection"));
        assert!(result.contains("</vdd-advisory>"));
    }

    #[test]
    fn test_format_findings_skips_false_positives() {
        let findings = vec![Finding {
            id: "test-id".to_string(),
            severity: Severity::Low,
            cwe: None,
            description: "Not a real issue".to_string(),
            file_path: None,
            line_range: None,
            status: FindingStatus::FalsePositive,
            adversary_reasoning: "".to_string(),
            iteration: 1,
        }];
        let result = format_findings_for_injection(&findings, &[]);
        assert_eq!(result, ""); // FP-only = no injection
    }

    #[test]
    fn test_parse_findings_valid_json() {
        let config = VddConfig {
            enabled: true,
            tracking: VddTracking {
                log_adversary_responses: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let engine = VddEngine {
            config,
            app_config: AppConfig {
                proxy: crate::config::ProxyConfig::default(),
                providers: std::collections::HashMap::new(),
                hooks: crate::config::HooksConfig::default(),
                session: crate::config::SessionConfig::default(),
                keybindings: crate::config::KeybindingsConfig::default(),
                vdd: VddConfig::default(),
                guardrails: crate::config::GuardrailsConfig::default(),
                permissions: crate::config::PermissionsConfig::default(),
                managed_settings_path: None,
            },
            client: Client::new(),
        };

        let response = r#"{"findings": [{"severity": "HIGH", "cwe": "CWE-89", "description": "SQL injection", "file": "src/db.rs", "lines": [10, 20], "reasoning": "User input concatenated"}], "assessment": "FINDINGS_PRESENT"}"#;
        let findings = engine.parse_findings(response, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].cwe, Some("CWE-89".to_string()));
        assert_eq!(findings[0].description, "SQL injection");
        assert_eq!(findings[0].file_path, Some("src/db.rs".to_string()));
        assert_eq!(findings[0].line_range, Some((10, 20)));
    }

    #[test]
    fn test_parse_findings_no_findings() {
        let config = VddConfig {
            enabled: true,
            tracking: VddTracking {
                log_adversary_responses: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let engine = VddEngine {
            config,
            app_config: AppConfig {
                proxy: crate::config::ProxyConfig::default(),
                providers: std::collections::HashMap::new(),
                hooks: crate::config::HooksConfig::default(),
                session: crate::config::SessionConfig::default(),
                keybindings: crate::config::KeybindingsConfig::default(),
                vdd: VddConfig::default(),
                guardrails: crate::config::GuardrailsConfig::default(),
                permissions: crate::config::PermissionsConfig::default(),
                managed_settings_path: None,
            },
            client: Client::new(),
        };

        let response = r#"{"findings": [], "assessment": "NO_FINDINGS"}"#;
        let findings = engine.parse_findings(response, 1);
        assert!(findings.is_empty());
    }

    /// Test that Layer 1 (duplicate detection) catches re-reported findings
    /// before the AI verification layer is reached.  The AI layer requires
    /// an API call, but duplicates are caught cheaply via string similarity.
    #[tokio::test]
    async fn test_triage_marks_duplicate_as_fp() {
        let config = VddConfig::default();
        let engine = VddEngine {
            config,
            app_config: AppConfig {
                proxy: crate::config::ProxyConfig::default(),
                providers: std::collections::HashMap::new(),
                hooks: crate::config::HooksConfig::default(),
                session: crate::config::SessionConfig::default(),
                keybindings: crate::config::KeybindingsConfig::default(),
                vdd: VddConfig::default(),
                guardrails: crate::config::GuardrailsConfig::default(),
                permissions: crate::config::PermissionsConfig::default(),
                managed_settings_path: None,
            },
            client: Client::new(),
        };

        let mut findings = vec![Finding {
            id: "1".to_string(),
            severity: Severity::Medium,
            cwe: None,
            description: "SQL injection in query builder module".to_string(),
            file_path: None,
            line_range: None,
            status: FindingStatus::Genuine,
            adversary_reasoning: "".to_string(),
            iteration: 2,
        }];

        let previous_fps = vec!["SQL injection in query builder module".to_string()];
        // Layer 1 (duplicate detection) catches this before Layer 3 (AI)
        // would be reached, so no API call is made.
        engine
            .triage_findings(
                &mut findings,
                &previous_fps,
                "fn main() {}",
                "test",
                None,
            )
            .await;
        assert_eq!(findings[0].status, FindingStatus::FalsePositive);
    }

    /// Test that Layer 2 (pattern matching) catches common Rust FPs.
    #[tokio::test]
    async fn test_triage_marks_common_pattern_as_fp() {
        let config = VddConfig::default();
        let engine = VddEngine {
            config,
            app_config: AppConfig {
                proxy: crate::config::ProxyConfig::default(),
                providers: std::collections::HashMap::new(),
                hooks: crate::config::HooksConfig::default(),
                session: crate::config::SessionConfig::default(),
                keybindings: crate::config::KeybindingsConfig::default(),
                vdd: VddConfig::default(),
                guardrails: crate::config::GuardrailsConfig::default(),
                permissions: crate::config::PermissionsConfig::default(),
                managed_settings_path: None,
            },
            client: Client::new(),
        };

        let mut findings = vec![Finding {
            id: "1".to_string(),
            severity: Severity::High,
            cwe: Some("CWE-362".to_string()),
            description: "Panic on poisoned mutex — unwrap() on mutex can crash".to_string(),
            file_path: Some("src/main.rs".to_string()),
            line_range: Some((10, 10)),
            status: FindingStatus::Genuine,
            adversary_reasoning: "The code calls unwrap() on mutex which panics if poisoned"
                .to_string(),
            iteration: 1,
        }];

        // Layer 2 catches this common Rust pattern before AI verification
        engine
            .triage_findings(
                &mut findings,
                &[],
                "let guard = mutex.lock().unwrap();",
                "test",
                None,
            )
            .await;
        assert_eq!(findings[0].status, FindingStatus::FalsePositive);
    }

    /// Test that findings surviving layers 1-2 remain Genuine when
    /// AI verification fails (non-blocking fallback).
    #[tokio::test]
    async fn test_triage_ai_failure_is_nonblocking() {
        let config = VddConfig::default();
        let engine = VddEngine {
            config,
            app_config: AppConfig {
                proxy: crate::config::ProxyConfig::default(),
                providers: std::collections::HashMap::new(), // No provider = AI call will fail
                hooks: crate::config::HooksConfig::default(),
                session: crate::config::SessionConfig::default(),
                keybindings: crate::config::KeybindingsConfig::default(),
                vdd: VddConfig::default(),
                guardrails: crate::config::GuardrailsConfig::default(),
                permissions: crate::config::PermissionsConfig::default(),
                managed_settings_path: None,
            },
            client: Client::new(),
        };

        let mut findings = vec![Finding {
            id: "novel-issue".to_string(),
            severity: Severity::High,
            cwe: Some("CWE-89".to_string()),
            description: "Novel SQL injection that no pattern catches".to_string(),
            file_path: Some("src/db.rs".to_string()),
            line_range: Some((45, 52)),
            status: FindingStatus::Genuine,
            adversary_reasoning: "User input concatenated into query".to_string(),
            iteration: 1,
        }];

        // AI verification will fail (no provider configured) but should
        // warn the user and keep the finding as Genuine (safe fallback).
        engine
            .triage_findings(
                &mut findings,
                &[],
                "let query = format!(\"SELECT * FROM users WHERE id = {}\", user_input);",
                "nonexistent-provider",
                None,
            )
            .await;
        assert_eq!(
            findings[0].status,
            FindingStatus::Genuine,
            "AI verification failure must not demote genuine findings"
        );
    }
}
