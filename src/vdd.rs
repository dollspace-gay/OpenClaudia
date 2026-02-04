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

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{AppConfig, VddConfig, VddMode};
use crate::providers::get_adapter;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use crate::session::TokenUsage;

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
// Core Types
// ==========================================================================

/// Severity classification for adversary findings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Critical => write!(f, "CRITICAL"),
            Severity::High => write!(f, "HIGH"),
            Severity::Medium => write!(f, "MEDIUM"),
            Severity::Low => write!(f, "LOW"),
            Severity::Info => write!(f, "INFO"),
        }
    }
}

/// Whether a finding is genuine or a false positive
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Genuine,
    FalsePositive,
    Disputed,
}

/// A single finding from the adversary's review
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub cwe: Option<String>,
    pub description: String,
    pub file_path: Option<String>,
    pub line_range: Option<(usize, usize)>,
    pub status: FindingStatus,
    pub adversary_reasoning: String,
    pub iteration: u32,
}

/// Raw finding from adversary JSON before triage
#[derive(Debug, Deserialize)]
struct RawFinding {
    severity: Option<String>,
    cwe: Option<String>,
    description: Option<String>,
    file: Option<String>,
    lines: Option<Vec<usize>>,
    reasoning: Option<String>,
}

/// Parsed adversary response
#[derive(Debug, Deserialize)]
struct AdversaryResponse {
    findings: Option<Vec<RawFinding>>,
    assessment: Option<String>,
}

/// Result of a single adversary review iteration
#[derive(Debug, Clone, Serialize)]
pub struct AdversaryReview {
    pub iteration: u32,
    pub findings: Vec<Finding>,
    pub raw_response: String,
    pub tokens_used: TokenUsage,
    pub timestamp: DateTime<Utc>,
}

/// Result of running a static analysis command
#[derive(Debug, Clone, Serialize)]
pub struct StaticAnalysisResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub passed: bool,
}

/// A complete adversarial loop iteration (builder response + analysis + adversary review)
#[derive(Debug, Clone, Serialize)]
pub struct VddIteration {
    pub number: u32,
    pub builder_response: String,
    pub static_analysis: Vec<StaticAnalysisResult>,
    pub adversary_review: AdversaryReview,
    pub genuine_count: u32,
    pub false_positive_count: u32,
}

/// Full VDD session tracking across all iterations
#[derive(Debug, Clone, Serialize)]
pub struct VddSession {
    pub id: String,
    pub mode: VddMode,
    pub iterations: Vec<VddIteration>,
    pub total_findings: u32,
    pub total_genuine: u32,
    pub total_false_positives: u32,
    pub false_positive_rate: f32,
    pub converged: bool,
    pub termination_reason: Option<String>,
    pub builder_tokens: TokenUsage,
    pub adversary_tokens: TokenUsage,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl VddSession {
    fn new(mode: VddMode) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            mode,
            iterations: Vec::new(),
            total_findings: 0,
            total_genuine: 0,
            total_false_positives: 0,
            false_positive_rate: 0.0,
            converged: false,
            termination_reason: None,
            builder_tokens: TokenUsage::default(),
            adversary_tokens: TokenUsage::default(),
            started_at: Utc::now(),
            ended_at: None,
        }
    }

    fn record_iteration(&mut self, iteration: VddIteration) {
        self.total_findings += iteration.genuine_count + iteration.false_positive_count;
        self.total_genuine += iteration.genuine_count;
        self.total_false_positives += iteration.false_positive_count;
        self.false_positive_rate = if self.total_findings > 0 {
            self.total_false_positives as f32 / self.total_findings as f32
        } else {
            0.0
        };
        self.adversary_tokens
            .accumulate(&iteration.adversary_review.tokens_used);
        self.iterations.push(iteration);
    }

    fn finalize(&mut self, converged: bool, reason: &str) {
        self.converged = converged;
        self.termination_reason = Some(reason.to_string());
        self.ended_at = Some(Utc::now());
    }
}

// ==========================================================================
// Confabulation Tracker
// ==========================================================================

