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

/// Build an OpenAI-compatible request body (used by OpenAI, DeepSeek, Qwen, Z.AI).
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
    let openai_tools = tools::get_all_tool_definitions(true);
    let tools_vec = openai_tools.as_array().cloned().unwrap_or_default();
    let functions: Vec<Value> = tools_vec
        .iter()
        .filter_map(|tool| {
            let func = tool.get("function")?;
            Some(serde_json::json!({
                "name": func.get("name")?,
                "description": func.get("description").unwrap_or(&serde_json::json!("")),
                "parameters": func.get("parameters").unwrap_or(&serde_json::json!({}))
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
        generation_config["thinkingConfig"] =
            serde_json::json!({"thinkingBudget": budget});
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
        "anthropic" => build_anthropic_request(
            model,
            messages,
            effective,
            claude_code_token,
            prompt_blocks,
        ),
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
    let mut headers = if let Some(token) = claude_code_token {
        crate::claude_credentials::get_oauth_headers(token)
    } else if let Some(key) = api_key {
        let adapter = get_adapter(provider);
        adapter.get_headers(key)
    } else {
        Vec::new()
    };
    headers.extend(extra_headers.iter().cloned());
    headers
}

// ─── Streaming + tool execution ─────────────────────────────────────────────

/// Run one turn of the conversation: send request, stream response, execute tools.
///
/// Sends `AppEvent` variants through `tx` as they occur so the TUI can update
/// in real time. Returns a `TurnResult` describing what happened.
///
/// # Errors
///
/// Returns `Err` if the HTTP request itself fails (network error, etc.).
pub async fn run_turn(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[(String, String)],
    request_body: &Value,
    provider: &str,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    tx: mpsc::Sender<AppEvent>,
) -> Result<TurnResult, String> {
    tracing::info!(
        endpoint,
        model = request_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?"),
        system_blocks = request_body
            .get("system")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        messages = request_body
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        has_tools = request_body
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
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
                .unwrap_or(2u64.pow(attempt + 1));
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
        return handle_google_response(response, memory_db, permission_mgr, &tx).await;
    }

    // Stream SSE response (Anthropic / OpenAI format)
    stream_sse_response(response, provider, memory_db, permission_mgr, &tx).await
}

/// Handle a non-streaming Google Gemini response.
async fn handle_google_response(
    response: reqwest::Response,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
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
        let code = error.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
        return Err(format!("Gemini API error ({code}): {msg}"));
    }

    if !text.is_empty() {
        send_event!(tx, AppEvent::StreamText(text.clone()));
    }

    // Extract tool calls
    let tool_calls: Vec<ToolCall> = gemini_json
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
                    let args = fc
                        .get("args")
                        .map(|a| serde_json::to_string(a).unwrap_or_default())
                        .unwrap_or_else(|| "{}".to_string());
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
        .unwrap_or_default();

    // Extract usage
    let input_tokens = gemini_json
        .get("usageMetadata")
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = gemini_json
        .get("usageMetadata")
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // Execute tool calls if any
    let (tool_results, needs_followup) =
        execute_tool_calls_for_tui(&tool_calls, memory_db, permission_mgr, tx).await;

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
    })
}

/// Stream an SSE response (Anthropic or OpenAI format), sending events to the TUI.
async fn stream_sse_response(
    response: reqwest::Response,
    provider: &str,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
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
    let (tool_results, has_tools) =
        execute_tool_calls_for_tui(&tool_calls, memory_db, permission_mgr, tx).await;

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
        if event_type == "content_block_start" {
            if let Some("thinking") = json
                .get("content_block")
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
            {
                return SseAction::ThinkingStart;
            }
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
            if always_denied.contains(tool_name) {
                send_event_or_break!(
                    tx,
                    AppEvent::ToolDone {
                        name: tool_name.clone(),
                        success: false,
                        content: "Denied (always deny for this session)".to_string(),
                    }
                );
                results.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call.id,
                    "content": "[DENIED] User denied permission for this tool.",
                    "is_error": true
                }));
                continue;
            }

            if !always_allowed.contains(tool_name) {
                // Send permission request and wait for response
                let (reply_tx, reply_rx) = mpsc::channel();
                let args_preview = if tool_call.function.arguments.len() > 200 {
                    format!(
                        "{}...",
                        crate::tools::safe_truncate(&tool_call.function.arguments, 197)
                    )
                } else {
                    tool_call.function.arguments.clone()
                };
                send_event_or_break!(
                    tx,
                    AppEvent::PermissionRequest {
                        tool_name: tool_name.clone(),
                        tool_args: args_preview,
                        reply: reply_tx,
                    }
                );

                // Block until user responds (TUI sends back the decision)
                match reply_rx.recv() {
                    Ok(PermissionResponse::Allow) => {}
                    Ok(PermissionResponse::AlwaysAllow) => {
                        always_allowed.insert(tool_name.clone());
                    }
                    Ok(PermissionResponse::AlwaysDeny) => {
                        always_denied.insert(tool_name.clone());
                        send_event_or_break!(
                            tx,
                            AppEvent::ToolDone {
                                name: tool_name.clone(),
                                success: false,
                                content: "Denied (always deny)".to_string(),
                            }
                        );
                        results.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_call.id,
                            "content": "[DENIED] User denied permission.",
                            "is_error": true
                        }));
                        continue;
                    }
                    Ok(PermissionResponse::Deny) | Err(_) => {
                        send_event_or_break!(
                            tx,
                            AppEvent::ToolDone {
                                name: tool_name.clone(),
                                success: false,
                                content: "Denied by user".to_string(),
                            }
                        );
                        results.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_call.id,
                            "content": "[DENIED] User denied permission.",
                            "is_error": true
                        }));
                        continue;
                    }
                }
            }
        }

        // Build a descriptive preview of what the tool is doing
        let args_desc = {
            let args: serde_json::Value =
                serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();
            match tool_name.as_str() {
                "read_file" => args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| format!("Reading {p}"))
                    .unwrap_or_else(|| "Reading file".to_string()),
                "write_file" => args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| format!("Writing {p}"))
                    .unwrap_or_else(|| "Writing file".to_string()),
                "edit_file" => args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| format!("Editing {p}"))
                    .unwrap_or_else(|| "Editing file".to_string()),
                "bash" => args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|c| {
                        let truncated = if c.len() > 80 {
                            crate::tools::safe_truncate(c, 77)
                        } else {
                            c
                        };
                        format!("$ {truncated}")
                    })
                    .unwrap_or_else(|| "Running command".to_string()),
                "list_files" => args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| format!("Listing {p}"))
                    .unwrap_or_else(|| "Listing files".to_string()),
                "web_search" => args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(|q| format!("Searching: {q}"))
                    .unwrap_or_else(|| "Searching web".to_string()),
                "web_fetch" => args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|u| format!("Fetching {u}"))
                    .unwrap_or_else(|| "Fetching URL".to_string()),
                "chainlink" => args
                    .get("args")
                    .and_then(|v| v.as_str())
                    .map(|a| format!("crosslink {a}"))
                    .unwrap_or_else(|| "Running crosslink".to_string()),
                _ => format!("Running {tool_name}"),
            }
        };

        send_event_or_break!(
            tx,
            AppEvent::ToolStart {
                name: tool_name.clone(),
                description: args_desc,
            }
        );

        // Run tool on a blocking thread so the async event channel stays
        // responsive — TUI can redraw and show the spinner/progress while
        // the tool executes.
        let tool_call_clone = tool_call.clone();
        let mem_db = memory_db.clone();
        let perm_mgr = permission_mgr.clone();
        // Library-layer gate runs in addition to the TUI's UX-layer
        // `PermissionResponse` flow. Closes crosslink #505 — previously
        // threaded `None`, which emitted a one-shot warn and left the
        // config-driven `default_allow` patterns unenforced.
        let result = tokio::task::spawn_blocking(move || {
            let pm = perm_mgr.as_deref();
            if let Some(ref db) = mem_db {
                tools::execute_tool_with_memory(&tool_call_clone, Some(db), pm)
            } else {
                tools::execute_tool_with_memory(&tool_call_clone, None, pm)
            }
        })
        .await
        .unwrap_or_else(|e| tools::ToolResult {
            tool_call_id: tool_call.id.clone(),
            content: format!("Tool execution panicked: {e}"),
            is_error: true,
        });

        send_event_or_break!(
            tx,
            AppEvent::ToolDone {
                name: tool_name.clone(),
                success: !result.is_error,
                content: result.content.clone(),
            }
        );

        let result_content = if result.is_error {
            format!("[ERROR] {}", result.content)
        } else {
            result.content
        };
        results.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": result.tool_call_id,
            "content": result_content,
            "is_error": result.is_error
        }));
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
        unsafe { std::env::remove_var("MAX_THINKING_TOKENS"); }
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
            unsafe { std::env::set_var("MAX_THINKING_TOKENS", v); }
        }
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
        if let Some(v) = prev.0 { unsafe { std::env::set_var("MAX_THINKING_TOKENS", v); } }
        if let Some(v) = prev.1 { unsafe { std::env::set_var("CLAUDE_CODE_EFFORT_LEVEL", v); } }
    }
}
