//! Three-layer finding triage: duplicate detection, pattern heuristics, AI verification.

use std::collections::HashMap;
use std::fmt::Write;

use reqwest::Client;
use serde_json::Value;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{AppConfig, VddConfig};
use crate::providers::ApiKey;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

use crate::vdd::confabulation::{is_common_false_positive, string_similarity};
use crate::vdd::error::VddError;
use crate::vdd::finding::{Finding, FindingStatus};
use crate::vdd::helpers::truncate_output;
use crate::vdd::parsing::{extract_json_from_response, parse_severity, try_parse_relaxed};
use crate::vdd::prompts::VERIFIER_SYSTEM_PROMPT;
use crate::vdd::review::AdversaryResponse;
use crate::vdd::transport::send_to_builder_for_verification;

/// Parse adversary response text into structured findings.
pub fn parse_findings(adversary_response: &str, iteration: u32) -> Vec<Finding> {
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

/// Inputs needed by the triage pipeline. Bundled into a struct so the
/// `triage_findings` API stays under the `too_many_arguments` lint threshold.
pub struct TriageContext<'a> {
    pub client: &'a Client,
    pub config: &'a VddConfig,
    pub app_config: &'a AppConfig,
    pub previous_fps: &'a [String],
    pub builder_code: &'a str,
    pub builder_provider: &'a str,
    pub builder_api_key: Option<&'a ApiKey>,
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
pub async fn triage_findings(findings: &mut [Finding], ctx: &TriageContext<'_>) {
    apply_duplicate_layer(findings, ctx.previous_fps);
    apply_pattern_layer(findings);

    // Layer 3: AI-powered verification — expensive but catches novel confabulations.
    // Only verify findings that survived layers 1 and 2.
    let surviving_genuine: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.status == FindingStatus::Genuine)
        .collect();

    if surviving_genuine.is_empty() {
        return;
    }

    match verify_findings(ctx, &surviving_genuine).await {
        Ok(verdicts) => apply_verdicts(findings, &verdicts),
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

/// Layer 1: mark findings whose descriptions closely match a previously
/// confirmed false positive.
fn apply_duplicate_layer(findings: &mut [Finding], previous_fps: &[String]) {
    for finding in findings.iter_mut() {
        let desc_lower = finding.description.to_lowercase();
        for fp_desc in previous_fps {
            if string_similarity(&desc_lower, &fp_desc.to_lowercase()) > 0.7 {
                finding.status = FindingStatus::FalsePositive;
                break;
            }
        }
    }
}

/// Layer 2: mark findings that match hardcoded false-positive patterns.
fn apply_pattern_layer(findings: &mut [Finding]) {
    for finding in findings.iter_mut() {
        if finding.status == FindingStatus::Genuine {
            let desc = &finding.description.to_lowercase();
            if is_common_false_positive(desc, &finding.adversary_reasoning.to_lowercase()) {
                finding.status = FindingStatus::FalsePositive;
            }
        }
    }
}

/// Apply verifier verdicts to the remaining Genuine findings (layer 3).
fn apply_verdicts(findings: &mut [Finding], verdicts: &HashMap<String, String>) {
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

/// Send findings to a verification agent that checks each adversary claim
/// against the actual code.  Uses the **builder's** provider (not the
/// adversary's) to get an independent second opinion.
///
/// Returns a map of `finding_id` → "genuine" | "confabulated".
async fn verify_findings(
    ctx: &TriageContext<'_>,
    findings: &[&Finding],
) -> Result<HashMap<String, String>, VddError> {
    if findings.is_empty() {
        return Ok(HashMap::new());
    }

    eprintln!(
        "\x1b[36m🔍 VDD verifier: checking {} finding(s) against code via {}\x1b[0m",
        findings.len(),
        ctx.builder_provider
    );

    // Build a code view centered on the findings' cited line ranges
    // rather than blindly truncating to a prefix — a finding that
    // cited lines past byte 12_000 used to be demoted to
    // FalsePositive simply because the verifier could not see the
    // code. See crosslink #498.
    let (code_view, code_truncated) =
        build_verification_code_view(ctx.builder_code, findings, 12_000);
    if code_truncated {
        tracing::warn!(
            findings = findings.len(),
            max_chars = 12_000,
            "VDD verification code view was truncated; findings whose cited lines \
             fell outside the view will be kept as Genuine (fail-safe per #498c)."
        );
    }
    let mut user_content =
        format!("## Code Under Review\n```\n{code_view}\n```\n\n## Adversary Findings to Verify\n");

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
                .map_or_else(|| "unknown".to_string(), |(a, b)| format!("{a}-{b}")),
            finding.description,
            finding.adversary_reasoning,
        );
    }

    // Use the builder's provider model for verification — independent
    // from the adversary to avoid correlated confabulation.
    let model = ctx
        .app_config
        .providers
        .get(ctx.builder_provider)
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
        max_tokens: Some(ctx.config.adversary.max_tokens),
        stream: Some(false),
        tools: None,
        tool_choice: None,
        extra: std::collections::HashMap::new(),
    };

    // Route through the builder's provider, not the adversary's
    let (response_text, tokens) = send_to_builder_for_verification(
        ctx.client,
        ctx.config,
        ctx.app_config,
        &request,
        ctx.builder_provider,
        ctx.builder_api_key,
    )
    .await?;

    eprintln!(
        "\x1b[36m  Verifier used {} input + {} output tokens\x1b[0m",
        tokens.input_tokens, tokens.output_tokens
    );

    // Parse verdicts from the response
    Ok(parse_verification_verdicts(&response_text))
}

