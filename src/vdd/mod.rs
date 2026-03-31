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
    pub fn new(config: &VddConfig, app_config: &AppConfig, client: Client) -> Self {
        Self {
            config: config.clone(),
            app_config: app_config.clone(),
            client,
        }
    }

    /// Simplified entry point for chat loop integration.
    /// Takes just the builder text and user task, returns findings for injection.
    pub async fn review_text(
        &self,
        builder_text: &str,
        user_task: &str,
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

        // Parse and triage findings
        let mut findings = self.parse_findings(&adversary_text, 1);
        self.triage_findings(&mut findings, &[]);

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
    pub async fn process_response(
        &self,
        builder_response: &Value,
        original_request: &ChatCompletionRequest,
        builder_provider: &str,
        builder_api_key: &str,
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
                    .advisory_review(&builder_text, original_request)
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
    ) -> Result<VddAdvisoryResult, VddError> {
        // Run static analysis
        let static_results = self.run_static_analysis().await;

        // Extract original task from request
        let original_task = extract_user_task(original_request);

        // Build and send adversary request
        let adversary_request =
            self.build_adversary_request(builder_text, &original_task, &static_results, 1);

        let (adversary_text, tokens_used) = self.send_to_adversary(&adversary_request).await?;

        // Parse and triage findings
        let mut findings = self.parse_findings(&adversary_text, 1);
        self.triage_findings(&mut findings, &[]);

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
        builder_api_key: &str,
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

            // Step 3: Parse and triage findings
            let mut findings = self.parse_findings(&adversary_text, iteration);
            self.triage_findings(&mut findings, &previous_fps);

            let genuine_count = findings
                .iter()
                .filter(|f| f.status == FindingStatus::Genuine)
                .count() as u32;
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
                        session.finalize(false, &format!("Builder revision failed: {}", e));
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

        let chainlink_issues = if !all_genuine.is_empty() {
            match self.create_chainlink_issues(&all_genuine).await {
                Ok(ids) => ids,
                Err(e) => {
                    warn!("VDD: Chainlink issue creation failed: {}", e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
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
            "## Original Task\n{}\n\n## Builder Output (Iteration {})\n{}",
            original_task, iteration, builder_output
        );

        // Append static analysis results if any
        if !static_analysis_results.is_empty() {
            user_content.push_str("\n\n## Static Analysis Results\n");
            for result in static_analysis_results {
                user_content.push_str(&format!(
                    "\n### `{}`\n**Exit code:** {} ({})\n",
                    result.command,
                    result.exit_code,
                    if result.passed { "PASSED" } else { "FAILED" }
                ));
                if !result.stdout.is_empty() {
                    let truncated = truncate_output(&result.stdout, 2000);
                    user_content.push_str(&format!("**stdout:**\n```\n{}\n```\n", truncated));
                }
                if !result.stderr.is_empty() {
                    let truncated = truncate_output(&result.stderr, 2000);
                    user_content.push_str(&format!("**stderr:**\n```\n{}\n```\n", truncated));
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

        let raw_findings = match parsed {
            Some(response) => {
                if response.assessment.as_deref() == Some("NO_FINDINGS") {
                    info!("VDD: Adversary reported no findings");
                    return Vec::new();
                }
                response.findings.unwrap_or_default()
            }
            None => {
                warn!("VDD: Could not parse adversary response as JSON, treating as no findings");
                info!(
                    "VDD: Unparseable response preview: {}",
                    truncate_output(adversary_response, 500)
                );
                return Vec::new();
            }
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

    /// Triage findings: mark duplicates and previously-seen false positives.
    fn triage_findings(&self, findings: &mut [Finding], previous_fps: &[String]) {
        for finding in findings.iter_mut() {
            // If this finding's description closely matches a previous false positive,
            // mark it as false positive (the adversary is re-reporting a known non-issue)
            let desc_lower = finding.description.to_lowercase();
            for fp_desc in previous_fps {
                if string_similarity(&desc_lower, &fp_desc.to_lowercase()) > 0.7 {
                    finding.status = FindingStatus::FalsePositive;
                    break;
                }
            }

            // Common false positive patterns for Rust code
            if finding.status == FindingStatus::Genuine {
                let desc = &finding.description.to_lowercase();
                if is_common_false_positive(desc, &finding.adversary_reasoning.to_lowercase()) {
                    finding.status = FindingStatus::FalsePositive;
                }
            }
        }
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
                finding.line_range
                    .map(|(s, e)| format!("{}-{}", s, e))
                    .unwrap_or_else(|| "N/A".to_string()),
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
            findings_text.push_str(&format!(
                "### Finding {} [{}] {}\n**File:** {}\n**Lines:** {}\n{}\n\n**Reasoning:** {}\n\n",
                i + 1,
                finding.severity,
                finding.cwe.as_deref().unwrap_or(""),
                finding.file_path.as_deref().unwrap_or("N/A"),
                finding
                    .line_range
                    .map(|(s, e)| format!("{}-{}", s, e))
                    .unwrap_or_else(|| "N/A".to_string()),
                finding.description,
                finding.adversary_reasoning,
            ));
        }

        // Clone original messages and append the revision request
        let mut messages = original_request.messages.clone();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text(format!(
                "<vdd-revision iteration=\"{}\">\n{}</vdd-revision>",
                iteration, findings_text
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

    /// Send a request to the adversary provider. Returns (response_text, token_usage).
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
            .as_deref()
            .or(provider_config.api_key.as_deref())
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

        let response = forward_request(
            &self.client,
            provider_config,
            &self.config.adversary.provider,
            &request.model,
            &endpoint,
            &transformed,
            headers,
        )
        .await
        .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

        let response_json: Value = response
            .json()
            .await
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
        api_key: &str,
    ) -> Result<(String, Value, TokenUsage), VddError> {
        let provider_config = self
            .app_config
            .providers
            .get(provider_name)
            .ok_or_else(|| {
                VddError::BuilderRevisionFailed(format!(
                    "Builder provider '{}' not configured",
                    provider_name
                ))
            })?;

        let adapter = get_adapter(provider_name);
        let transformed = adapter
            .transform_request(request)
            .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

        let headers = adapter.get_headers(api_key);
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
        format!("{}/v1beta/models/{}:generateContent", base_url, model)
    } else {
        format!("{}{}", base_url, endpoint)
    };

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
            output.push_str(&format!(
                "{}. [{}] {}{}: {}\n",
                i + 1,
                finding.severity,
                finding
                    .cwe
                    .as_deref()
                    .map(|c| format!("{} ", c))
                    .unwrap_or_default(),
                finding
                    .file_path
                    .as_deref()
                    .map(|f| format!(" in {}", f))
                    .unwrap_or_default(),
                finding.description
            ));
        }
        output.push_str("\nAddress these issues in your next response.\n");
    }

    let failed_analysis: Vec<&StaticAnalysisResult> =
        static_analysis.iter().filter(|r| !r.passed).collect();
    if !failed_analysis.is_empty() {
        output.push_str("\nStatic analysis failures:\n");
        for result in failed_analysis {
            output.push_str(&format!(
                "- `{}` (exit code {})\n",
                result.command, result.exit_code
            ));
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

    #[test]
    fn test_triage_marks_duplicate_as_fp() {
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
        engine.triage_findings(&mut findings, &previous_fps);
        assert_eq!(findings[0].status, FindingStatus::FalsePositive);
    }
}
