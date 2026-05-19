//! API pipeline — builds requests, streams responses, and executes tools.
//!
//! Extracted from the `cmd_chat` function in `main.rs` to enable reuse
//! from both the rustyline REPL and the ratatui TUI.

use crate::memory::MemoryDb;
use crate::permissions::PermissionManager;
use crate::providers::{convert_messages_to_anthropic, convert_tools_to_anthropic, get_adapter};
use crate::proxy::{self, normalize_base_url};
use crate::session::TokenUsage;
use crate::tools::{self, AnthropicToolAccumulator, ToolCall, ToolCallAccumulator};
use crate::tui::events::{AppEvent, PermissionResponse};
use futures::StreamExt;
use serde_json::Value;
use std::sync::mpsc;
use std::sync::Arc;

/// Send an event to the TUI, logging and returning early if the channel is closed.
macro_rules! send_event {
    ($tx:expr, $event:expr) => {
        if $tx.send($event).is_err() {
            tracing::warn!("TUI channel closed, stopping pipeline");
            return Err("TUI channel closed".to_string());
        }
    };
}

/// Send an event to the TUI from a non-Result context (tool execution loop).
/// Returns from the enclosing function with current results if channel is dead.
macro_rules! send_event_or_break {
    ($tx:expr, $event:expr) => {
        if $tx.send($event).is_err() {
            tracing::warn!("TUI channel closed during tool execution");
            break;
        }
    };
}

/// Outcome of a single conversation turn (one API round-trip + tool execution).
#[derive(Debug)]
pub struct TurnResult {
    /// Full response text accumulated during streaming.
    pub content: String,
    /// Structured tool calls returned by the model.
    pub tool_calls: Vec<ToolCall>,
    /// Tool result messages to append to the conversation history.
    pub tool_results: Vec<Value>,
    /// Token usage observed from streaming events.
    pub usage: TokenUsage,
    /// Whether the model returned tool calls that need a follow-up API call.
    pub needs_followup: bool,
    /// Normalized finish reason surfaced to the caller, when the provider
    /// reports one. `None` for normal stop on streams that do not propagate
    /// a distinct termination cause through this layer.
    ///
    /// Values currently emitted by [`handle_google_response`] (crosslink #788):
    /// - `Some("safety_blocked")` — Gemini set `finishReason` to `SAFETY`,
    ///   `RECITATION`, or `BLOCKLIST`. Text may be empty; callers should
    ///   surface a user-visible error rather than treating this as a normal
    ///   empty completion.
    /// - `Some("length")` — `MAX_TOKENS` truncation.
    /// - `Some("stop")` — explicit `STOP` from the provider.
    /// - `Some(other)` — verbatim pass-through for unrecognized reasons.
    pub finish_reason: Option<String>,
}

// ─── Request building ───────────────────────────────────────────────────────

/// Build an Anthropic-format request body.
///
/// If `prompt_blocks` is provided, the system prompt is emitted as a
/// multi-block array for cache efficiency (stable prefix with
/// `cache_control`, dynamic suffix without).  Otherwise the system
/// prompt is extracted from `messages` as a single cached block.
#[must_use]
pub fn build_anthropic_request(
    model: &str,
    messages: &[Value],
    effort_level: &str,
    claude_code_token: Option<&str>,
    prompt_blocks: Option<&crate::prompt::SystemPromptBlocks>,
) -> Value {
    let anthropic_messages = convert_messages_to_anthropic(messages);
    let openai_tools = tools::get_all_tool_definitions(true);
    let anthropic_tools = convert_tools_to_anthropic(openai_tools.as_array().unwrap_or(&vec![]));

    let mut req = serde_json::json!({
        "model": model,
        "messages": anthropic_messages,
        "max_tokens": crate::DEFAULT_MAX_TOKENS,
        "stream": true,
        "tools": anthropic_tools
    });

    if let Some(blocks) = prompt_blocks {
        // Multi-block system prompt: stable prefix (cached) + dynamic suffix (not cached)
        req["system"] = crate::providers::build_system_blocks(blocks);
    } else {
        // Legacy single-block path: extract from messages
        let system_msg = messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
            .map(String::from);
        if let Some(sys) = system_msg {
            req["system"] = crate::providers::build_system_blocks_from_string(&sys);
        }
    }

    if claude_code_token.is_some() {
        crate::claude_credentials::inject_system_prompt(&mut req);
    }

    // Apply effort level. `high` / `max` switch the Anthropic thinking
    // budget to Claude Code's ULTRATHINK constant (31999); MAX_THINKING_TOKENS
    // env var overrides outright. See `crate::thinking` for the precedence
    // chain and keyword-trigger logic (ultrathink / think ultra hard).
    match effort_level {
        "high" | "max" => {
            if let Some(budget) = crate::thinking::anthropic_thinking_budget(Some(effort_level)) {
                req["thinking"] = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                });
                // Headroom for the thinking block plus the answer. Claude
                // Code uses max_tokens > budget_tokens; 40k covers 32k
                // thinking + ~8k answer comfortably.
                req["max_tokens"] = serde_json::json!(40_000);
            }
        }
        "low" => {
            req["max_tokens"] = serde_json::json!(2048);
        }
        _ => {} // medium = default
    }

    req
}

/// Build an OpenAI-compatible request body (used by `OpenAI`, `DeepSeek`, Qwen, Z.AI).
///
/// `effort_level` propagates as `reasoning_effort` for `high`/`max` to
/// unlock o1/o3 reasoning; `max` downgrades to `high` because the API
/// only accepts the level on a subset of models (matches Claude Code's
/// `modelSupportsMaxEffort` clamp).
#[must_use]
pub fn build_openai_request(model: &str, messages: &[Value], effort_level: &str) -> Value {
    let mut req = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": crate::DEFAULT_MAX_TOKENS,
        "stream": true,
        "tools": tools::get_all_tool_definitions(true)
    });
    if matches!(effort_level, "high" | "max") {
        req["reasoning_effort"] = serde_json::json!("high");
    }
    req
}

/// Build a Google Gemini-format request body.
#[must_use]
pub fn build_google_request(messages: &[Value], effort_level: &str) -> Value {
    static EMPTY_STR: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| serde_json::json!(""));
    static EMPTY_OBJ: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| serde_json::json!({}));
    let openai_tools = tools::get_all_tool_definitions(true);
    let tools_vec = openai_tools.as_array().cloned().unwrap_or_default();
    let functions: Vec<Value> = tools_vec
        .iter()
        .filter_map(|tool| {
            let func = tool.get("function")?;
            Some(serde_json::json!({
                "name": func.get("name")?,
                "description": func.get("description").unwrap_or_else(|| &*EMPTY_STR),
                "parameters": func.get("parameters").unwrap_or_else(|| &*EMPTY_OBJ)
            }))
        })
        .collect();

    let mut contents = Vec::new();
    let mut system_text: Option<String> = None;
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if role == "system" {
            system_text = Some(text.to_string());
            continue;
        }
        let gemini_role = if role == "assistant" { "model" } else { "user" };
        contents.push(serde_json::json!({
            "role": gemini_role,
            "parts": [{"text": text}]
        }));
    }

    // Gemini 2.5 takes `thinkingConfig.thinkingBudget` inside
    // generationConfig. When effort is high/max we hand it the Claude
    // Code ULTRATHINK constant, clamped to Gemini's 24k ceiling.
    let mut generation_config = serde_json::json!({"maxOutputTokens": 4096});
    if matches!(effort_level, "high" | "max") {
        const GEMINI_THINKING_CAP: u32 = 24_576;
        let budget = crate::thinking::anthropic_thinking_budget(Some(effort_level))
            .unwrap_or(crate::thinking::ULTRATHINK_BUDGET_TOKENS)
            .min(GEMINI_THINKING_CAP);
        generation_config["thinkingConfig"] = serde_json::json!({"thinkingBudget": budget});
    }
    let mut req = serde_json::json!({
        "contents": contents,
        "generationConfig": generation_config,
        "tools": [{"functionDeclarations": functions}]
    });
    if let Some(sys) = system_text {
        req["systemInstruction"] = serde_json::json!({"parts": [{"text": sys}]});
    }
    req
}

