//! VDD system prompts and request-template builders (adversary + revision).

use std::fmt::Write;

use crate::config::{AppConfig, VddConfig};
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

use crate::vdd::finding::Finding;
use crate::vdd::helpers::truncate_output;
use crate::vdd::static_analysis::StaticAnalysisResult;

/// System prompt for the verification agent. This is a separate step from
/// the adversary — it evaluates the adversary's findings against the actual
/// code to detect confabulated (hallucinated) findings.
pub const VERIFIER_SYSTEM_PROMPT: &str = r#"You are a verification agent in a Verification-Driven Development (VDD) loop. Your job is to evaluate whether adversary findings about code are GENUINE or CONFABULATED (hallucinated).

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
pub const ADVERSARY_SYSTEM_PROMPT: &str = r#"You are an adversarial code reviewer operating in a Verification-Driven Development (VDD) loop. Your role is to find genuine bugs, security vulnerabilities, logic errors, and correctness issues in the code changes presented to you.

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

/// Build a fresh adversary request with complete context isolation.
/// The adversary sees ONLY: its system prompt, the builder's output,
/// the original task description, and static analysis results.
pub fn build_adversary_request(
    config: &VddConfig,
    app_config: &AppConfig,
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
    let model = config.adversary.model.clone().unwrap_or_else(|| {
        app_config
            .providers
            .get(&config.adversary.provider)
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
        temperature: Some(config.adversary.temperature),
        max_tokens: Some(config.adversary.max_tokens),
        stream: Some(false), // Always non-streaming for VDD
        tools: None,
        tool_choice: None,
        extra: std::collections::HashMap::new(),
    }
}

/// Build a revision request to send back to the builder with genuine findings.
pub fn build_revision_request(
    original_request: &ChatCompletionRequest,
    genuine_findings: &[&Finding],
    iteration: u32,
) -> ChatCompletionRequest {
    let mut findings_text = String::from(
        "The following genuine issues were found by adversarial review. \
         Fix ALL of them in your revised response:\n\n",
    );

    for (i, finding) in genuine_findings.iter().enumerate() {
        let _ = write!(
            findings_text,
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