/// Parse the verification agent's response into a `finding_id` → verdict map.
fn parse_verification_verdicts(response: &str) -> HashMap<String, String> {
    let mut verdicts = HashMap::new();

    // Try to extract JSON from the response
    let json_str = extract_json_from_response(response).unwrap_or_else(|| response.to_string());

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

    verdicts
}

/// Build the code view shown to the verification LLM, centered around
/// the cited line ranges of each finding.
///
/// The previous implementation truncated `builder_code` to the first
/// `max_chars` bytes regardless of where the cited code lived, which
/// systematically demoted findings past the cutoff to `FalsePositive`
/// ("I can't find this code") even though they were genuine. This
/// helper:
///
///  1. For every finding with `file_path + line_range`, extracts a
///     ±`CONTEXT_LINES`-line window around that range.
///  2. Merges overlapping / adjacent windows.
///  3. Emits the windows in order with `...` separators between them
///     and with `<line_number>: ` prefixes so the verifier can match
///     the adversary's cited lines directly.
///  4. Falls back to raw truncation when no finding has line info OR
///     when the merged window view itself exceeds `max_chars` (in
///     that case we also log at warn! — the fail-safe from #498c is
///     enforced by the caller treating truncated windows as reason
///     to keep findings Genuine rather than demoted).
///
/// Returns the rendered view plus a boolean indicating whether any
/// truncation occurred; callers use the flag to decide whether to
/// fail-safe the affected findings. See crosslink #498.
fn build_verification_code_view(
    builder_code: &str,
    findings: &[&Finding],
    max_chars: usize,
) -> (String, bool) {
    const CONTEXT_LINES: usize = 20;

    // If the whole code fits, no truncation is needed.
    if builder_code.len() <= max_chars {
        return (builder_code.to_string(), false);
    }

    // Collect line ranges from findings that have them. Lines are
    // 1-indexed in the finding; convert to 0-indexed for slicing.
    let ranges: Vec<(usize, usize)> = findings
        .iter()
        .filter_map(|f| f.line_range)
        .map(|(start, end)| {
            let zero_indexed_start = start.saturating_sub(1);
            (
                zero_indexed_start.saturating_sub(CONTEXT_LINES),
                end.saturating_add(CONTEXT_LINES),
            )
        })
        .collect();

    if ranges.is_empty() {
        // No line info — fall back to raw truncation.
        return (truncate_output(builder_code, max_chars), true);
    }

    // Merge overlapping windows.
    let mut sorted = ranges;
    sorted.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in sorted {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    let lines: Vec<&str> = builder_code.lines().collect();
    let mut out = String::new();
    let mut any_truncated = false;
    for (i, (start, end)) in merged.iter().enumerate() {
        if i > 0 {
            out.push_str("\n...\n");
        }
        let end = (*end).min(lines.len());
        for (rel, line) in lines[*start..end].iter().enumerate() {
            let line_num = *start + rel + 1;
            if out.len() + line.len() + 16 > max_chars {
                out.push_str("\n... [window truncated]");
                any_truncated = true;
                break;
            }
            let _ = writeln!(out, "{line_num}: {line}");
        }
        if out.len() >= max_chars {
            any_truncated = true;
            break;
        }
    }

    if out.is_empty() {
        return (truncate_output(builder_code, max_chars), true);
    }

    (out, any_truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::vdd::finding::Severity;

    fn test_app_config() -> AppConfig {
        AppConfig {
            proxy: crate::config::ProxyConfig::default(),
            providers: std::collections::HashMap::new(),
            hooks: crate::config::HooksConfig::default(),
            session: crate::config::SessionConfig::default(),
            keybindings: crate::config::KeybindingsConfig::default(),
            vdd: VddConfig::default(),
            guardrails: crate::config::GuardrailsConfig::default(),
            permissions: crate::config::PermissionsConfig::default(),
            managed_settings_path: None,
        }
    }

    #[test]
    fn test_parse_findings_valid_json() {
        let response = r#"{"findings": [{"severity": "HIGH", "cwe": "CWE-89", "description": "SQL injection", "file": "src/db.rs", "lines": [10, 20], "reasoning": "User input concatenated"}], "assessment": "FINDINGS_PRESENT"}"#;
        let findings = parse_findings(response, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].cwe, Some("CWE-89".to_string()));
        assert_eq!(findings[0].description, "SQL injection");
        assert_eq!(findings[0].file_path, Some("src/db.rs".to_string()));
        assert_eq!(findings[0].line_range, Some((10, 20)));
    }

    #[test]
    fn test_parse_findings_no_findings() {
        let response = r#"{"findings": [], "assessment": "NO_FINDINGS"}"#;
        let findings = parse_findings(response, 1);
        assert!(findings.is_empty());
    }

    /// Test that Layer 1 (duplicate detection) catches re-reported findings
    /// before the AI verification layer is reached.  The AI layer requires
    /// an API call, but duplicates are caught cheaply via string similarity.
    #[tokio::test]
    async fn test_triage_marks_duplicate_as_fp() {
        let config = VddConfig::default();
        let app_config = test_app_config();
        let client = Client::new();

        let mut findings = vec![Finding {
            id: "1".to_string(),
            severity: Severity::Medium,
            cwe: None,
            description: "SQL injection in query builder module".to_string(),
            file_path: None,
            line_range: None,
            status: FindingStatus::Genuine,
            adversary_reasoning: String::new(),
            iteration: 2,
        }];

        let previous_fps = vec!["SQL injection in query builder module".to_string()];
        // Layer 1 (duplicate detection) catches this before Layer 3 (AI)
        // would be reached, so no API call is made.
        let ctx = TriageContext {
            client: &client,
            config: &config,
            app_config: &app_config,
            previous_fps: &previous_fps,
            builder_code: "fn main() {}",
            builder_provider: "test",
            builder_api_key: None,
        };
        triage_findings(&mut findings, &ctx).await;
        assert_eq!(findings[0].status, FindingStatus::FalsePositive);
    }

    /// Test that Layer 2 (pattern matching) catches common Rust FPs.
    #[tokio::test]
    async fn test_triage_marks_common_pattern_as_fp() {
        let config = VddConfig::default();
        let app_config = test_app_config();
        let client = Client::new();

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
        let ctx = TriageContext {
            client: &client,
            config: &config,
            app_config: &app_config,
            previous_fps: &[],
            builder_code: "let guard = mutex.lock().unwrap();",
            builder_provider: "test",
            builder_api_key: None,
        };
        triage_findings(&mut findings, &ctx).await;
        assert_eq!(findings[0].status, FindingStatus::FalsePositive);
    }

    /// Test that findings surviving layers 1-2 remain Genuine when
    /// AI verification fails (non-blocking fallback).
    #[tokio::test]
    async fn test_triage_ai_failure_is_nonblocking() {
        let config = VddConfig::default();
        let app_config = test_app_config(); // No provider = AI call will fail
        let client = Client::new();

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
        let ctx = TriageContext {
            client: &client,
            config: &config,
            app_config: &app_config,
            previous_fps: &[],
            builder_code: "let query = format!(\"SELECT * FROM users WHERE id = {}\", user_input);",
            builder_provider: "nonexistent-provider",
            builder_api_key: None,
        };
        triage_findings(&mut findings, &ctx).await;
        assert_eq!(
            findings[0].status,
            FindingStatus::Genuine,
            "AI verification failure must not demote genuine findings"
        );
    }

    // --- Regression tests for crosslink #498 ---

    fn finding_with_lines(id: &str, start: usize, end: usize) -> Finding {
        Finding {
            id: id.to_string(),
            severity: Severity::Medium,
            cwe: None,
            file_path: Some("lib.rs".to_string()),
            line_range: Some((start, end)),
            description: format!("finding {id}"),
            adversary_reasoning: String::new(),
            status: FindingStatus::Genuine,
            iteration: 0,
        }
    }

    #[test]
    fn code_view_returns_whole_body_when_under_cap() {
        let code = "line1\nline2\nline3\n";
        let f = finding_with_lines("f1", 2, 2);
        let findings: Vec<&Finding> = vec![&f];
        let (view, truncated) = build_verification_code_view(code, &findings, 1000);
        assert_eq!(view, code);
        assert!(!truncated);
    }

    #[test]
    fn code_view_extracts_window_around_cited_lines() {
        // 200 lines, finding cites lines 150-152. Max bytes forces a
        // window view rather than the whole body.
        let code: String = (1..=200).fold(String::new(), |mut s, n| {
            let _ = writeln!(s, "line {n}");
            s
        });
        let f = finding_with_lines("f1", 150, 152);
        let findings: Vec<&Finding> = vec![&f];
        let (view, _) = build_verification_code_view(&code, &findings, 500);
        // Must contain the cited lines even though they're deep in the file.
        assert!(view.contains("line 150"), "missing cited line: {view}");
        assert!(view.contains("line 152"), "missing cited line: {view}");
        // Context lines above/below should also be present.
        assert!(view.contains("line 135") || view.contains("line 140"));
    }

    #[test]
    fn code_view_merges_overlapping_windows() {
        let code: String = (1..=200).fold(String::new(), |mut s, n| {
            let _ = writeln!(s, "line {n}");
            s
        });
        let f1 = finding_with_lines("f1", 50, 52);
        let f2 = finding_with_lines("f2", 55, 57); // overlaps f1's ±20 window
        let findings: Vec<&Finding> = vec![&f1, &f2];
        let (view, _) = build_verification_code_view(&code, &findings, 2000);
        // Should have exactly one `...` separator (none) because the
        // windows merged.
        assert!(
            !view.contains("\n...\n"),
            "overlapping windows not merged: {view}"
        );
        assert!(view.contains("line 50"));
        assert!(view.contains("line 57"));
    }

    #[test]
    fn code_view_falls_back_to_truncation_when_no_line_info() {
        let code: String = "x".repeat(5000);
        let f = Finding {
            id: "f1".to_string(),
            severity: Severity::Medium,
            cwe: None,
            file_path: None,
            line_range: None,
            description: "no lines".to_string(),
            adversary_reasoning: String::new(),
            status: FindingStatus::Genuine,
            iteration: 0,
        };
        let findings: Vec<&Finding> = vec![&f];
        let (view, truncated) = build_verification_code_view(&code, &findings, 1000);
        assert!(truncated);
        assert!(view.contains("truncated"));
    }
}