/// Build the appropriate request body for the given provider.
///
/// `prompt_blocks` is used only for the Anthropic path to enable multi-block
/// cache-efficient system prompts.  Pass `None` for the legacy single-block path.
#[must_use]
pub fn build_request(
    provider: &str,
    model: &str,
    messages: &[Value],
    effort_level: &str,
    claude_code_token: Option<&str>,
    prompt_blocks: Option<&crate::prompt::SystemPromptBlocks>,
) -> Value {
    // Resolve ultrathink keyword / env override against the base effort
    // so every provider path sees the same effective level (Claude Code
    // does the same in `resolveAppliedEffort`). If the env var is set
    // to `unset` / `auto` we drop to `medium` — keeping effort out of
    // the request body entirely isn't an option for OC's existing
    // string-typed signature, and `medium` is the API's no-op level.
    let resolved = crate::thinking::resolve_effort(effort_level, messages);
    let effective = resolved.as_deref().unwrap_or("medium");
    match provider {
        "anthropic" => {
            build_anthropic_request(model, messages, effective, claude_code_token, prompt_blocks)
        }
        "google" => build_google_request(messages, effective),
        _ => build_openai_request(model, messages, effective),
    }
}

/// Resolve the API endpoint for the given provider configuration.
#[must_use]
pub fn resolve_endpoint(
    provider: &str,
    model: &str,
    base_url: &str,
    claude_code_token: Option<&str>,
) -> String {
    if claude_code_token.is_some() {
        crate::claude_credentials::get_oauth_endpoint(model)
    } else {
        let adapter = get_adapter(provider);
        format!(
            "{}{}",
            normalize_base_url(base_url),
            adapter.chat_endpoint(model)
        )
    }
}

/// Build the headers needed for the API request.
///
/// `api_key` is `Option<&ApiKey>`: `None` is valid only when
/// `claude_code_token` is `Some(_)` (OAuth path doesn't need an API key).
/// If both are `None` the function returns an empty auth set — the caller
/// is expected to have validated the combination. See crosslink #256.
#[must_use]
pub fn resolve_headers(
    provider: &str,
    api_key: Option<&crate::providers::ApiKey>,
    claude_code_token: Option<&str>,
    extra_headers: &[(String, String)],
) -> Vec<(String, String)> {
    let mut headers = claude_code_token.map_or_else(
        || {
            api_key.map_or_else(Vec::new, |key| {
                let adapter = get_adapter(provider);
                adapter.get_headers(key)
            })
        },
        crate::claude_credentials::get_oauth_headers,
    );
    headers.extend(extra_headers.iter().cloned());
    headers
}

// ─── Streaming + tool execution ─────────────────────────────────────────────

/// Parameters for [`run_turn`]. Bundled to keep the call-site argument count
/// within clippy's `too_many_arguments` limit.
pub struct RunTurnParams<'a> {
    pub client: &'a reqwest::Client,
    pub endpoint: &'a str,
    pub headers: &'a [(String, String)],
    pub request_body: &'a Value,
    pub provider: &'a str,
    pub memory_db: Option<Arc<MemoryDb>>,
    pub permission_mgr: Option<Arc<PermissionManager>>,
    pub hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    pub session_id: Option<String>,
    pub tx: mpsc::Sender<AppEvent>,
}

/// Run one turn of the conversation: send request, stream response, execute tools.
///
/// Sends `AppEvent` variants through `tx` as they occur so the TUI can update
/// in real time. Returns a `TurnResult` describing what happened.
///
/// # Errors
///
/// Returns `Err` if the HTTP request itself fails (network error, etc.).
pub async fn run_turn(p: RunTurnParams<'_>) -> Result<TurnResult, String> {
    let RunTurnParams {
        client,
        endpoint,
        headers,
        request_body,
        provider,
        memory_db,
        permission_mgr,
        hook_engine,
        session_id,
        tx,
    } = p;
    tracing::info!(
        endpoint,
        model = request_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?"),
        system_blocks = request_body
            .get("system")
            .and_then(|v| v.as_array())
            .map_or(0, std::vec::Vec::len),
        messages = request_body
            .get("messages")
            .and_then(|v| v.as_array())
            .map_or(0, std::vec::Vec::len),
        has_tools = request_body
            .get("tools")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty()),
        "Sending API request"
    );

    // Send request with retry on transient errors (429, 529, 503)
    let max_retries = 3u32;
    let mut response = None;
    for attempt in 0..=max_retries {
        let mut req = client.post(endpoint).json(request_body);
        for (key, value) in headers {
            req = req.header(key, value);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("Request failed: {e}"))?;
        let status = resp.status().as_u16();

        if matches!(status, 429 | 503 | 529) && attempt < max_retries {
            let wait_secs = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(|| 2u64.pow(attempt + 1));
            tracing::warn!(status, attempt, wait_secs, "Transient API error, retrying");
            let _ = tx.send(AppEvent::StreamText(format!(
                "\n(Retrying in {wait_secs}s — {status}...)\n"
            )));
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
            continue;
        }

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("API error {status}: {body}"));
        }

        response = Some(resp);
        break;
    }
    let response = response.ok_or_else(|| "Max retries exceeded".to_string())?;

    // For Google, handle non-streaming JSON response
    if provider == "google" {
        return handle_google_response(
            response,
            memory_db,
            permission_mgr,
            hook_engine.clone(),
            session_id.clone(),
            &tx,
        )
        .await;
    }

    // Stream SSE response (Anthropic / OpenAI format)
    stream_sse_response(
        response,
        provider,
        memory_db,
        permission_mgr,
        hook_engine,
        session_id,
        &tx,
    )
    .await
}

/// Outcome of classifying the top-level `finishReason` from a Gemini
/// non-streaming response.
///
/// Pure data carrier produced by [`classify_google_finish_reason`] so the
/// mapping logic stays unit-testable in isolation from the channels and
/// HTTP plumbing in [`handle_google_response`]. See crosslink #788 for
/// the gap this addresses: the prior implementation silently dropped
/// `SAFETY` / `RECITATION` / `BLOCKLIST` and returned an empty
/// completion to the TUI with no signal whatsoever.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GoogleFinishClassification {
    /// Normalized finish reason to surface on `TurnResult.finish_reason`.
    pub finish_reason: Option<String>,
    /// When `Some`, a user-visible error message the caller must push
    /// onto the TUI via `AppEvent::ApiError`. Set for filtered output
    /// (`SAFETY` / `RECITATION` / `BLOCKLIST`); `None` otherwise.
    pub user_error: Option<String>,
}

/// Classify `candidates[0].finishReason` from a Gemini JSON response.
///
/// Maps Gemini's enum vocabulary to OC's normalized vocabulary:
/// - `SAFETY` / `RECITATION` / `BLOCKLIST` → `Some("safety_blocked")`
///   plus a user-facing error and a `tracing::warn!` log.
/// - `MAX_TOKENS` → `Some("length")` plus a `tracing::warn!` log.
/// - `STOP` → `Some("stop")`.
/// - Any other non-empty string → `Some(other)` (verbatim pass-through;
///   never classified as a safety block).
/// - Missing / non-string → `None`.
///
/// `text_len_bytes` is the length of the text body already extracted by
/// the caller; it is included in the warn log so operators can correlate
/// "blocked + had partial text" vs "blocked + empty completion".
#[must_use]
pub fn classify_google_finish_reason(
    gemini_json: &Value,
    text_len_bytes: usize,
) -> GoogleFinishClassification {
    let raw = gemini_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finishReason"))
        .and_then(|r| r.as_str());

    match raw {
        Some(reason @ ("SAFETY" | "RECITATION" | "BLOCKLIST")) => {
            tracing::warn!(
                finish_reason = reason,
                text_len = text_len_bytes,
                "Gemini suppressed candidate output (filtered response)"
            );
            GoogleFinishClassification {
                finish_reason: Some("safety_blocked".to_string()),
                user_error: Some(format!(
                    "Gemini blocked the response (finishReason={reason}). \
                     The model returned no usable content."
                )),
            }
        }
        Some("MAX_TOKENS") => {
            tracing::warn!(
                finish_reason = "MAX_TOKENS",
                text_len = text_len_bytes,
                "Gemini truncated response at max_tokens"
            );
            GoogleFinishClassification {
                finish_reason: Some("length".to_string()),
                user_error: None,
            }
        }
        Some("STOP") => GoogleFinishClassification {
            finish_reason: Some("stop".to_string()),
            user_error: None,
        },
        Some(other) => GoogleFinishClassification {
            // Unknown / future finish reasons: pass through verbatim so
            // the caller can decide. Do NOT classify these as safety
            // blocks — that would over-trigger user-visible errors on
            // benign new Gemini enum values.
            finish_reason: Some(other.to_string()),
            user_error: None,
        },
        None => GoogleFinishClassification::default(),
    }
}