/// Tracks false positive rates across iterations to detect when the adversary
/// starts hallucinating problems (confabulation threshold).
#[derive(Debug, Clone)]
pub struct ConfabulationTracker {
    /// FP rate per iteration
    pub history: Vec<f32>,
    /// Threshold above which we consider the adversary is confabulating
    pub threshold: f32,
    /// Minimum iterations before checking threshold
    pub min_iterations: u32,
}

impl ConfabulationTracker {
    pub fn new(threshold: f32, min_iterations: u32) -> Self {
        Self {
            history: Vec::new(),
            threshold,
            min_iterations,
        }
    }

    /// Record an iteration's finding counts
    pub fn record_iteration(&mut self, genuine: u32, false_positives: u32) {
        let total = genuine + false_positives;
        let rate = if total > 0 {
            false_positives as f32 / total as f32
        } else {
            // No findings at all = consider it a clean pass (FP rate 1.0 for convergence)
            1.0
        };
        self.history.push(rate);
    }

    /// Current cumulative false positive rate
    pub fn current_rate(&self) -> f32 {
        if self.history.is_empty() {
            return 0.0;
        }
        let total: f32 = self.history.iter().sum();
        total / self.history.len() as f32
    }

    /// Most recent iteration's false positive rate
    pub fn latest_rate(&self) -> f32 {
        self.history.last().copied().unwrap_or(0.0)
    }

    /// Should the loop terminate? Checks both minimum iterations and threshold.
    pub fn should_terminate(&self) -> bool {
        if (self.history.len() as u32) < self.min_iterations {
            return false;
        }
        self.latest_rate() >= self.threshold
    }
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
        if !self.config.static_analysis.enabled || self.config.static_analysis.commands.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();
        let timeout = Duration::from_secs(self.config.static_analysis.timeout_seconds);