/// Extract structured tool calls from a Gemini non-streaming response.
fn extract_google_tool_calls(gemini_json: &Value) -> Vec<ToolCall> {
    gemini_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| {
                    let fc = p.get("functionCall")?;
                    let name = fc.get("name")?.as_str()?.to_string();
                    let args = fc.get("args").map_or_else(
                        || "{}".to_string(),
                        |a| serde_json::to_string(a).unwrap_or_default(),
                    );
                    Some(ToolCall {
                        id: format!("call_{}", uuid::Uuid::new_v4()),
                        call_type: "function".to_string(),
                        function: tools::FunctionCall {
                            name,
                            arguments: args,
                        },
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Extract `(prompt_tokens, candidates_tokens)` from a Gemini response.
fn extract_google_usage(gemini_json: &Value) -> (u64, u64) {
    let usage = gemini_json.get("usageMetadata");
    let input = usage
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (input, output)
}

/// Handle a non-streaming Google Gemini response.
async fn handle_google_response(
    response: reqwest::Response,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    session_id: Option<String>,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<TurnResult, String> {
    let body = response.text().await.unwrap_or_default();
    let gemini_json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse Gemini response: {e}"))?;

    let text: String = gemini_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    // Check for Gemini error responses
    if let Some(error) = gemini_json.get("error") {
        let msg = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        let code = error
            .get("code")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        return Err(format!("Gemini API error ({code}): {msg}"));
    }

    // #788: surface Gemini SAFETY / RECITATION / BLOCKLIST blocks via the pure helper.
    let GoogleFinishClassification {
        finish_reason,
        user_error,
    } = classify_google_finish_reason(&gemini_json, text.len());
    if let Some(msg) = user_error {
        send_event!(tx, AppEvent::ApiError(msg));
    }

    if !text.is_empty() {
        send_event!(tx, AppEvent::StreamText(text.clone()));
    }

    let tool_calls = extract_google_tool_calls(&gemini_json);
    let (input_tokens, output_tokens) = extract_google_usage(&gemini_json);

    // Execute tool calls if any
    let (tool_results, needs_followup) = execute_tool_calls_for_tui(
        &tool_calls,
        memory_db,
        permission_mgr,
        hook_engine,
        session_id.as_deref(),
        tx,
    )
    .await;

    if !needs_followup {
        send_event!(tx, AppEvent::ResponseDone);
    }

    Ok(TurnResult {
        content: text,
        tool_calls,
        tool_results,
        usage: TokenUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        },
        needs_followup,
        finish_reason,
    })
}

/// Outcome of enforcing the per-line SSE buffer cap.
///
/// SSE frames are line-delimited. A hostile or broken upstream that
/// streams bytes without ever emitting `\n` would otherwise grow the
/// accumulator without bound until the process OOMs (crosslink #695).
/// This enum records the action taken by [`enforce_sse_line_cap`].
#[derive(Debug, PartialEq, Eq)]
pub enum SseLineCapOutcome {
    /// Buffer is within the cap; nothing was discarded.
    WithinCap,
    /// Buffer exceeded [`proxy::MAX_SSE_LINE_BYTES`] without a newline.
    /// The accumulator was reset; the caller should log a warning.
    /// Carries the number of bytes discarded for forensic reporting.
    Exceeded {
        /// Number of bytes dropped from the accumulator.
        discarded_bytes: usize,
    },
}

/// Enforce the per-line SSE buffer cap.
///
/// If `buffer` already contains a newline, the in-flight line is bounded
/// by the next `find('\n')` and we leave the accumulator untouched —
/// existing drain logic will consume it. Otherwise, if the unterminated
/// remainder has grown past [`proxy::MAX_SSE_LINE_BYTES`] we clear the
/// buffer and report the discard so the caller can warn.
///
/// Pure function — no I/O, no allocation when within cap, fully testable.
pub fn enforce_sse_line_cap(buffer: &mut String) -> SseLineCapOutcome {
    if buffer.contains('\n') {
        return SseLineCapOutcome::WithinCap;
    }
    if buffer.len() < proxy::MAX_SSE_LINE_BYTES {
        return SseLineCapOutcome::WithinCap;
    }
    let discarded_bytes = buffer.len();
    buffer.clear();
    SseLineCapOutcome::Exceeded { discarded_bytes }
}

/// Enforce the SSE line cap and forward an `ApiError` event on overflow.
///
/// Thin wrapper around [`enforce_sse_line_cap`] that handles the
/// side-effecting reporting path (tracing + channel emit). Keeps
/// the streaming loop body small enough to satisfy clippy's
/// `too_many_lines` ceiling.
///
/// Returns `Err` only when the channel is closed; otherwise `Ok(())`.
fn enforce_sse_line_cap_with_report(
    buffer: &mut String,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<(), String> {
    if let SseLineCapOutcome::Exceeded { discarded_bytes } = enforce_sse_line_cap(buffer) {
        let cap = proxy::MAX_SSE_LINE_BYTES;
        tracing::warn!(
            discarded_bytes,
            cap,
            "SSE line exceeded {cap} bytes without newline; resetting accumulator (crosslink #695)"
        );
        send_event!(
            tx,
            AppEvent::ApiError(format!(
                "SSE line exceeded {cap} bytes without newline; accumulator reset"
            ))
        );
    }
    Ok(())
}

/// Stream an SSE response (Anthropic or `OpenAI` format), sending events to the TUI.
async fn stream_sse_response(
    response: reqwest::Response,
    provider: &str,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    session_id: Option<String>,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<TurnResult, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut full_content = String::new();
    let mut tool_accumulator = ToolCallAccumulator::new();
    let mut anthropic_accumulator = AnthropicToolAccumulator::new();
    let mut stream_usage = TokenUsage::default();
    let mut in_thinking_block = false;
    let mut last_data_time = std::time::Instant::now();
    let stream_timeout = std::time::Duration::from_secs(proxy::SSE_STREAM_TIMEOUT_SECS);

    while let Some(chunk_result) = stream.next().await {
        // Check stream timeout
        if last_data_time.elapsed() > stream_timeout {
            if !full_content.is_empty() {
                full_content.push_str("\n\n[Response truncated: stream timeout]");
            }
            break;
        }

        match chunk_result {
            Ok(chunk) => {
                last_data_time = std::time::Instant::now();
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Crosslink #695: cap the per-line accumulator. A hostile
                // upstream that never emits `\n` would otherwise grow
                // `buffer` unboundedly until OOM.
                enforce_sse_line_cap_with_report(&mut buffer, tx)?;

                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            break;
                        }

                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                            // Extract usage BEFORE the accumulator (both can process the same event)
                            if let Some(usage) = proxy::extract_usage_from_sse_event(&json) {
                                stream_usage.accumulate(&usage);
                            }

                            match process_sse_event(
                                &json,
                                in_thinking_block,
                                &mut anthropic_accumulator,
                                &mut tool_accumulator,
                            ) {
                                SseAction::Text(text) => {
                                    send_event!(tx, AppEvent::StreamText(text.clone()));
                                    full_content.push_str(&text);
                                }
                                SseAction::Thinking(text) => {
                                    send_event!(tx, AppEvent::StreamThinking(text));
                                }
                                SseAction::ThinkingStart => {
                                    in_thinking_block = true;
                                    send_event!(
                                        tx,
                                        AppEvent::StreamThinking("[thinking...]\n".to_string(),)
                                    );
                                }
                                SseAction::ThinkingEnd => {
                                    in_thinking_block = false;
                                }
                                SseAction::None => {}
                            }
                        }
                    }
                }
            }
            Err(e) => {
                send_event!(tx, AppEvent::ApiError(format!("Stream error: {e}")));
                break;
            }
        }
    }

    // Determine tool calls from the appropriate accumulator
    let tool_calls = if provider == "anthropic" && anthropic_accumulator.has_tool_use() {
        anthropic_accumulator.finalize_tool_calls()
    } else if tool_accumulator.has_tool_calls() {
        tool_accumulator.finalize()
    } else {
        vec![]
    };

    // Execute tool calls if any
    let (tool_results, has_tools) = execute_tool_calls_for_tui(
        &tool_calls,
        memory_db,
        permission_mgr,
        hook_engine,
        session_id.as_deref(),
        tx,
    )
    .await;

    // Only send ResponseDone if there are NO tool calls needing followup.
    // When there are tool calls, the caller (app.rs agentic loop) handles
    // the followup requests and sends ResponseDone when truly finished.
    if !has_tools {
        send_event!(tx, AppEvent::ResponseDone);
    }

    Ok(TurnResult {
        content: full_content,
        tool_calls,
        tool_results,
        usage: stream_usage,
        needs_followup: has_tools,
        // The SSE accumulators expose stop_reason internally but this
        // layer does not currently surface it. Anthropic / OpenAI
        // streams report `None`; only the Google JSON path populates
        // this field today (crosslink #788).
        finish_reason: None,
    })
}

/// Result of processing a single SSE event — testable without channels.
#[derive(Debug)]
pub enum SseAction {
    /// Emit text to the streaming output
    Text(String),
    /// Emit thinking text
    Thinking(String),
    /// Start a thinking block
    ThinkingStart,
    /// End a thinking block
    ThinkingEnd,
    /// No action needed (event consumed internally by accumulators)
    None,
}

/// Process a single SSE JSON event and return the action to take.
/// Pure function — no channels, no I/O, fully testable.
#[must_use]
pub fn process_sse_event(
    json: &Value,
    in_thinking_block: bool,
    anthropic_accumulator: &mut AnthropicToolAccumulator,
    tool_accumulator: &mut ToolCallAccumulator,
) -> SseAction {
    // Note: usage extraction is handled by the caller after the accumulator
    // processes the event. We used to return SseAction::Usage here, but that
    // caused the accumulator to miss events like message_start and message_delta
    // which contain both usage AND tool call state (stop_reason: "tool_use").

    // Thinking block detection (Anthropic)
    if let Some(event_type) = json.get("type").and_then(|t| t.as_str()) {
        if event_type == "content_block_start"
            && json
                .get("content_block")
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("thinking")
        {
            return SseAction::ThinkingStart;
        }
        if event_type == "content_block_stop" && in_thinking_block {
            return SseAction::ThinkingEnd;
        }
        if event_type == "content_block_delta" && in_thinking_block {
            if let Some(text) = json
                .get("delta")
                .and_then(|d| d.get("thinking"))
                .and_then(|t| t.as_str())
            {
                return SseAction::Thinking(text.to_string());
            }
            if let Some(text) = json
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
            {
                return SseAction::Thinking(text.to_string());
            }
        }
    }

    // Anthropic format: process through accumulator
    if let Some(text) = anthropic_accumulator.process_event(json) {
        return SseAction::Text(text);
    }

    // OpenAI format: choices[0].delta.content
    if let Some(delta) = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
    {
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            return SseAction::Text(content.to_string());
        }
        tool_accumulator.process_delta(delta);
    }

    SseAction::None
}

/// Tools that are safe to execute without permission (read-only / informational).
const SAFE_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "grep",
    "glob",
    "web_fetch",
    "web_search",
    "ask_user_question",
    "todo_read",
    "task",
    "agent_output",
    "enter_plan_mode",
    "exit_plan_mode",
    "lsp",
    "memory_search",
    "core_memory_get",
    "chainlink",
];

/// Check if a tool needs permission before execution.
#[must_use]
pub fn tool_needs_permission(tool_name: &str) -> bool {
    !SAFE_TOOLS.contains(&tool_name)
}

/// Execute tool calls and send progress events to the TUI.
///
/// Each tool runs on a blocking thread via `spawn_blocking` so the async
/// event channel stays responsive — the TUI can redraw and show progress
/// while tools execute.
///
/// Outcome of a TUI permission check for a single tool call.
enum PermissionOutcome {
    /// The tool is allowed to proceed.
    Allowed,
    /// The tool was denied; the caller should push `result_json` and `continue`.
    DeniedWithResult(serde_json::Value),
    /// The permission channel is broken; the caller should `break`.
    ChannelBroken,
}

/// Check whether a tool call is permitted in the current session.
///
/// Consults `always_allowed`/`always_denied` session caches (batch-scoped),
/// then the `PermissionManager`'s session-scoped TUI always-allow/deny cache
/// (crosslink #724 — persists across batches for the lifetime of the
/// manager), then sends a `PermissionRequest` event and blocks for the
/// user's decision if neither cache matches.
fn check_tool_permission(
    tool_name: &str,
    tool_call_id: &str,
    arguments: &str,
    always_allowed: &mut std::collections::HashSet<String>,
    always_denied: &mut std::collections::HashSet<String>,
    permission_mgr: Option<&PermissionManager>,
    tx: &mpsc::Sender<AppEvent>,
) -> PermissionOutcome {
    // Batch-scoped cache (this invocation of execute_tool_calls_for_tui).
    if always_denied.contains(tool_name) {
        let _ = tx.send(AppEvent::ToolDone {
            name: tool_name.to_string(),
            success: false,
            content: "Denied (always deny for this session)".to_string(),
        });
        return PermissionOutcome::DeniedWithResult(
            serde_json::json!({ "role": "tool", "tool_call_id": tool_call_id, "content": "[DENIED] User denied permission for this tool.", "is_error": true }),
        );
    }
    if always_allowed.contains(tool_name) {
        return PermissionOutcome::Allowed;
    }
    // Session-scoped cache (crosslink #724 — survives across batches).
    if let Some(mgr) = permission_mgr {
        if mgr.tui_is_always_denied(tool_name) {
            let _ = tx.send(AppEvent::ToolDone {
                name: tool_name.to_string(),
                success: false,
                content: "Denied (always deny for this session)".to_string(),
            });
            return PermissionOutcome::DeniedWithResult(
                serde_json::json!({ "role": "tool", "tool_call_id": tool_call_id, "content": "[DENIED] User denied permission for this tool.", "is_error": true }),
            );
        }
        if mgr.tui_is_always_allowed(tool_name) {
            return PermissionOutcome::Allowed;
        }
    }
    let args_preview = if arguments.len() > 200 {
        format!("{}...", crate::tools::safe_truncate(arguments, 197))
    } else {
        arguments.to_string()
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(AppEvent::PermissionRequest {
            tool_name: tool_name.to_string(),
            tool_args: args_preview,
            reply: reply_tx,
        })
        .is_err()
    {
        return PermissionOutcome::ChannelBroken;
    }
    match reply_rx.recv() {
        Ok(PermissionResponse::Allow) => PermissionOutcome::Allowed,
        Ok(PermissionResponse::AlwaysAllow) => {
            always_allowed.insert(tool_name.to_string());
            // Persist for the rest of the session (crosslink #724).
            if let Some(mgr) = permission_mgr {
                mgr.tui_remember_always_allowed(tool_name.to_string());
            }
            PermissionOutcome::Allowed
        }
        Ok(PermissionResponse::AlwaysDeny) => {
            always_denied.insert(tool_name.to_string());
            // Persist for the rest of the session (crosslink #724).
            if let Some(mgr) = permission_mgr {
                mgr.tui_remember_always_denied(tool_name.to_string());
            }
            let _ = tx.send(AppEvent::ToolDone {
                name: tool_name.to_string(),
                success: false,
                content: "Denied (always deny)".to_string(),
            });
            PermissionOutcome::DeniedWithResult(
                serde_json::json!({ "role": "tool", "tool_call_id": tool_call_id, "content": "[DENIED] User denied permission.", "is_error": true }),
            )
        }
        Ok(PermissionResponse::Deny) | Err(_) => {
            let _ = tx.send(AppEvent::ToolDone {
                name: tool_name.to_string(),
                success: false,
                content: "Denied by user".to_string(),
            });
            PermissionOutcome::DeniedWithResult(
                serde_json::json!({ "role": "tool", "tool_call_id": tool_call_id, "content": "[DENIED] User denied permission.", "is_error": true }),
            )
        }
    }
}

/// Execute one tool call on a blocking thread, fire `PostToolUse` hooks, and
/// return the JSON result to append to conversation history.
/// Returns `None` when the event channel is broken (caller should `break`).
async fn execute_single_tool(
    tool_call: &ToolCall,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    session_id: Option<&str>,
    hook_engine: Option<&crate::hooks::HookEngine>,
    tx: &mpsc::Sender<AppEvent>,
) -> Option<Value> {
    let tool_name = &tool_call.function.name;
    let tool_call_clone = tool_call.clone();
    let mem_db = memory_db;
    let perm_mgr = permission_mgr;
    let session_for_task = session_id.map(str::to_string);
    let result = tokio::task::spawn_blocking(move || {
        let _session_guard = session_for_task.map(tools::SessionIdGuard::set);
        tools::execute_tool_with_memory(&tool_call_clone, mem_db.as_deref(), perm_mgr.as_deref())
    })
    .await
    .unwrap_or_else(|e| tools::ToolResult {
        tool_call_id: tool_call.id.clone(),
        content: format!("Tool execution panicked: {e}"),
        is_error: true,
    });

    if tx
        .send(AppEvent::ToolDone {
            name: tool_name.clone(),
            success: !result.is_error,
            content: result.content.clone(),
        })
        .is_err()
    {
        return None;
    }
    if let Some(engine) = hook_engine {
        let tool_input: Value =
            serde_json::from_str(&tool_call.function.arguments).unwrap_or(Value::Null);
        engine
            .fire_post_tool(
                !result.is_error,
                tool_name,
                tool_input,
                &result.content,
                session_id,
            )
            .await;
    }
    let result_content = if result.is_error {
        format!("[ERROR] {}", result.content)
    } else {
        result.content
    };
    Some(
        serde_json::json!({ "role": "tool", "tool_call_id": result.tool_call_id, "content": result_content, "is_error": result.is_error }),
    )
}

/// Build a human-readable one-line description of what a tool call will do.
fn describe_tool_call(tool_name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    match tool_name {
        "read_file" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map_or_else(|| "Reading file".to_string(), |p| format!("Reading {p}")),
        "write_file" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map_or_else(|| "Writing file".to_string(), |p| format!("Writing {p}")),
        "edit_file" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map_or_else(|| "Editing file".to_string(), |p| format!("Editing {p}")),
        "bash" => args.get("command").and_then(|v| v.as_str()).map_or_else(
            || "Running command".to_string(),
            |c| {
                let truncated = if c.len() > 80 {
                    crate::tools::safe_truncate(c, 77)
                } else {
                    c
                };
                format!("$ {truncated}")
            },
        ),
        "list_files" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map_or_else(|| "Listing files".to_string(), |p| format!("Listing {p}")),
        "web_search" => args.get("query").and_then(|v| v.as_str()).map_or_else(
            || "Searching web".to_string(),
            |q| format!("Searching: {q}"),
        ),
        "web_fetch" => args
            .get("url")
            .and_then(|v| v.as_str())
            .map_or_else(|| "Fetching URL".to_string(), |u| format!("Fetching {u}")),
        "chainlink" => args.get("args").and_then(|v| v.as_str()).map_or_else(
            || "Running crosslink".to_string(),
            |a| format!("crosslink {a}"),
        ),
        _ => format!("Running {tool_name}"),
    }
}

/// Checks permissions for write/destructive tools via a channel-based
/// handshake: sends `PermissionRequest` to the TUI and blocks until
/// the user responds with y/n/a/d.
///
/// Returns the tool result messages (for appending to conversation history)
/// and a boolean indicating whether there were any tool calls.
async fn execute_tool_calls_for_tui(
    tool_calls: &[ToolCall],
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    session_id: Option<&str>,
    tx: &mpsc::Sender<AppEvent>,
) -> (Vec<Value>, bool) {
    // Session-level "always allow/deny" cache (lives for this agentic loop)
    let mut always_allowed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut always_denied: std::collections::HashSet<String> = std::collections::HashSet::new();
    if tool_calls.is_empty() {
        return (vec![], false);
    }

    let mut results = Vec::new();

    for tool_call in tool_calls {
        let tool_name = &tool_call.function.name;

        // Check blast radius guardrails
        if let Err(msg) = crate::guardrails::check_file_access(&tool_call.function.arguments) {
            send_event_or_break!(
                tx,
                AppEvent::ToolDone {
                    name: tool_name.clone(),
                    success: false,
                    content: format!("Blocked by guardrails: {msg}"),
                }
            );
            results.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call.id,
                "content": format!("[BLOCKED] {msg}"),
                "is_error": true
            }));
            continue;
        }

        // Permission check for write/destructive tools
        if tool_needs_permission(tool_name) {
            match check_tool_permission(
                tool_name,
                &tool_call.id,
                &tool_call.function.arguments,
                &mut always_allowed,
                &mut always_denied,
                permission_mgr.as_deref(),
                tx,
            ) {
                PermissionOutcome::Allowed => {}
                PermissionOutcome::DeniedWithResult(result_json) => {
                    results.push(result_json);
                    continue;
                }
                PermissionOutcome::ChannelBroken => break,
            }
        }

        let args_desc = describe_tool_call(tool_name, &tool_call.function.arguments);
        send_event_or_break!(
            tx,
            AppEvent::ToolStart {
                name: tool_name.clone(),
                description: args_desc
            }
        );

        let tool_result = execute_single_tool(
            tool_call,
            memory_db.clone(),
            permission_mgr.clone(),
            session_id,
            hook_engine.as_ref().map(Arc::as_ref),
            tx,
        )
        .await;
        match tool_result {
            None => break, // channel broken
            Some(result_json) => results.push(result_json),
        }
    }

    // Run quality gates after tool execution
    let gates = crate::guardrails::run_quality_gates();
    for gate in &gates {
        if !gate.passed {
            send_event_or_break!(
                tx,
                AppEvent::StreamText(format!(
                    "\n⚠ Quality gate '{}': {}\n",
                    gate.name,
                    gate.stdout.lines().next().unwrap_or("failed")
                ))
            );
        }
    }

    (results, true)
}