        for command in &self.config.static_analysis.commands {
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
        let parsed: Option<AdversaryResponse> =
            serde_json::from_str(adversary_response).ok().or_else(|| {
                // Try to extract JSON from markdown code blocks
                extract_json_from_response(adversary_response)
                    .and_then(|json| serde_json::from_str(&json).ok())
            }).or_else(|| {
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
        let endpoint = adapter.chat_endpoint();

        let response = forward_request(
            &self.client,
            provider_config,
            &self.config.adversary.provider,
            &request.model,
            endpoint,
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
        let endpoint = adapter.chat_endpoint();

        let response = forward_request(
            &self.client,
            provider_config,
            provider_name,
            &request.model,
            endpoint,
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
        format!(
            "{}/v1beta/models/{}:generateContent",
            base_url, model
        )
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

/// Extract the text content from a chat completion response.
/// Supports OpenAI, Anthropic, and Google/Gemini formats.
fn extract_response_text(response: &Value) -> String {
    // OpenAI format: choices[0].message.content
    if let Some(content) = response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return content.to_string();
    }

    // Anthropic format: content[0].text
    if let Some(content) = response
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
        })
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
    {
        return content.to_string();
    }

    // Google/Gemini format: candidates[0].content.parts[0].text
    if let Some(content) = response
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.get(0))
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
    {
        return content.to_string();
    }

    // Log what we actually received for debugging
    debug!(
        "VDD: Unknown response format, dumping structure: {:?}",
        response.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );

    String::new()
}

/// Extract token usage from a provider response.
fn extract_token_usage(response: &Value) -> TokenUsage {
    // OpenAI/Anthropic format: usage.prompt_tokens / usage.completion_tokens
    if let Some(usage) = response.get("usage") {
        return TokenUsage {
            input_tokens: usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_write_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };
    }

    // Google/Gemini format: usageMetadata.promptTokenCount / candidatesTokenCount
    if let Some(usage) = response.get("usageMetadata") {
        return TokenUsage {
            input_tokens: usage
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: usage
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_tokens: usage
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_write_tokens: 0,
        };
    }

    TokenUsage::default()
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

/// Try to extract JSON from a response that may contain markdown code blocks.
fn extract_json_from_response(text: &str) -> Option<String> {
    // Look for ```json ... ``` blocks
    if let Some(start) = text.find("```json") {
        let json_start = start + 7;
        if let Some(end) = text[json_start..].find("```") {
            return Some(text[json_start..json_start + end].trim().to_string());
        }
    }

    // Look for ``` ... ``` blocks
    if let Some(start) = text.find("```") {
        let json_start = start + 3;
        // Skip optional language identifier on the same line
        let line_end = text[json_start..].find('\n').unwrap_or(0);
        let actual_start = json_start + line_end;
        if let Some(end) = text[actual_start..].find("```") {
            return Some(text[actual_start..actual_start + end].trim().to_string());
        }
    }

    // Try to find raw JSON object starting with {"findings"
    if let Some(start) = text.find(r#"{"findings""#) {
        // Find the matching closing brace
        let mut depth = 0;
        let mut end_pos = start;
        for (i, c) in text[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end_pos > start {
            return Some(text[start..=end_pos].to_string());
        }
    }

    // Try to find raw JSON object (any)
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }

    None
}

/// Try to construct a valid AdversaryResponse from partial/malformed JSON
fn try_parse_relaxed(text: &str) -> Option<AdversaryResponse> {
    // Check for "NO_FINDINGS" or "no findings" anywhere in response
    let lower = text.to_lowercase();
    if lower.contains("no_findings")
        || lower.contains("no findings")
        || lower.contains("no issues")
        || lower.contains("no vulnerabilities")
        || lower.contains("code looks correct")
        || lower.contains("looks good")
    {
        return Some(AdversaryResponse {
            findings: Some(vec![]),
            assessment: Some("NO_FINDINGS".to_string()),
        });
    }

    None
}

/// Parse a severity string into the Severity enum.
fn parse_severity(s: &str) -> Severity {
    match s.to_uppercase().as_str() {
        "CRITICAL" => Severity::Critical,
        "HIGH" => Severity::High,
        "MEDIUM" | "MED" => Severity::Medium,
        "LOW" => Severity::Low,
        _ => Severity::Info,
    }
}

/// Simple string similarity based on shared word overlap (Jaccard-like).
fn string_similarity(a: &str, b: &str) -> f32 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        return 0.0;
    }

    intersection as f32 / union as f32
}

/// Detect common false positive patterns in adversary findings.
fn is_common_false_positive(description: &str, reasoning: &str) -> bool {
    let combined = format!("{} {}", description, reasoning);

    let false_positive_patterns = [
        // Standard Rust patterns the adversary may flag incorrectly
        "unwrap() on mutex",
        "poisoned mutex",
        "hardcoded password in test",
        "hardcoded key in test",
        "hardcoded secret in test",
        "deprecated api",
        // Standard library usage that's actually correct
        "silent fallback on mlock",
        "graceful degradation",
        // Protocol-mandated choices
        "hmac-sha1 in yubikey",
        "yubikey hardware uses",
        // Admin-configured values
        "ssrf via.*endpoint",
        "admin-configured.*trusted",
    ];

    for pattern in &false_positive_patterns {
        if combined.contains(pattern) {
            return true;
        }
    }

    // Check for regex patterns
    let regex_patterns = [
        r"test\s+(code|file|module)\s+(requires|needs|uses)\s+deterministic",
        r"admin[\-\s]configured\s+(endpoint|url|path)",
    ];

    for pattern in &regex_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if re.is_match(&combined) {
                return true;
            }
        }
    }

    false
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

/// Run a shell command with timeout, returning structured result.
async fn run_shell_command(command: &str, timeout: Duration) -> StaticAnalysisResult {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };

    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(shell)
            .arg(flag)
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            StaticAnalysisResult {
                command: command.to_string(),
                exit_code,
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                passed: exit_code == 0,
            }
        }
        Ok(Err(e)) => StaticAnalysisResult {
            command: command.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Command failed to execute: {}", e),
            passed: false,
        },
        Err(_) => StaticAnalysisResult {
            command: command.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Command timed out after {}s", timeout.as_secs()),
            passed: false,
        },
    }
}