/// Build the assistant message with tool calls for appending to conversation history.
#[must_use]
pub fn build_assistant_message_with_tools(
    content: &str,
    tool_calls: &[ToolCall],
    _provider: &str,
) -> Value {
    let tool_calls_json: Vec<Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "type": tc.call_type,
                "function": {
                    "name": tc.function.name,
                    "arguments": tc.function.arguments
                }
            })
        })
        .collect();

    serde_json::json!({
        "role": "assistant",
        "content": Value::String(content.to_string()),
        "tool_calls": tool_calls_json
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_openai_request() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
        let req = build_openai_request("gpt-4", &messages, "medium");
        assert_eq!(req["model"], "gpt-4");
        assert_eq!(req["stream"], true);
        assert!(req["tools"].is_array());
        assert!(req.get("reasoning_effort").is_none());

        let high = build_openai_request("gpt-4", &messages, "high");
        assert_eq!(high["reasoning_effort"], "high");
    }

    #[test]
    fn test_build_anthropic_request_legacy_single_block() {
        let messages = vec![
            serde_json::json!({"role": "system", "content": "You are helpful."}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ];
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None);
        assert_eq!(req["model"], "claude-sonnet-4-6");
        assert!(req["system"].is_array());
        // Legacy path: single block with cache_control
        assert_eq!(req["system"].as_array().unwrap().len(), 1);
        assert!(req["system"][0]["cache_control"].is_object());
        assert!(req["tools"].is_array());
    }

    #[test]
    fn test_build_anthropic_request_multi_block() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
        let blocks = crate::prompt::SystemPromptBlocks {
            stable_prefix: "identity and tools".to_string(),
            dynamic_suffix: "hooks and env".to_string(),
        };
        let req = build_anthropic_request(
            "claude-sonnet-4-6",
            &messages,
            "medium",
            None,
            Some(&blocks),
        );
        assert_eq!(req["model"], "claude-sonnet-4-6");
        let sys = req["system"].as_array().unwrap();
        // Two blocks: prefix (cached) + suffix (not cached)
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], "identity and tools");
        assert!(
            sys[0]["cache_control"].is_object(),
            "prefix must have cache_control"
        );
        assert_eq!(sys[1]["text"], "hooks and env");
        assert!(
            sys[1].get("cache_control").is_none(),
            "suffix must NOT have cache_control"
        );
    }

    #[test]
    fn test_build_anthropic_request_empty_suffix_single_block() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
        let blocks = crate::prompt::SystemPromptBlocks {
            stable_prefix: "everything is static".to_string(),
            dynamic_suffix: String::new(),
        };
        let req = build_anthropic_request(
            "claude-sonnet-4-6",
            &messages,
            "medium",
            None,
            Some(&blocks),
        );
        let sys = req["system"].as_array().unwrap();
        // Empty suffix collapses to single cached block
        assert_eq!(sys.len(), 1);
        assert!(sys[0]["cache_control"].is_object());
    }

    #[test]
    fn test_build_request_dispatches() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let req = build_request("openai", "gpt-4", &messages, "medium", None, None);
        assert_eq!(req["model"], "gpt-4");

        let req = build_request(
            "anthropic",
            "claude-sonnet-4-6",
            &messages,
            "medium",
            None,
            None,
        );
        assert_eq!(req["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn test_build_assistant_message_with_tools() {
        let tool_calls = vec![ToolCall {
            id: "call_123".to_string(),
            call_type: "function".to_string(),
            function: tools::FunctionCall {
                name: "bash".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            },
        }];
        let msg = build_assistant_message_with_tools("hello", &tool_calls, "anthropic");
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "hello");
        assert!(msg["tool_calls"].is_array());
        assert_eq!(msg["tool_calls"][0]["id"], "call_123");
    }

    #[test]
    fn test_effort_levels() {
        // Tests read env vars — guard against interference from the ambient
        // MAX_THINKING_TOKENS override.
        // SAFETY: no other test in this module mutates MAX_THINKING_TOKENS.
        let prev = std::env::var("MAX_THINKING_TOKENS").ok();
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
        }
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];

        let high = build_anthropic_request("claude-sonnet-4-6", &messages, "high", None, None);
        assert_eq!(
            high["thinking"]["budget_tokens"],
            crate::thinking::ULTRATHINK_BUDGET_TOKENS,
        );
        assert_eq!(high["max_tokens"], 40_000);

        let maxr = build_anthropic_request("claude-sonnet-4-6", &messages, "max", None, None);
        assert_eq!(
            maxr["thinking"]["budget_tokens"],
            crate::thinking::ULTRATHINK_BUDGET_TOKENS,
        );

        let low = build_anthropic_request("claude-sonnet-4-6", &messages, "low", None, None);
        assert!(low.get("thinking").is_none());
        assert_eq!(low["max_tokens"], 2048);

        let med = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None);
        assert!(med.get("thinking").is_none());
        assert_eq!(med["max_tokens"], crate::DEFAULT_MAX_TOKENS);
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("MAX_THINKING_TOKENS", v);
            }
        }
    }

    // ── Phase 2 spec-pinning tests (#552 / spec #537) ────────────────────────

    /// B2 — medium effort DOES NOT attach thinking parameters.
    ///
    /// CURRENT CONTRACT: OC only enables thinking for "high"/"max".
    /// Gap #599 tracks enabling adaptive thinking by default (CC behaviour).
    #[test]
    fn b2_medium_effort_no_thinking_pin_gap_599() {
        let prev = std::env::var("MAX_THINKING_TOKENS").ok();
        // SAFETY: single-threaded test, no concurrent writers.
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
        }
        let messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None);
        // OC does NOT enable thinking for medium — gap #599: CC uses adaptive thinking
        assert!(
            req.get("thinking").is_none(),
            "medium effort must not attach thinking block (gap #599 tracks adaptive default)"
        );
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("MAX_THINKING_TOKENS", v);
            }
        }
    }

    /// B2 — high effort attaches `thinking.type = "enabled"` with budget > 0.
    ///
    /// Pins the exact budget constant (31999 = CC's `ULTRATHINK_BUDGET_TOKENS`).
    #[test]
    fn b2_high_effort_attaches_thinking_budget() {
        let prev = std::env::var("MAX_THINKING_TOKENS").ok();
        // SAFETY: single-threaded test, no concurrent writers.
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
        }
        let messages = vec![serde_json::json!({"role": "user", "content": "think"})];
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "high", None, None);
        assert_eq!(
            req["thinking"]["type"], "enabled",
            "high effort must set thinking.type = enabled"
        );
        // Budget must be CC's ULTRATHINK constant (31999)
        let budget = req["thinking"]["budget_tokens"].as_u64().unwrap_or(0);
        assert_eq!(
            budget,
            u64::from(crate::thinking::ULTRATHINK_BUDGET_TOKENS),
            "budget_tokens must equal ULTRATHINK_BUDGET_TOKENS"
        );
        // max_tokens must exceed budget_tokens (OC uses 40000)
        let max = req["max_tokens"].as_u64().unwrap_or(0);
        assert!(
            max > budget,
            "max_tokens ({max}) must be > budget_tokens ({budget})"
        );
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("MAX_THINKING_TOKENS", v);
            }
        }
    }

    /// B2 — Google request attaches `thinkingConfig.thinkingBudget` for high effort.
    ///
    /// Gemini 2.5 thinking is capped at 24576.
    #[test]
    fn b2_google_request_thinking_budget_capped() {
        const GEMINI_CAP: u64 = 24_576;
        let prev = std::env::var("MAX_THINKING_TOKENS").ok();
        // SAFETY: single-threaded test, no concurrent writers.
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
        }
        let messages = vec![serde_json::json!({"role": "user", "content": "think"})];
        let req = build_google_request(&messages, "high");
        let budget = req["generationConfig"]["thinkingConfig"]["thinkingBudget"]
            .as_u64()
            .unwrap_or(0);
        assert!(budget > 0, "high effort must set thinkingBudget > 0");
        assert!(
            budget <= GEMINI_CAP,
            "thinkingBudget ({budget}) must not exceed Gemini cap ({GEMINI_CAP})"
        );
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("MAX_THINKING_TOKENS", v);
            }
        }
    }

    /// B5 — `TurnResult.needs_followup` is `true` iff tool calls were accumulated.
    ///
    /// Pure-logic check via `process_sse_event` + `AnthropicToolAccumulator`.
    /// The `needs_followup` field drives whether the caller re-enters the agentic loop.
    #[test]
    fn b5_needs_followup_reflects_tool_accumulator_state() {
        let mut ant = tools::AnthropicToolAccumulator::new();
        let mut oai = tools::ToolCallAccumulator::new();

        // No tool events → no tool use
        let no_tool: serde_json::Value = serde_json::json!({
            "type": "content_block_start",
            "content_block": { "type": "text" }
        });
        let _ = process_sse_event(&no_tool, false, &mut ant, &mut oai);
        // simulate stop with end_turn
        let end_event: serde_json::Value = serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" }
        });
        let _ = process_sse_event(&end_event, false, &mut ant, &mut oai);
        assert!(
            !ant.has_tool_use(),
            "no tool blocks → needs_followup must be false"
        );

        // Now simulate a tool_use block
        let mut ant2 = tools::AnthropicToolAccumulator::new();
        let mut oai2 = tools::ToolCallAccumulator::new();
        for raw in &[
            r#"{"type":"content_block_start","content_block":{"type":"tool_use","id":"c1","name":"bash"}}"#,
            r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ] {
            let ev: serde_json::Value = serde_json::from_str(raw).unwrap();
            let _ = process_sse_event(&ev, false, &mut ant2, &mut oai2);
        }
        assert!(
            ant2.has_tool_use(),
            "tool_use stop_reason → needs_followup must be true"
        );
    }

    /// B6 — `SSE_STREAM_TIMEOUT_SECS` is pinned at 30 seconds.
    ///
    /// Increasing this without a gap issue would silently change user-visible
    /// latency characteristics. Stream timeout appends inline text (gap #600
    /// tracks upgrading to a structured error event like CC does).
    #[test]
    fn b6_stream_timeout_constant_is_30s() {
        // Pin current value — gap #600 tracks upgrading to structured error
        assert_eq!(
            crate::proxy::SSE_STREAM_TIMEOUT_SECS,
            30,
            "SSE_STREAM_TIMEOUT_SECS must stay at 30s until gap #600 is addressed"
        );
    }

    /// B1 — retry loop only covers 429, 503, 529 (NOT 408 — gap #597).
    ///
    /// This tests the request-builder side: a 200 response contains `stream: true`.
    #[test]
    fn b1_build_request_stream_flag_always_set() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let req = build_openai_request("gpt-4", &messages, "medium");
        assert_eq!(
            req["stream"], true,
            "stream must always be true in OC requests"
        );
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None);
        assert_eq!(req["stream"], true);
        let req = build_google_request(&messages, "medium");
        // Google request body doesn't include "stream" — it's a separate code path
        // The absence is the contract (Gemini uses non-streaming JSON — gap #602)
        assert!(
            req.get("stream").is_none(),
            "Google request must NOT have stream field (non-streaming path — gap #602)"
        );
    }

    /// B3 — `process_sse_event` returns `SseAction::None` for unknown event types.
    #[test]
    fn b3_process_sse_event_unknown_type_returns_none() {
        let event: serde_json::Value = serde_json::json!({"type": "ping"});
        let mut ant = tools::AnthropicToolAccumulator::new();
        let mut oai = tools::ToolCallAccumulator::new();
        let action = process_sse_event(&event, false, &mut ant, &mut oai);
        assert!(
            matches!(action, SseAction::None),
            "unknown SSE event type must return SseAction::None"
        );
    }

    /// B3 — `tool_needs_permission` classifies read-only tools as safe.
    #[test]
    fn b3_tool_needs_permission_safe_list() {
        assert!(!tool_needs_permission("read_file"), "read_file is safe");
        assert!(!tool_needs_permission("list_files"), "list_files is safe");
        assert!(!tool_needs_permission("grep"), "grep is safe");
        assert!(
            tool_needs_permission("write_file"),
            "write_file needs permission"
        );
        assert!(tool_needs_permission("bash"), "bash needs permission");
        assert!(
            tool_needs_permission("edit_file"),
            "edit_file needs permission"
        );
    }

    /// crosslink #724 — `check_tool_permission` consults the
    /// `PermissionManager`'s session-scoped TUI cache and short-circuits to
    /// `Allowed` without sending a `PermissionRequest` event. This is the
    /// integration test that proves the cache survives across batches: a
    /// fresh `execute_tool_calls_for_tui` invocation would see this state.
    #[test]
    fn issue_724_check_tool_permission_uses_session_always_allowed() {
        use std::sync::mpsc as std_mpsc;

        let mgr = PermissionManager::unrestricted();
        // Simulate: in a prior batch, the user picked "Always allow" for Bash.
        mgr.tui_remember_always_allowed("bash".to_string());

        // Batch-scoped caches start empty (as they would on every new batch).
        let mut always_allowed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut always_denied: std::collections::HashSet<String> = std::collections::HashSet::new();

        let (tx, rx) = std_mpsc::channel::<AppEvent>();
        let outcome = check_tool_permission(
            "bash",
            "call_1",
            "{\"command\":\"ls\"}",
            &mut always_allowed,
            &mut always_denied,
            Some(&mgr),
            &tx,
        );
        assert!(
            matches!(outcome, PermissionOutcome::Allowed),
            "#724: a prior 'always allow' must short-circuit to Allowed without a prompt"
        );
        // No PermissionRequest event should have been emitted.
        assert!(
            rx.try_recv().is_err(),
            "#724: no PermissionRequest event must be sent when the session cache allows"
        );
    }

    /// crosslink #724 — symmetric to the above: a session-scoped "always deny"
    /// short-circuits to `DeniedWithResult` without prompting the user again.
    #[test]
    fn issue_724_check_tool_permission_uses_session_always_denied() {
        use std::sync::mpsc as std_mpsc;

        let mgr = PermissionManager::unrestricted();
        mgr.tui_remember_always_denied("bash".to_string());

        let mut always_allowed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut always_denied: std::collections::HashSet<String> = std::collections::HashSet::new();

        let (tx, rx) = std_mpsc::channel::<AppEvent>();
        let outcome = check_tool_permission(
            "bash",
            "call_1",
            "{\"command\":\"rm -rf /\"}",
            &mut always_allowed,
            &mut always_denied,
            Some(&mgr),
            &tx,
        );
        assert!(
            matches!(outcome, PermissionOutcome::DeniedWithResult(_)),
            "#724: a prior 'always deny' must short-circuit to Denied without a prompt"
        );
        // A ToolDone event is emitted to inform the TUI, but NOT a PermissionRequest.
        let mut saw_perm_request = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AppEvent::PermissionRequest { .. }) {
                saw_perm_request = true;
            }
        }
        assert!(
            !saw_perm_request,
            "#724: no PermissionRequest event must be sent when the session cache denies"
        );
    }

    #[test]
    fn ultrathink_keyword_promotes_anthropic_thinking() {
        let prev = (
            std::env::var("MAX_THINKING_TOKENS").ok(),
            std::env::var("CLAUDE_CODE_EFFORT_LEVEL").ok(),
        );
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
            std::env::remove_var("CLAUDE_CODE_EFFORT_LEVEL");
        }
        let messages = vec![serde_json::json!({
            "role": "user",
            "content": "ultrathink and plan this out"
        })];
        // Base effort is medium — dispatcher should see the keyword and
        // bump to high, attaching the ULTRATHINK budget.
        let req = build_request(
            "anthropic",
            "claude-sonnet-4-6",
            &messages,
            "medium",
            None,
            None,
        );
        assert_eq!(
            req["thinking"]["budget_tokens"],
            crate::thinking::ULTRATHINK_BUDGET_TOKENS,
        );
        if let Some(v) = prev.0 {
            unsafe {
                std::env::set_var("MAX_THINKING_TOKENS", v);
            }
        }
        if let Some(v) = prev.1 {
            unsafe {
                std::env::set_var("CLAUDE_CODE_EFFORT_LEVEL", v);
            }
        }
    }

    // ─── Crosslink #695: SSE line-cap forensic evidence ──────────────────
    //
    // The SSE reader in `stream_sse_response` previously accumulated upstream
    // bytes into an unbounded `String` until a `\n` was found. A hostile or
    // broken upstream that streams payloads without newlines could grow the
    // accumulator until OOM. `enforce_sse_line_cap` is the pure-function
    // guard that backs the fix; these tests pin its contract.

    /// #695 — `MAX_SSE_LINE_BYTES` constant is pinned at 1 MiB.
    ///
    /// Raising this without an explicit gap issue weakens the OOM defense.
    /// Lowering it could split legitimately-long SSE frames.
    #[test]
    fn issue_695_max_sse_line_bytes_constant_is_1mib() {
        assert_eq!(
            crate::proxy::MAX_SSE_LINE_BYTES,
            1024 * 1024,
            "MAX_SSE_LINE_BYTES must remain at 1 MiB until a gap issue revises it"
        );
    }

    /// #695 — small newline-free buffer stays untouched (no false trip).
    ///
    /// A partial frame mid-flight is normal: the accumulator must hold
    /// pending bytes until the terminator arrives.
    #[test]
    fn issue_695_enforce_sse_line_cap_small_buffer_is_no_op() {
        let mut buffer = "data: {\"partial\":\"frame".to_string();
        let original_len = buffer.len();
        let outcome = enforce_sse_line_cap(&mut buffer);
        assert_eq!(outcome, SseLineCapOutcome::WithinCap);
        assert_eq!(
            buffer.len(),
            original_len,
            "within-cap buffer must not be mutated"
        );
    }

    /// #695 — the buffer is bounded against an unbounded newline-free
    /// upstream simulation.
    ///
    /// Forensic invariant: no matter how many chunks the helper sees,
    /// the buffer size after enforcement never exceeds `MAX_SSE_LINE_BYTES`.
    /// This mirrors the OOM attack scenario described in the issue.
    #[test]
    fn issue_695_enforce_sse_line_cap_bounds_unbounded_input() {
        let mut buffer = String::new();
        // Simulate 8 chunks of 256 KiB of newline-free bytes — together
        // 2 MiB, double the cap.
        let chunk = "A".repeat(256 * 1024);
        let mut total_discarded = 0usize;
        let mut times_tripped = 0usize;
        for _ in 0..8 {
            buffer.push_str(&chunk);
            match enforce_sse_line_cap(&mut buffer) {
                SseLineCapOutcome::WithinCap => {}
                SseLineCapOutcome::Exceeded { discarded_bytes } => {
                    total_discarded += discarded_bytes;
                    times_tripped += 1;
                    // The cap MUST have reset the buffer.
                    assert!(
                        buffer.is_empty(),
                        "Exceeded outcome must leave the buffer empty (was {} bytes)",
                        buffer.len()
                    );
                }
            }
            // After every iteration the live buffer must respect the cap.
            assert!(
                buffer.len() < crate::proxy::MAX_SSE_LINE_BYTES,
                "buffer.len() = {} must stay below MAX_SSE_LINE_BYTES = {}",
                buffer.len(),
                crate::proxy::MAX_SSE_LINE_BYTES
            );
        }
        assert!(
            times_tripped >= 1,
            "2 MiB of newline-free input must trip the cap at least once (tripped {times_tripped} times)"
        );
        let cap = crate::proxy::MAX_SSE_LINE_BYTES;
        assert!(
            total_discarded >= cap,
            "expected at least {cap} bytes discarded in aggregate, got {total_discarded}"
        );
    }

    /// #695 — when a buffer contains a newline the cap MUST NOT fire,
    /// even if total length exceeds the cap.
    ///
    /// The cap only targets unterminated runaway lines; a legitimate
    /// frame larger than the cap is still routed to the line drainer
    /// (it terminates on its own `\n`). This guards against false
    /// positives that would silently drop valid SSE frames.
    #[test]
    fn issue_695_enforce_sse_line_cap_skips_when_newline_present() {
        let mut buffer = String::with_capacity(2 * 1024 * 1024);
        buffer.push_str(&"x".repeat(2 * 1024 * 1024));
        buffer.push('\n');
        let pre_len = buffer.len();
        let outcome = enforce_sse_line_cap(&mut buffer);
        assert_eq!(
            outcome,
            SseLineCapOutcome::WithinCap,
            "newline-terminated frames are the drainer's job, not the cap's"
        );
        assert_eq!(
            buffer.len(),
            pre_len,
            "newline-terminated buffer must not be cleared"
        );
    }

    /// #695 — buffer reset is total: a newline-free overflow is
    /// discarded in full, not truncated.
    ///
    /// Forensic invariant: after the cap trips, the next valid frame
    /// arriving on the wire parses cleanly. Truncation (keeping a
    /// suffix) would corrupt the next line.
    #[test]
    fn issue_695_enforce_sse_line_cap_reset_is_total() {
        let mut buffer = "B".repeat(crate::proxy::MAX_SSE_LINE_BYTES + 7);
        let pre_len = buffer.len();
        let outcome = enforce_sse_line_cap(&mut buffer);
        assert_eq!(
            outcome,
            SseLineCapOutcome::Exceeded {
                discarded_bytes: pre_len
            },
            "discarded count must equal the full pre-reset buffer length"
        );
        assert_eq!(
            buffer.len(),
            0,
            "buffer must be fully cleared, not truncated"
        );

        // After reset, a fresh valid frame must drain normally.
        buffer.push_str("data: {\"ok\":true}\n");
        assert!(buffer.contains('\n'));
        let post_outcome = enforce_sse_line_cap(&mut buffer);
        assert_eq!(post_outcome, SseLineCapOutcome::WithinCap);
    }

    // ── Crosslink #788 — Gemini SAFETY finish-reason handling ────────────

    /// #788-1: `SAFETY` finish reason maps to `safety_blocked` and surfaces
    /// a user-visible error string. Pinning the prior bug: the function
    /// used to drop this signal silently and the TUI saw an empty completion.
    #[test]
    fn issue_788_safety_finish_reason_maps_to_safety_blocked_with_user_error() {
        let body = serde_json::json!({
            "candidates": [{
                "finishReason": "SAFETY",
                "content": { "parts": [] }
            }]
        });
        let out = classify_google_finish_reason(&body, 0);
        assert_eq!(
            out.finish_reason.as_deref(),
            Some("safety_blocked"),
            "SAFETY must normalize to safety_blocked"
        );
        let err = out.user_error.expect("SAFETY must produce a user error");
        assert!(
            err.contains("SAFETY"),
            "user error must name the original Gemini finishReason: {err}"
        );
        assert!(
            err.contains("blocked"),
            "user error must explain that the response was blocked: {err}"
        );
    }

    /// #788-2: `RECITATION` and `BLOCKLIST` map to the same normalized
    /// `safety_blocked` outcome — they are all "suppressed by filter"
    /// from the caller's perspective.
    #[test]
    fn issue_788_recitation_and_blocklist_also_map_to_safety_blocked() {
        for reason in ["RECITATION", "BLOCKLIST"] {
            let body = serde_json::json!({
                "candidates": [{ "finishReason": reason }]
            });
            let out = classify_google_finish_reason(&body, 0);
            assert_eq!(
                out.finish_reason.as_deref(),
                Some("safety_blocked"),
                "{reason} must normalize to safety_blocked"
            );
            assert!(
                out.user_error.is_some(),
                "{reason} must surface a user-visible error"
            );
        }
    }

    /// #788-3: Normal `STOP` and `MAX_TOKENS` must NOT trigger a user
    /// error — they are non-block terminations. `MAX_TOKENS` maps to
    /// `length` (matching OpenAI-side naming used elsewhere); `STOP`
    /// maps to `stop`. Missing `finishReason` yields the default
    /// (all `None`).
    #[test]
    fn issue_788_benign_finish_reasons_do_not_surface_error() {
        let stop = classify_google_finish_reason(
            &serde_json::json!({"candidates":[{"finishReason":"STOP"}]}),
            42,
        );
        assert_eq!(stop.finish_reason.as_deref(), Some("stop"));
        assert!(stop.user_error.is_none(), "STOP must not produce an error");

        let max = classify_google_finish_reason(
            &serde_json::json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}),
            128,
        );
        assert_eq!(max.finish_reason.as_deref(), Some("length"));
        assert!(
            max.user_error.is_none(),
            "MAX_TOKENS must not surface an ApiError (only a warn log)"
        );

        let none = classify_google_finish_reason(&serde_json::json!({"candidates":[{}]}), 0);
        assert_eq!(none, GoogleFinishClassification::default());
    }

    /// #788-4: Unknown finish reasons must pass through verbatim, NOT
    /// silently re-classified as a safety block. Pins behaviour against
    /// accidental over-triggering of user-visible errors if Google adds
    /// a new enum variant.
    #[test]
    fn issue_788_unknown_finish_reason_passes_through_verbatim_without_error() {
        let body = serde_json::json!({
            "candidates": [{ "finishReason": "FUTURE_REASON_X" }]
        });
        let out = classify_google_finish_reason(&body, 0);
        assert_eq!(
            out.finish_reason.as_deref(),
            Some("FUTURE_REASON_X"),
            "unknown finish reasons must pass through unchanged"
        );
        assert!(
            out.user_error.is_none(),
            "unknown finish reasons must NOT trigger a user-visible error"
        );
    }
}