/// Run `chainlink create` and `chainlink label` to create an issue.
async fn run_chainlink_create(title: &str, label: &str, comment: &str) -> Result<String, VddError> {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };

    // Create the issue
    let create_output = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(format!(
            "chainlink create \"{}\" -p high",
            title.replace('"', "\\\"")
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| VddError::ChainlinkError(format!("Failed to run chainlink: {}", e)))?;

    let create_text = String::from_utf8_lossy(&create_output.stdout);

    // Extract issue ID from output like "Created issue #123"
    let issue_id = create_text
        .split('#')
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("unknown")
        .to_string();

    // Label it
    let _ = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(format!("chainlink label {} {}", issue_id, label))
        .output()
        .await;

    // Add comment with details
    let _ = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(format!(
            "chainlink comment {} \"{}\"",
            issue_id,
            comment.replace('"', "\\\"").replace('\n', " ")
        ))
        .output()
        .await;

    Ok(issue_id)
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confabulation_tracker_below_min_iterations() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(0, 5); // 100% FP but only 1 iteration
        assert!(!tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_terminates() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(2, 3); // 60% FP
        tracker.record_iteration(1, 5); // 83% FP — above threshold, past min
        assert!(tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_does_not_terminate_below_threshold() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(3, 2); // 40% FP
        tracker.record_iteration(2, 2); // 50% FP
        assert!(!tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_no_findings_terminates() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(1, 0); // some genuine first
        tracker.record_iteration(0, 0); // no findings = 1.0 FP rate
        assert!(tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_current_rate() {
        let mut tracker = ConfabulationTracker::new(0.75, 1);
        tracker.record_iteration(2, 8); // 80%
        tracker.record_iteration(1, 4); // 80%
        assert!((tracker.current_rate() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_confabulation_tracker_empty() {
        let tracker = ConfabulationTracker::new(0.75, 2);
        assert_eq!(tracker.current_rate(), 0.0);
        assert_eq!(tracker.latest_rate(), 0.0);
        assert!(!tracker.should_terminate());
    }

    #[test]
    fn test_parse_severity() {
        assert_eq!(parse_severity("CRITICAL"), Severity::Critical);
        assert_eq!(parse_severity("critical"), Severity::Critical);
        assert_eq!(parse_severity("HIGH"), Severity::High);
        assert_eq!(parse_severity("MEDIUM"), Severity::Medium);
        assert_eq!(parse_severity("MED"), Severity::Medium);
        assert_eq!(parse_severity("LOW"), Severity::Low);
        assert_eq!(parse_severity("INFO"), Severity::Info);
        assert_eq!(parse_severity("unknown"), Severity::Info);
    }

    #[test]
    fn test_string_similarity_identical() {
        assert!((string_similarity("hello world", "hello world") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_string_similarity_disjoint() {
        assert!((string_similarity("hello world", "foo bar")).abs() < 0.01);
    }

    #[test]
    fn test_string_similarity_partial() {
        let sim = string_similarity(
            "sql injection in query builder",
            "sql injection in db module",
        );
        assert!(sim > 0.3 && sim < 0.8);
    }

    #[test]
    fn test_string_similarity_empty() {
        assert!((string_similarity("", "") - 1.0).abs() < 0.01);
        assert!((string_similarity("hello", "")).abs() < 0.01);
    }

    #[test]
    fn test_extract_json_from_code_block() {
        let text = r#"Here is my analysis:
```json
{"findings": [], "assessment": "NO_FINDINGS"}
```
"#;
        let json = extract_json_from_response(text).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["assessment"], "NO_FINDINGS");
    }

    #[test]
    fn test_extract_json_from_raw() {
        let text = r#"Some preamble text {"findings": [{"severity": "HIGH"}], "assessment": "FINDINGS_PRESENT"} trailing text"#;
        let json = extract_json_from_response(text).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["assessment"], "FINDINGS_PRESENT");
    }

    #[test]
    fn test_extract_response_text_openai_format() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Hello from the model"
                }
            }]
        });
        assert_eq!(extract_response_text(&response), "Hello from the model");
    }

    #[test]
    fn test_extract_response_text_anthropic_format() {
        let response = serde_json::json!({
            "content": [{
                "type": "text",
                "text": "Hello from Anthropic"
            }]
        });
        assert_eq!(extract_response_text(&response), "Hello from Anthropic");
    }

    #[test]
    fn test_extract_response_text_empty() {
        let response = serde_json::json!({});
        assert_eq!(extract_response_text(&response), "");
    }

    #[test]
    fn test_extract_response_text_google_format() {
        let response = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "text": "Hello from Gemini"
                    }]
                }
            }]
        });
        assert_eq!(extract_response_text(&response), "Hello from Gemini");
    }

    #[test]
    fn test_extract_token_usage_google_format() {
        let response = serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 150,
                "candidatesTokenCount": 80,
                "cachedContentTokenCount": 25
            }
        });
        let usage = extract_token_usage(&response);
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 25);
    }

    #[test]
    fn test_extract_token_usage_openai() {
        let response = serde_json::json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });
        let usage = extract_token_usage(&response);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn test_extract_token_usage_anthropic() {
        let response = serde_json::json!({
            "usage": {
                "input_tokens": 200,
                "output_tokens": 75,
                "cache_read_input_tokens": 50,
                "cache_creation_input_tokens": 10
            }
        });
        let usage = extract_token_usage(&response);
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(usage.cache_read_tokens, 50);
        assert_eq!(usage.cache_write_tokens, 10);
    }

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
    fn test_is_common_false_positive() {
        assert!(is_common_false_positive(
            "mutex unwrap() on poisoned mutex could panic",
            "the code uses unwrap() on mutex"
        ));
        assert!(is_common_false_positive(
            "hardcoded password in test file",
            "test code has password = 'test123'"
        ));
        assert!(!is_common_false_positive(
            "sql injection in user input handler",
            "string concatenation used for query"
        ));
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
    fn test_vdd_session_record_iteration() {
        let mut session = VddSession::new(VddMode::Blocking);
        let iteration = VddIteration {
            number: 1,
            builder_response: "code here".to_string(),
            static_analysis: Vec::new(),
            adversary_review: AdversaryReview {
                iteration: 1,
                findings: Vec::new(),
                raw_response: "{}".to_string(),
                tokens_used: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    ..Default::default()
                },
                timestamp: Utc::now(),
            },
            genuine_count: 2,
            false_positive_count: 3,
        };

        session.record_iteration(iteration);
        assert_eq!(session.total_findings, 5);
        assert_eq!(session.total_genuine, 2);
        assert_eq!(session.total_false_positives, 3);
        assert!((session.false_positive_rate - 0.6).abs() < 0.01);
        assert_eq!(session.adversary_tokens.input_tokens, 100);
    }

    #[test]
    fn test_vdd_session_finalize() {
        let mut session = VddSession::new(VddMode::Advisory);
        session.finalize(true, "Confabulation threshold reached");
        assert!(session.converged);
        assert_eq!(
            session.termination_reason,
            Some("Confabulation threshold reached".to_string())
        );
        assert!(session.ended_at.is_some());
    }

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Critical < Severity::High);
        assert!(Severity::High < Severity::Medium);
        assert!(Severity::Medium < Severity::Low);
        assert!(Severity::Low < Severity::Info);
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(format!("{}", Severity::Critical), "CRITICAL");
        assert_eq!(format!("{}", Severity::High), "HIGH");
        assert_eq!(format!("{}", Severity::Medium), "MEDIUM");
        assert_eq!(format!("{}", Severity::Low), "LOW");
        assert_eq!(format!("{}", Severity::Info), "INFO");
    }

    #[test]
    fn test_parse_findings_valid_json() {
        use crate::config::{VddConfig, VddTracking};

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
            tracking: crate::config::VddTracking {
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
