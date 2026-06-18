//! API pipeline — builds requests, streams responses, and executes tools.
//!
//! Extracted from the `cmd_chat` function in `main.rs` to enable reuse
//! from both the rustyline REPL and the ratatui TUI.

use crate::config::ThinkingConfig;
use crate::memory::MemoryDb;
use crate::permissions::{PermissionManager, PermissionRule};
use crate::providers::{
    convert_messages_to_anthropic_checked, convert_tool_definitions_to_anthropic_checked,
    convert_tools_to_gemini_functions, extract_gemini_text_content, get_adapter,
};
use crate::proxy::{self, normalize_base_url};
use crate::session::TokenUsage;
use crate::tools::{self, AnthropicToolAccumulator, ToolCall, ToolCallAccumulator};
use crate::tui::events::{AppEvent, PermissionResponse};
use futures::StreamExt;
use serde_json::Value;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

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
    /// Provider reasoning content accumulated during streaming, when the
    /// upstream exposes it separately from visible text.
    pub reasoning_content: Option<String>,
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
///
/// # Errors
///
/// Returns an error when historical assistant `tool_calls` contain malformed
/// Anthropic tool-call arguments that cannot be represented safely.
pub fn build_anthropic_request(
    model: &str,
    messages: &[Value],
    effort_level: &str,
    claude_code_token: Option<&str>,
    prompt_blocks: Option<&crate::prompt::SystemPromptBlocks>,
) -> Result<Value, String> {
    let anthropic_messages =
        convert_messages_to_anthropic_checked(messages).map_err(|e| e.to_string())?;
    let openai_tools = tools::get_all_tool_definitions(true);
    let anthropic_tools =
        convert_tool_definitions_to_anthropic_checked(&openai_tools).map_err(|e| e.to_string())?;

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

    Ok(req)
}

/// Build an OpenAI-compatible request body (used by `OpenAI`, `DeepSeek`, Qwen, Z.AI).
///
/// `effort_level` propagates as `reasoning_effort` for `high`/`max`;
/// `max` downgrades to `high` because the API only accepts the level on
/// a subset of models (matches Claude Code's `modelSupportsMaxEffort`
/// clamp).
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

fn build_chat_completion_request(
    model: &str,
    messages: &[Value],
) -> Result<proxy::ChatCompletionRequest, String> {
    let messages = messages
        .iter()
        .enumerate()
        .map(|(index, msg)| {
            serde_json::from_value::<proxy::ChatMessage>(msg.clone()).map_err(|e| {
                format!("message at index {index} is not a valid chat message: {e}: {msg}")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tools = tools::get_all_tool_definitions(true)
        .as_array()
        .ok_or_else(|| "built-in tool definitions must be a JSON array".to_string())?
        .clone();

    Ok(proxy::ChatCompletionRequest {
        model: model.to_string(),
        messages,
        temperature: None,
        max_tokens: Some(crate::DEFAULT_MAX_TOKENS),
        stream: Some(true),
        tools: Some(tools),
        tool_choice: None,
        extra: std::collections::HashMap::new(),
    })
}

fn thinking_config_for_pipeline_effort(effort_level: &str) -> Option<ThinkingConfig> {
    matches!(effort_level, "high" | "max").then(|| ThinkingConfig {
        enabled: true,
        budget_tokens: None,
        preserve_across_turns: false,
        reasoning_effort: Some(if effort_level == "max" { "max" } else { "high" }.to_string()),
        adaptive: true,
    })
}

fn build_adapter_request(
    provider: &str,
    model: &str,
    messages: &[Value],
    effort_level: &str,
) -> Result<Value, String> {
    let request = build_chat_completion_request(model, messages)?;
    let adapter = get_adapter(provider).map_err(|e| e.to_string())?;
    let body = thinking_config_for_pipeline_effort(effort_level).map_or_else(
        || adapter.transform_request(&request),
        |thinking| adapter.transform_request_with_thinking(&request, &thinking),
    );
    body.map_err(|e| e.to_string())
}

/// Build a Google Gemini-format request body.
///
/// # Errors
///
/// Returns an error if the built-in tool definitions cannot be represented as
/// Gemini function declarations.
pub fn build_google_request(messages: &[Value], effort_level: &str) -> Result<Value, String> {
    let openai_tools = tools::get_all_tool_definitions(true);
    let tools_vec = openai_tools
        .as_array()
        .ok_or_else(|| "built-in tool definitions must be a JSON array".to_string())?;
    let functions = convert_tools_to_gemini_functions(tools_vec).map_err(|e| e.to_string())?;

    let mut contents = Vec::new();
    let mut system_parts: Vec<String> = Vec::new();
    for (msg_index, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(Value::as_str).ok_or_else(|| {
            format!("Google message at index {msg_index} missing string 'role': {msg}")
        })?;
        let text = msg.get("content").and_then(Value::as_str).ok_or_else(|| {
            format!("Google message at index {msg_index} missing string 'content': {msg}")
        })?;
        if role == "system" {
            if !text.is_empty() {
                system_parts.push(text.to_string());
            }
            continue;
        }
        let gemini_role = match role {
            "assistant" => "model",
            "user" | "tool" => "user",
            _ => {
                return Err(format!(
                    "Google message at index {msg_index} has unsupported role '{role}': {msg}"
                ));
            }
        };
        contents.push(serde_json::json!({
            "role": gemini_role,
            "parts": [{"text": text}]
        }));
    }

    // Gemini takes `thinkingConfig.thinkingBudget` inside
    // generationConfig. When effort is high/max we hand it the Claude
    // Code ULTRATHINK constant, clamped to Gemini's documented ceiling.
    let mut generation_config = serde_json::json!({"maxOutputTokens": 4096});
    if matches!(effort_level, "high" | "max") {
        const GEMINI_THINKING_CAP: u32 = 32_768;
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
    let system_text = (!system_parts.is_empty()).then(|| system_parts.join("\n\n"));
    if let Some(sys) = system_text {
        req["systemInstruction"] = serde_json::json!({"parts": [{"text": sys}]});
    }
    Ok(req)
}

/// Build the appropriate request body for the given provider.
///
/// `prompt_blocks` is used only for the Anthropic path to enable multi-block
/// cache-efficient system prompts.  Pass `None` for the legacy single-block path.
///
/// # Errors
///
/// Returns an error when the selected provider's request conversion rejects
/// malformed message history.
pub fn build_request(
    provider: &str,
    model: &str,
    messages: &[Value],
    effort_level: &str,
    claude_code_token: Option<&str>,
    prompt_blocks: Option<&crate::prompt::SystemPromptBlocks>,
) -> Result<Value, String> {
    // Resolve ultrathink keyword / env override against the base effort
    // so every provider path sees the same effective level (Claude Code
    // does the same in `resolveAppliedEffort`). If env says `unset` /
    // `auto`, `medium` flows through as the request builders' no-op
    // effort level, omitting provider effort hints.
    let resolved = crate::thinking::resolve_effort(effort_level, messages);
    let effective = resolved.as_deref().unwrap_or("medium");
    match provider.to_ascii_lowercase().as_str() {
        "anthropic" => {
            build_anthropic_request(model, messages, effective, claude_code_token, prompt_blocks)
        }
        "google" | "gemini" => build_google_request(messages, effective),
        _ => build_adapter_request(provider, model, messages, effective),
    }
}

/// Resolve the API endpoint for the given provider configuration.
///
/// # Errors
///
/// Returns [`crate::providers::ProviderError::UnknownProvider`] when
/// `provider` is not a registered adapter name AND the caller is not
/// using a Claude Code OAuth token (OAuth bypasses adapter dispatch
/// because the endpoint is fixed by `get_oauth_endpoint`). Previously
/// (crosslink #433) this function silently fell back to
/// `/v1/chat/completions` against `OpenAIAdapter`, hiding typos in
/// `proxy.target` from the user.
pub fn resolve_endpoint(
    provider: &str,
    model: &str,
    base_url: &str,
    claude_code_token: Option<&str>,
) -> Result<String, crate::providers::ProviderError> {
    if claude_code_token.is_some() {
        Ok(crate::claude_credentials::get_oauth_endpoint(model))
    } else {
        let adapter = get_adapter(provider)?;
        Ok(format!(
            "{}{}",
            normalize_base_url(base_url),
            adapter.chat_endpoint(model)
        ))
    }
}

/// Build the headers needed for the API request.
///
/// `api_key` is `Option<&ApiKey>`. If both `api_key` and
/// `claude_code_token` are `None`, the function returns an empty auth set.
/// Callers validate whether that is acceptable for the selected provider
/// (for example, Anthropic OAuth bootstrap and local providers can proceed
/// without static API keys). See crosslink #256.
///
/// # Errors
///
/// Returns [`crate::providers::ProviderError::UnknownProvider`] when
/// `provider` is unknown AND an API key is being used (the OAuth path
/// uses `get_oauth_headers` which doesn't go through adapter dispatch).
/// See crosslink #433.
pub fn resolve_headers(
    provider: &str,
    api_key: Option<&crate::providers::ApiKey>,
    claude_code_token: Option<&str>,
    extra_headers: &[(String, String)],
) -> Result<Vec<(String, String)>, crate::providers::ProviderError> {
    let mut headers = if let Some(token) = claude_code_token {
        crate::claude_credentials::get_oauth_headers(token)
    } else if let Some(key) = api_key {
        let adapter = get_adapter(provider)?;
        adapter.get_headers(key)
    } else {
        Vec::new()
    };
    headers.extend(extra_headers.iter().cloned());
    Ok(headers)
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
    pub transient_allowed_tool_rules: &'a [PermissionRule],
    pub hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    /// Session-scoped `TaskManager` used by `task_create` / `task_update`
    /// / `task_list` / `task_get`. The TUI keeps a single
    /// `Arc<Mutex<TaskManager>>` and clones the `Arc` into every turn so
    /// the task tools have a place to read/write — without this they
    /// returned "Task management not available (no session)".
    pub task_mgr: Arc<Mutex<crate::session::TaskManager>>,
    pub session_id: Option<String>,
    pub tx: mpsc::Sender<AppEvent>,
}

// ---------------------------------------------------------------------------
// Retry classifier + backoff helpers (crosslink #592, #595, #596, #597)
// ---------------------------------------------------------------------------

/// Maximum retry attempts for transient API errors.
///
/// Matches CC's `withRetry.ts::DEFAULT_MAX_RETRIES` (10). Per crosslink
/// #592 — was previously 3, which gave up too quickly on rate-limit
/// surges that 10 attempts of jittered exponential backoff would have
/// ridden out.
pub const MAX_API_RETRIES: u32 = 10;

/// HTTP status codes that warrant a retry. Matches CC's
/// `withRetry.ts` transient-status set:
///   * 408 — Request Timeout
///   * 409 — Conflict (transient concurrent-mutation case)
///   * 429 — Rate Limited
///   * 500 / 502 / 503 / 504 — server-side transient
///   * 529 — Anthropic-specific "service overloaded"
#[must_use]
pub const fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500 | 502 | 503 | 504 | 529)
}

/// Transport-layer errors that warrant a retry.
///
/// `ConnectionReset` and `BrokenPipe` are the canonical "TCP/TLS
/// dropped under us" signals every long-lived streaming client sees;
/// both are transient. Per crosslink #597.
#[must_use]
pub fn is_transient_transport_error(err: &reqwest::Error) -> bool {
    use std::io;
    // Walk the source chain looking for an io::Error whose kind is
    // ConnectionReset or BrokenPipe. reqwest doesn't surface these
    // structurally so source-chain inspection is the supported path.
    let mut cur: &dyn std::error::Error = err;
    loop {
        if let Some(ioerr) = cur.downcast_ref::<io::Error>() {
            if matches!(
                ioerr.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
            ) {
                return true;
            }
        }
        match cur.source() {
            Some(next) => cur = next,
            None => return false,
        }
    }
}

/// Exponential backoff (base = `2^(attempt+1)` seconds) with ±25% jitter,
/// per crosslink #596 — when many clients hit the same 429 they must not
/// all retry in lockstep and re-collide. Returns a wait in seconds.
///
/// The jitter is deterministic on a thread-local RNG path (we use
/// `std::time::Instant::now()` nanos as the jitter source so unit tests
/// observe non-equal sleeps without needing a `rand` dependency).
fn backoff_with_jitter(attempt: u32) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let base = 2u64.saturating_pow(attempt + 1);
    // Jitter range is ±25% of base, minimum ±1 so attempt=0 still
    // produces some spread between concurrent retriers.
    let max_jitter = std::cmp::max(base / 4, 1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let jitter = nanos % (max_jitter * 2 + 1); // 0..=2*max_jitter
                                               // Apply jitter as an unsigned offset around the base. Adding
                                               // `jitter` then subtracting `max_jitter` keeps everything unsigned
                                               // and saturates at 1 (we never want a 0-second sleep on a stuck
                                               // transient).
    let raw = base.saturating_add(jitter).saturating_sub(max_jitter);
    raw.max(1)
}

fn retry_jitter_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()))
}

const fn retry_after_with_jitter_from(
    retry_after_secs: u64,
    jitter_seed: u64,
) -> std::time::Duration {
    let base_ms = retry_after_secs.saturating_mul(1_000);
    if base_ms == 0 {
        return std::time::Duration::ZERO;
    }
    let max_jitter_ms = base_ms / 4;
    let jitter_ms = jitter_seed % (max_jitter_ms + 1);
    std::time::Duration::from_millis(base_ms.saturating_add(jitter_ms))
}

fn retry_after_with_jitter(retry_after_secs: u64) -> std::time::Duration {
    retry_after_with_jitter_from(retry_after_secs, retry_jitter_seed())
}

fn retry_delay_label(delay: std::time::Duration) -> String {
    let millis = delay.as_millis();
    if millis.is_multiple_of(1_000) {
        format!("{}s", millis / 1_000)
    } else {
        let seconds = millis / 1_000;
        let hundredths = (millis % 1_000) / 10;
        format!("{seconds}.{hundredths:02}s")
    }
}

/// Map a model name to a lighter sibling that's suitable as a fallback when
/// the requested model is sustainedly overloaded (HTTP 529).
///
/// The mapping is intentionally conservative — it only fires for model
/// families where the lighter sibling is a known good degraded-mode target.
/// Returns an empty string when no sensible fallback is known, in which
/// case [`AppEvent::OverloadFallback`] is still emitted (so log consumers
/// see the exhaustion signal) but the UI surface should not auto-switch.
///
/// See crosslink #598 — CC has an analogous mapping in
/// `getFallbackModelForOverload` that downgrades opus→sonnet→haiku.
#[must_use]
pub fn overload_fallback_for(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    // Claude family — opus → sonnet → haiku
    if m.contains("opus") {
        return "claude-sonnet-4-6";
    }
    if m.contains("sonnet") {
        return "claude-haiku-4-5";
    }
    if m.contains("haiku") {
        // Already the lightest tier — no further fallback.
        return "";
    }
    // GPT family — latest frontier/standard models → current mini/nano tiers.
    if m.starts_with("gpt-5.5") || m.starts_with("gpt-5.4") {
        return "gpt-5.4-mini";
    }
    if m.starts_with("gpt-5") {
        return "gpt-5-mini";
    }
    // Older GPT/o-series families keep the legacy lightweight fallback.
    if m.starts_with("gpt-4") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        return "gpt-4o-mini";
    }
    // Gemini family — pro → flash
    if m.contains("gemini") && m.contains("pro") {
        return "gemini-3.5-flash";
    }
    ""
}

/// Drive the API request through up to `MAX_API_RETRIES` attempts,
/// classifying transient transport errors and retryable HTTP statuses
/// per crosslink #595/#596/#597. Each retry emits a structured
/// `tracing::warn!` (`target="openclaudia::retry"`, `event="api_retry"`)
/// so log consumers can bucket retry pressure programmatically. The
/// user-facing `AppEvent::StreamText` retry marker is preserved for
/// REPL/TUI compatibility.
///
/// When the loop exhausts [`MAX_API_RETRIES`] on a 529 ("service
/// overloaded") status, the function additionally emits
/// [`AppEvent::OverloadFallback`] with an advisory model hint so the
/// orchestrator can suggest or automatically switch to a lighter
/// sibling. See crosslink #598.
async fn send_with_retry(
    client: &reqwest::Client,
    endpoint: &str,
    headers: &[(String, String)],
    request_body: &Value,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<reqwest::Response, String> {
    let mut response = None;
    for attempt in 0..=MAX_API_RETRIES {
        let mut req = client.post(endpoint).json(request_body);
        for (key, value) in headers {
            req = req.header(key, value);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if attempt < MAX_API_RETRIES && is_transient_transport_error(&e) => {
                let wait_secs = backoff_with_jitter(attempt);
                tracing::warn!(
                    target: "openclaudia::retry",
                    event = "api_retry",
                    kind = "transport",
                    attempt = attempt + 1,
                    max_attempts = MAX_API_RETRIES + 1,
                    wait_secs,
                    error = %e,
                    "transient transport error, retrying"
                );
                let _ = tx.send(AppEvent::StreamText(format!(
                    "\n(Retrying in {wait_secs}s — transport...)\n"
                )));
                tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
                continue;
            }
            Err(e) => return Err(format!("Request failed: {e}")),
        };
        let status = resp.status().as_u16();

        if is_retryable_status(status) && attempt < MAX_API_RETRIES {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map_or_else(
                    || std::time::Duration::from_secs(backoff_with_jitter(attempt)),
                    retry_after_with_jitter,
                );
            tracing::warn!(
                target: "openclaudia::retry",
                event = "api_retry",
                kind = "status",
                attempt = attempt + 1,
                max_attempts = MAX_API_RETRIES + 1,
                status,
                wait_ms = wait.as_millis(),
                "transient API status, retrying"
            );
            let delay_label = retry_delay_label(wait);
            let _ = tx.send(AppEvent::StreamText(format!(
                "\n(Retrying in {delay_label} — {status}...)\n"
            )));
            tokio::time::sleep(wait).await;
            continue;
        }

        if !resp.status().is_success() {
            // Crosslink #598: the retry loop has reached its budget on a
            // retryable status. If that status is 529 (Anthropic "service
            // overloaded"), emit an OverloadFallback advisory so the UI
            // can suggest / auto-switch to a lighter model. We compute
            // the hint from the request body's `model` field — the
            // request was built upstream by the proxy and always carries
            // it.
            if status == 529 {
                let model = request_body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let hint = overload_fallback_for(model);
                tracing::warn!(
                    target: "openclaudia::retry",
                    event = "overload_fallback",
                    model,
                    model_hint = hint,
                    "529 overload persisted past retry budget; emitting OverloadFallback"
                );
                let _ = tx.send(AppEvent::OverloadFallback {
                    model_hint: hint.to_string(),
                });
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("API error {status}: {body}"));
        }

        response = Some(resp);
        break;
    }
    response.ok_or_else(|| "Max retries exceeded".to_string())
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
        transient_allowed_tool_rules,
        hook_engine,
        task_mgr,
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

    // Send request with retry on transient errors. See `send_with_retry`
    // for the per-attempt classification logic (crosslink #592 #595 #596 #597).
    let response = send_with_retry(client, endpoint, headers, request_body, &tx).await?;

    // For Google, handle non-streaming JSON response
    if provider == "google" {
        return handle_google_response(
            response,
            memory_db,
            permission_mgr,
            transient_allowed_tool_rules,
            hook_engine.clone(),
            task_mgr.clone(),
            session_id.clone(),
            &tx,
        )
        .await;
    }

    // Stream SSE response (Anthropic / OpenAI format)
    stream_sse_response(SseStreamParams {
        response,
        provider,
        memory_db,
        permission_mgr,
        transient_allowed_tool_rules,
        hook_engine,
        task_mgr,
        session_id,
        tx: &tx,
    })
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

fn google_response_parts(gemini_json: &Value) -> Result<&[Value], String> {
    let candidate = gemini_json
        .get("candidates")
        .and_then(|c| c.get(0))
        .ok_or_else(|| format!("Gemini response missing candidates[0]: {gemini_json}"))?;

    candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .map(Vec::as_slice)
        .ok_or_else(|| format!("Gemini candidate missing content.parts array: {candidate}"))
}

fn extract_google_text(parts: &[Value]) -> Result<String, String> {
    extract_gemini_text_content(parts).map_err(|e| e.to_string())
}

/// Extract structured tool calls from Gemini `content.parts`.
fn extract_google_tool_calls_from_parts(parts: &[Value]) -> Result<Vec<ToolCall>, String> {
    let mut calls = Vec::new();

    for part in parts {
        let Some(fc) = part.get("functionCall") else {
            continue;
        };

        if !fc.is_object() {
            return Err(format!("Gemini functionCall must be an object: {fc}"));
        }

        let name = fc
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("Gemini functionCall missing non-empty string 'name': {fc}"))?
            .to_string();

        let args = fc
            .get("args")
            .ok_or_else(|| format!("Gemini functionCall missing object 'args': {fc}"))?;

        if !args.is_object() {
            return Err(format!(
                "Gemini functionCall has non-object 'args': expected JSON object, got {}",
                json_value_type_name(args)
            ));
        }

        let args = serde_json::to_string(args).map_err(|e| {
            format!("Gemini functionCall has unserializable 'args': {e}; functionCall: {fc}")
        })?;

        calls.push(ToolCall {
            id: format!("call_{}", uuid::Uuid::new_v4()),
            call_type: "function".to_string(),
            function: tools::FunctionCall {
                name,
                arguments: args,
            },
        });
    }

    Ok(calls)
}

/// Extract structured tool calls from a Gemini non-streaming response.
#[cfg(test)]
fn extract_google_tool_calls(gemini_json: &Value) -> Result<Vec<ToolCall>, String> {
    extract_google_tool_calls_from_parts(google_response_parts(gemini_json)?)
}

const fn json_value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
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
    transient_allowed_tool_rules: &[PermissionRule],
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    task_mgr: Arc<Mutex<crate::session::TaskManager>>,
    session_id: Option<String>,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<TurnResult, String> {
    let body = response.text().await.unwrap_or_default();
    let gemini_json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse Gemini response: {e}"))?;

    // Check for Gemini error responses
    if let Some(error) = gemini_json.get("error") {
        let msg = error
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
            .ok_or_else(|| {
                format!("Gemini API error missing non-empty string 'message': {error}")
            })?;
        let code = error
            .get("code")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        return Err(format!("Gemini API error ({code}): {msg}"));
    }

    let parts = google_response_parts(&gemini_json)?;
    let text = extract_google_text(parts)?;

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

    let tool_calls = extract_google_tool_calls_from_parts(parts)?;
    let (input_tokens, output_tokens) = extract_google_usage(&gemini_json);

    // Execute tool calls if any
    let (tool_results, needs_followup) = execute_tool_calls_for_tui(
        &tool_calls,
        memory_db,
        permission_mgr,
        transient_allowed_tool_rules,
        hook_engine,
        task_mgr,
        session_id.as_deref(),
        tx,
    )
    .await;

    if !needs_followup {
        send_event!(tx, AppEvent::ResponseDone);
    }

    Ok(TurnResult {
        content: text,
        reasoning_content: None,
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
/// Emit the structured #600 timeout event and append the user-facing
/// inline marker. Extracted so `stream_sse_response` stays within the
/// `too_many_lines` lint.
fn handle_sse_timeout(elapsed_secs: u64, full_content: &mut String) {
    tracing::error!(
        target: "openclaudia::stream",
        event = "sse_stream_timeout",
        kind = "result",
        is_error = true,
        elapsed_secs,
        timeout_secs = proxy::SSE_STREAM_TIMEOUT_SECS,
        content_so_far_bytes = full_content.len(),
        "SSE stream timed out without further data"
    );
    if !full_content.is_empty() {
        full_content.push_str("\n\n[Response truncated: stream timeout]");
    }
}

/// Borrowed inputs threaded through the SSE-streaming code path.
///
/// Bundled because the inner function previously took 8 positional
/// arguments, which trips `clippy::too_many_arguments` (threshold 7).
/// All fields are owned / `Arc`-shared resources the inner pipeline
/// stages need; the param struct mirrors the established
/// [`RunTurnParams`] pattern.
struct SseStreamParams<'a> {
    response: reqwest::Response,
    provider: &'a str,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    transient_allowed_tool_rules: &'a [PermissionRule],
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    task_mgr: Arc<Mutex<crate::session::TaskManager>>,
    session_id: Option<String>,
    tx: &'a mpsc::Sender<AppEvent>,
}

async fn stream_sse_response(p: SseStreamParams<'_>) -> Result<TurnResult, String> {
    let SseStreamParams {
        response,
        provider,
        memory_db,
        permission_mgr,
        transient_allowed_tool_rules,
        hook_engine,
        task_mgr,
        session_id,
        tx,
    } = p;
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut full_content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_accumulator = ToolCallAccumulator::new();
    let mut anthropic_accumulator = AnthropicToolAccumulator::new();
    let mut stream_usage = TokenUsage::default();
    let mut in_thinking_block = false;
    let mut last_data_time = std::time::Instant::now();
    let stream_timeout = std::time::Duration::from_secs(proxy::SSE_STREAM_TIMEOUT_SECS);

    while let Some(chunk_result) = stream.next().await {
        if last_data_time.elapsed() > stream_timeout {
            handle_sse_timeout(last_data_time.elapsed().as_secs(), &mut full_content);
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

                            let action = process_sse_event(
                                &json,
                                in_thinking_block,
                                &mut anthropic_accumulator,
                                &mut tool_accumulator,
                            );
                            dispatch_sse_action(
                                action,
                                SseActionDispatch {
                                    full_content: &mut full_content,
                                    reasoning_content: &mut reasoning_content,
                                    in_thinking_block: &mut in_thinking_block,
                                    tx,
                                },
                            )?;
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

    finalize_sse_stream(SseFinalize {
        provider,
        full_content,
        reasoning_content,
        tool_accumulator,
        anthropic_accumulator,
        stream_usage,
        memory_db,
        permission_mgr,
        transient_allowed_tool_rules,
        hook_engine,
        task_mgr,
        session_id,
        tx,
    })
    .await
}

struct SseActionDispatch<'a> {
    full_content: &'a mut String,
    reasoning_content: &'a mut String,
    in_thinking_block: &'a mut bool,
    tx: &'a mpsc::Sender<AppEvent>,
}

fn dispatch_sse_action(action: SseAction, ctx: SseActionDispatch<'_>) -> Result<(), String> {
    let SseActionDispatch {
        full_content,
        reasoning_content,
        in_thinking_block,
        tx,
    } = ctx;
    match action {
        SseAction::Text(text) => {
            send_event!(tx, AppEvent::StreamText(text.clone()));
            full_content.push_str(&text);
        }
        SseAction::Thinking(text) => {
            send_event!(tx, AppEvent::StreamThinking(text));
        }
        SseAction::Reasoning(text) => {
            let display_text = merge_reasoning_delta(reasoning_content, &text);
            if !display_text.is_empty() {
                send_event!(tx, AppEvent::StreamThinking(display_text));
            }
        }
        SseAction::ThinkingStart => {
            *in_thinking_block = true;
            send_event!(tx, AppEvent::StreamThinking("[thinking...]\n".to_string(),));
        }
        SseAction::ThinkingEnd => {
            *in_thinking_block = false;
        }
        SseAction::None => {}
    }
    Ok(())
}

/// Owned + borrowed state handed to [`finalize_sse_stream`].
///
/// Extracted from `stream_sse_response` (which otherwise tipped over
/// the `clippy::too_many_lines` threshold once `task_mgr` was threaded
/// through). The struct lets the finalize helper take ownership of the
/// accumulators and the per-turn channels in a single move.
struct SseFinalize<'a> {
    provider: &'a str,
    full_content: String,
    reasoning_content: String,
    tool_accumulator: ToolCallAccumulator,
    anthropic_accumulator: AnthropicToolAccumulator,
    stream_usage: TokenUsage,
    memory_db: Option<Arc<MemoryDb>>,
    permission_mgr: Option<Arc<PermissionManager>>,
    transient_allowed_tool_rules: &'a [PermissionRule],
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    task_mgr: Arc<Mutex<crate::session::TaskManager>>,
    session_id: Option<String>,
    tx: &'a mpsc::Sender<AppEvent>,
}

/// Drain the streaming accumulators into a `TurnResult`, dispatching
/// any captured tool calls. Sends `ResponseDone` when no follow-up
/// turn is needed — the agentic loop handles the follow-up case.
async fn finalize_sse_stream(f: SseFinalize<'_>) -> Result<TurnResult, String> {
    // Determine tool calls from the appropriate accumulator
    let tool_calls = if f.provider == "anthropic" && f.anthropic_accumulator.has_tool_use() {
        f.anthropic_accumulator.finalize_tool_calls()
    } else if f.tool_accumulator.has_tool_calls() {
        f.tool_accumulator.finalize()
    } else {
        vec![]
    };

    // Execute tool calls if any
    let (tool_results, has_tools) = execute_tool_calls_for_tui(
        &tool_calls,
        f.memory_db,
        f.permission_mgr,
        f.transient_allowed_tool_rules,
        f.hook_engine,
        f.task_mgr,
        f.session_id.as_deref(),
        f.tx,
    )
    .await;

    // Only send ResponseDone if there are NO tool calls needing followup.
    // When there are tool calls, the caller (app.rs agentic loop) handles
    // the followup requests and sends ResponseDone when truly finished.
    if !has_tools {
        send_event!(f.tx, AppEvent::ResponseDone);
    }

    Ok(TurnResult {
        content: f.full_content,
        reasoning_content: (!f.reasoning_content.is_empty()).then_some(f.reasoning_content),
        tool_calls,
        tool_results,
        usage: f.stream_usage,
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
    /// Emit OpenAI-compatible reasoning text.
    Reasoning(String),
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
        if let Some(reasoning) = openai_reasoning_delta_text(delta) {
            return SseAction::Reasoning(reasoning);
        }
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            return SseAction::Text(content.to_string());
        }
        tool_accumulator.process_delta(delta);
    }

    SseAction::None
}

fn openai_reasoning_delta_text(delta: &Value) -> Option<String> {
    if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
        return (!reasoning.is_empty()).then(|| reasoning.to_string());
    }
    if let Some(reasoning) = delta.get("reasoning").and_then(Value::as_str) {
        return (!reasoning.is_empty()).then(|| reasoning.to_string());
    }

    let details = delta.get("reasoning_details").and_then(Value::as_array)?;
    let text = details
        .iter()
        .filter_map(|detail| detail.get("text").and_then(Value::as_str))
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

/// Append a reasoning delta to `buffer` and return only the newly-displayable text.
///
/// Some OpenAI-compatible providers send cumulative reasoning text instead of
/// incremental chunks. This keeps persisted reasoning complete while avoiding
/// duplicate display output.
#[must_use]
pub fn merge_reasoning_delta(buffer: &mut String, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    if !buffer.is_empty() && text.starts_with(buffer.as_str()) {
        let suffix = text[buffer.len()..].to_string();
        buffer.push_str(&suffix);
        suffix
    } else {
        buffer.push_str(text);
        text.to_string()
    }
}

/// Tools that are safe to execute without permission (read-only / informational).
const SAFE_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "grep",
    "glob",
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
    "crosslink",
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
    Allowed { checked: bool },
    /// The tool was denied; the caller should push `result_json` and `continue`.
    DeniedWithResult(serde_json::Value),
    /// The permission channel is broken; the caller should `break`.
    ChannelBroken,
}

fn permission_denied_with_result(
    tool_name: &str,
    tool_call_id: &str,
    tool_done_content: &str,
    model_content: &str,
    tx: &mpsc::Sender<AppEvent>,
) -> PermissionOutcome {
    let _ = tx.send(AppEvent::ToolDone {
        name: tool_name.to_string(),
        success: false,
        content: tool_done_content.to_string(),
    });
    PermissionOutcome::DeniedWithResult(serde_json::json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": model_content,
        "is_error": true
    }))
}

fn parse_permission_arguments_for_tui(
    tool_name: &str,
    tool_call_id: &str,
    arguments: &str,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<Value, PermissionOutcome> {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) => Ok(Value::Object(map)),
        Ok(value) => {
            let msg = format!(
                "Invalid tool arguments JSON for '{tool_name}': expected a JSON object, got {}",
                json_value_type_name(&value)
            );
            Err(permission_denied_with_result(
                tool_name,
                tool_call_id,
                &msg,
                &format!("[ERROR] {msg}"),
                tx,
            ))
        }
        Err(err) => {
            let msg = format!("Invalid tool arguments JSON for '{tool_name}': {err}");
            Err(permission_denied_with_result(
                tool_name,
                tool_call_id,
                &msg,
                &format!("[ERROR] {msg}"),
                tx,
            ))
        }
    }
}

fn permission_manager_outcome_for_tui(
    tool_name: &str,
    tool_call_id: &str,
    arguments: &str,
    mgr: &PermissionManager,
    transient_allowed_tool_rules: &[PermissionRule],
    tx: &mpsc::Sender<AppEvent>,
) -> Option<PermissionOutcome> {
    let args = match parse_permission_arguments_for_tui(tool_name, tool_call_id, arguments, tx) {
        Ok(args) => args,
        Err(outcome) => return Some(outcome),
    };

    match mgr.check_with_transient_allow_rules(tool_name, &args, transient_allowed_tool_rules) {
        crate::permissions::CheckResult::Allowed => {
            Some(PermissionOutcome::Allowed { checked: true })
        }
        crate::permissions::CheckResult::Denied(reason) => Some(permission_denied_with_result(
            tool_name,
            tool_call_id,
            &format!("Permission denied: {reason}"),
            &format!("[DENIED] Permission denied: {reason}"),
            tx,
        )),
        crate::permissions::CheckResult::NeedsPrompt { .. } => None,
    }
}

async fn permission_request_hook_outcome(
    tool_name: &str,
    tool_call_id: &str,
    arguments: &str,
    session_id: Option<&str>,
    hook_engine: Option<&crate::hooks::HookEngine>,
    tx: &mpsc::Sender<AppEvent>,
) -> Option<PermissionOutcome> {
    let engine = hook_engine?;
    let tool_input = serde_json::from_str::<Value>(arguments)
        .unwrap_or_else(|_| serde_json::json!({ "raw_arguments": arguments }));
    let mut input = crate::hooks::HookInput::new(crate::hooks::HookEvent::PermissionRequest)
        .with_tool(tool_name, tool_input)
        .with_extra(
            "tool_call_id",
            serde_json::Value::String(tool_call_id.to_string()),
        );
    if let Some(session_id) = session_id {
        input = input.with_session_id(session_id);
    }

    let result = engine
        .run(crate::hooks::HookEvent::PermissionRequest, &input)
        .await;
    if result.allowed {
        return None;
    }

    let reason = result
        .outputs
        .iter()
        .find_map(|output| output.reason.as_deref())
        .unwrap_or("Permission request blocked by hook");
    Some(permission_denied_with_result(
        tool_name,
        tool_call_id,
        &format!("Permission request blocked by hook: {reason}"),
        &format!("[DENIED] Permission request blocked by hook: {reason}"),
        tx,
    ))
}

/// Check whether a tool call is permitted in the current session.
///
/// Consults batch/session deny caches first, then the `PermissionManager`
/// (so hard-safety denials and config auto-allows win), then batch/session
/// allow caches, then `PermissionRequest` hooks, and finally sends a
/// `PermissionRequest` event and `.await`s the user's decision via a tokio
/// `oneshot` if no rule matches.
///
/// `async` so the reply wait yields the runtime — under
/// `flavor = "current_thread"` a synchronous `mpsc::recv` here would
/// pin the only thread and deadlock the main TUI loop (which is the
/// one that has to deliver the user's response).
async fn check_tool_permission(
    tool_name: &str,
    tool_call_id: &str,
    arguments: &str,
    always_allowed: &mut std::collections::HashSet<String>,
    always_denied: &mut std::collections::HashSet<String>,
    permission_mgr: Option<&PermissionManager>,
    transient_allowed_tool_rules: &[PermissionRule],
    hook_engine: Option<&crate::hooks::HookEngine>,
    session_id: Option<&str>,
    tx: &mpsc::Sender<AppEvent>,
) -> PermissionOutcome {
    // Batch-scoped cache (this invocation of execute_tool_calls_for_tui).
    if always_denied.contains(tool_name) {
        return permission_denied_with_result(
            tool_name,
            tool_call_id,
            "Denied (always deny for this session)",
            "[DENIED] User denied permission for this tool.",
            tx,
        );
    }
    if always_allowed.contains(tool_name) && permission_mgr.is_none() {
        return PermissionOutcome::Allowed { checked: false };
    }
    // Session-scoped cache (crosslink #724 — survives across batches).
    let mut session_always_allowed = false;
    if let Some(mgr) = permission_mgr {
        if mgr.tui_is_always_denied(tool_name) {
            return permission_denied_with_result(
                tool_name,
                tool_call_id,
                "Denied (always deny for this session)",
                "[DENIED] User denied permission for this tool.",
                tx,
            );
        }
        if mgr.tui_is_always_allowed(tool_name) {
            session_always_allowed = true;
        }
        if let Some(outcome) = permission_manager_outcome_for_tui(
            tool_name,
            tool_call_id,
            arguments,
            mgr,
            transient_allowed_tool_rules,
            tx,
        ) {
            return outcome;
        }
    }

    if always_allowed.contains(tool_name) || session_always_allowed {
        return PermissionOutcome::Allowed {
            checked: permission_mgr.is_some(),
        };
    }

    if let Some(outcome) = permission_request_hook_outcome(
        tool_name,
        tool_call_id,
        arguments,
        session_id,
        hook_engine,
        tx,
    )
    .await
    {
        return outcome;
    }

    let args_preview = if arguments.len() > 200 {
        format!("{}...", crate::tools::safe_truncate(arguments, 197))
    } else {
        arguments.to_string()
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
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
    match reply_rx.await {
        Ok(PermissionResponse::Allow) => PermissionOutcome::Allowed {
            checked: permission_mgr.is_some(),
        },
        Ok(PermissionResponse::AlwaysAllow) => {
            always_allowed.insert(tool_name.to_string());
            // Persist for the rest of the session (crosslink #724).
            if let Some(mgr) = permission_mgr {
                mgr.tui_remember_always_allowed(tool_name.to_string());
            }
            PermissionOutcome::Allowed {
                checked: permission_mgr.is_some(),
            }
        }
        Ok(PermissionResponse::AlwaysDeny) => {
            always_denied.insert(tool_name.to_string());
            // Persist for the rest of the session (crosslink #724).
            if let Some(mgr) = permission_mgr {
                mgr.tui_remember_always_denied(tool_name.to_string());
            }
            permission_denied_with_result(
                tool_name,
                tool_call_id,
                "Denied (always deny)",
                "[DENIED] User denied permission.",
                tx,
            )
        }
        Ok(PermissionResponse::Deny) | Err(_) => permission_denied_with_result(
            tool_name,
            tool_call_id,
            "Denied by user",
            "[DENIED] User denied permission.",
            tx,
        ),
    }
}

struct ToolPermissionDispatch {
    mgr: Option<Arc<PermissionManager>>,
    already_checked: bool,
}

/// Execute one tool call on a blocking thread, fire `PostToolUse` hooks, and
/// return the JSON result to append to conversation history.
/// Returns `None` when the event channel is broken (caller should `break`).
async fn execute_single_tool(
    tool_call: &ToolCall,
    memory_db: Option<Arc<MemoryDb>>,
    permission: ToolPermissionDispatch,
    task_mgr: Arc<Mutex<crate::session::TaskManager>>,
    session_id: Option<&str>,
    hook_context: Option<(&crate::hooks::HookEngine, Value)>,
    tx: &mpsc::Sender<AppEvent>,
) -> Option<Value> {
    let tool_name = &tool_call.function.name;
    let tool_call_clone = tool_call.clone();
    let mem_db = memory_db;
    let perm_mgr = permission.mgr;
    let permission_already_checked_for_blocking = permission.already_checked;
    let session_for_task = session_id.map(str::to_string);
    let task_mgr_for_blocking = task_mgr;
    let result = tokio::task::spawn_blocking(move || {
        let _session_guard = session_for_task.map(tools::SessionIdGuard::set);
        // Lock the TaskManager only inside the blocking thread so we
        // don't hold the mutex across `.await`. Failure-mode parity with
        // the legacy "no session" branch: poisoned mutex → recover the
        // inner data rather than panicking, so a single panicking task
        // tool doesn't take down the entire TUI session.
        let mut task_guard = task_mgr_for_blocking
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if permission_already_checked_for_blocking {
            tools::execute_tool_with_tasks_unchecked(
                &tool_call_clone,
                mem_db.as_deref(),
                None,
                Some(&mut *task_guard),
            )
        } else {
            tools::execute_tool_with_tasks(
                &tool_call_clone,
                mem_db.as_deref(),
                None,
                Some(&mut *task_guard),
                perm_mgr.as_deref(),
            )
        }
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
    if let Some((engine, tool_input)) = hook_context {
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
fn describe_tool_call(tool_name: &str, args: &Value) -> String {
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
        "crosslink" => args.get("args").and_then(|v| v.as_str()).map_or_else(
            || "Running crosslink".to_string(),
            |a| format!("crosslink {a}"),
        ),
        _ => format!("Running {tool_name}"),
    }
}

fn parse_tool_arguments_for_tui(tool_name: &str, arguments: &str) -> Result<Value, String> {
    let value = serde_json::from_str::<Value>(arguments)
        .map_err(|e| format!("Invalid tool arguments JSON for '{tool_name}': {e}"))?;
    if !value.is_object() {
        return Err(format!(
            "Invalid tool arguments JSON for '{tool_name}': expected a JSON object, got {}",
            json_value_type_name(&value)
        ));
    }
    Ok(value)
}

fn malformed_tool_arguments_result(
    tool_call: &ToolCall,
    msg: &str,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<Value, ()> {
    tx.send(AppEvent::ToolDone {
        name: tool_call.function.name.clone(),
        success: false,
        content: msg.to_string(),
    })
    .map_err(|_| ())?;

    Ok(serde_json::json!({
        "role": "tool",
        "tool_call_id": tool_call.id,
        "content": format!("[ERROR] {msg}"),
        "is_error": true
    }))
}

/// Return the effective path that the pipeline should pre-check with
/// guardrails before read/search tool execution.
///
/// Write-like tools intentionally return `None` here because their handlers
/// already call `guardrails::check_file_access` at the mutation boundary.
fn guardrail_path_for_tool_call(tool_name: &str, args: &Value) -> Option<String> {
    let args = args.as_object()?;

    match tool_name {
        "read_file" => args.get("path").and_then(Value::as_str).map(str::to_string),
        "list_files" | "glob" | "grep" => Some(
            args.get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string(),
        ),
        _ => None,
    }
}

fn guardrail_block_for_tool_call(tool_name: &str, args: &Value) -> Option<String> {
    let path = guardrail_path_for_tool_call(tool_name, args)?;
    crate::guardrails::check_file_access(&path).err()
}

fn emit_failed_quality_gate_events(tx: &mpsc::Sender<AppEvent>) {
    for gate in crate::guardrails::run_quality_gates() {
        if gate.passed {
            continue;
        }
        if tx
            .send(AppEvent::StreamText(format!(
                "\n⚠ Quality gate '{}': {}\n",
                gate.name,
                gate.stdout.lines().next().unwrap_or("failed")
            )))
            .is_err()
        {
            tracing::warn!("TUI channel closed during tool execution");
            break;
        }
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
    transient_allowed_tool_rules: &[PermissionRule],
    hook_engine: Option<Arc<crate::hooks::HookEngine>>,
    task_mgr: Arc<Mutex<crate::session::TaskManager>>,
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
        let tool_args = match parse_tool_arguments_for_tui(tool_name, &tool_call.function.arguments)
        {
            Ok(args) => args,
            Err(msg) => match malformed_tool_arguments_result(tool_call, &msg, tx) {
                Ok(result_json) => {
                    results.push(result_json);
                    continue;
                }
                Err(()) => break,
            },
        };

        // Check read/search blast radius guardrails against the effective
        // filesystem path, not the raw JSON argument envelope.
        if let Some(msg) = guardrail_block_for_tool_call(tool_name, &tool_args) {
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
        let mut permission_already_checked = false;
        if tool_needs_permission(tool_name) {
            match check_tool_permission(
                tool_name,
                &tool_call.id,
                &tool_call.function.arguments,
                &mut always_allowed,
                &mut always_denied,
                permission_mgr.as_deref(),
                transient_allowed_tool_rules,
                hook_engine.as_deref(),
                session_id,
                tx,
            )
            .await
            {
                PermissionOutcome::Allowed { checked } => {
                    permission_already_checked = checked;
                }
                PermissionOutcome::DeniedWithResult(result_json) => {
                    results.push(result_json);
                    continue;
                }
                PermissionOutcome::ChannelBroken => break,
            }
        }

        let args_desc = describe_tool_call(tool_name, &tool_args);
        let hook_context = hook_engine
            .as_ref()
            .map(|engine| (Arc::as_ref(engine), tool_args.clone()));
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
            ToolPermissionDispatch {
                mgr: permission_mgr.clone(),
                already_checked: permission_already_checked,
            },
            task_mgr.clone(),
            session_id,
            hook_context,
            tx,
        )
        .await;
        match tool_result {
            None => break, // channel broken
            Some(mut result_json) => {
                // ask_user_question bridge — see `intercept_user_question`.
                // Returns `Err(())` only when the AppEvent channel is dead,
                // matching the existing break-on-broken-channel semantics.
                if intercept_user_question(&mut result_json, tx).await.is_err() {
                    break;
                }
                results.push(result_json);
            }
        }
    }

    emit_failed_quality_gate_events(tx);

    (results, true)
}

/// Bridge the sync `ask_user_question` tool's `USER_QUESTION_MARKER`
/// payload onto the full-screen TUI's modal flow.
///
/// The sync tool returns a JSON object of shape `{"type":
/// "user_question", "questions": [...]}` as the tool-result content.
/// The REPL intercepts that via `process_tool_result_marker` and
/// blocks on stdin (`handle_user_questions`). Under the full-screen
/// TUI we route the question set to a modal via `AppEvent::
/// UserQuestion`, park on a oneshot for the answer JSON, and
/// rewrite `result_json["content"]` so the model only ever sees
/// the user's answers — never the raw marker.
///
/// Returns `Err(())` only when the `AppEvent` channel is dead
/// (TUI shut down mid-turn). Tool-result payloads that aren't a
/// `user_question` marker are returned unchanged and `Ok(())`.
async fn intercept_user_question(
    result_json: &mut Value,
    tx: &mpsc::Sender<AppEvent>,
) -> Result<(), ()> {
    let Some(content) = result_json.get("content").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if !matches!(
        crate::tools::parse_tool_control_signal(content),
        Some(crate::tools::ToolControlSignal::UserQuestion)
    ) {
        return Ok(());
    }
    let Some(questions) = crate::tools::parse_user_questions(content) else {
        return Ok(());
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if tx
        .send(AppEvent::UserQuestion {
            questions,
            reply: reply_tx,
        })
        .is_err()
    {
        return Err(());
    }

    // Modal dropped the sender (e.g. user cancelled with Ctrl+C) →
    // surface a structured `_cancelled: true` payload to the agent
    // instead of hanging.
    let answers = reply_rx
        .await
        .unwrap_or_else(|_| "{\"_cancelled\": true}".to_string());
    if let Some(obj) = result_json.as_object_mut() {
        obj.insert("content".to_string(), Value::String(answers));
    }
    Ok(())
}

/// Build the assistant message with tool calls for appending to conversation history.
#[must_use]
pub fn build_assistant_message_with_tools(
    content: &str,
    reasoning_content: Option<&str>,
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

    let mut message = serde_json::json!({
        "role": "assistant",
        "content": Value::String(content.to_string()),
        "tool_calls": tool_calls_json
    });
    if let Some(reasoning) = reasoning_content.filter(|text| !text.is_empty()) {
        message["reasoning_content"] = Value::String(reasoning.to_string());
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardrail_path_for_tool_call_uses_actual_read_path() {
        let args = serde_json::json!({"path":"src/main.rs"});

        assert_eq!(
            guardrail_path_for_tool_call("read_file", &args),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn guardrail_path_for_tool_call_defaults_read_search_paths() {
        let args = serde_json::json!({});

        for tool_name in ["list_files", "glob", "grep"] {
            assert_eq!(
                guardrail_path_for_tool_call(tool_name, &args),
                Some(".".to_string()),
                "{tool_name} should precheck its executor's default path"
            );
        }
    }

    #[test]
    fn guardrail_path_for_tool_call_matches_optional_path_type_semantics() {
        let args = serde_json::json!({"path":42});

        assert_eq!(
            guardrail_path_for_tool_call("list_files", &args),
            Some(".".to_string())
        );
        assert_eq!(guardrail_path_for_tool_call("read_file", &args), None);
    }

    #[test]
    fn guardrail_path_for_tool_call_skips_non_object_arguments() {
        let args = serde_json::json!([]);

        assert_eq!(guardrail_path_for_tool_call("list_files", &args), None);
    }

    #[test]
    fn guardrail_path_for_tool_call_skips_write_tools_checked_by_handlers() {
        let write_args = serde_json::json!({"path":"src/main.rs","content":"new"});
        let edit_args = serde_json::json!({"path":"src/main.rs"});
        let notebook_args = serde_json::json!({"notebook_path":"nb.ipynb"});

        assert_eq!(
            guardrail_path_for_tool_call("write_file", &write_args),
            None
        );
        assert_eq!(guardrail_path_for_tool_call("edit_file", &edit_args), None);
        assert_eq!(
            guardrail_path_for_tool_call("notebook_edit", &notebook_args),
            None
        );
    }

    #[test]
    fn parse_tool_arguments_for_tui_rejects_malformed_and_non_object_json() {
        let malformed = parse_tool_arguments_for_tui("bash", "{not json")
            .expect_err("malformed tool args must fail before TUI prompting");
        assert!(
            malformed.contains("Invalid tool arguments JSON"),
            "{malformed}"
        );
        assert!(malformed.contains("bash"), "{malformed}");

        let non_object = parse_tool_arguments_for_tui("bash", "[]")
            .expect_err("non-object tool args must fail before TUI prompting");
        assert!(
            non_object.contains("expected a JSON object"),
            "{non_object}"
        );
        assert!(non_object.contains("array"), "{non_object}");
    }

    #[tokio::test]
    async fn execute_tool_calls_for_tui_rejects_malformed_arguments_before_prompting() {
        use std::sync::mpsc as std_mpsc;

        let tool_call = ToolCall {
            id: "call_bad".to_string(),
            call_type: "function".to_string(),
            function: tools::FunctionCall {
                name: "bash".to_string(),
                arguments: "{not json".to_string(),
            },
        };
        let (tx, rx) = std_mpsc::channel::<AppEvent>();
        let task_mgr = Arc::new(Mutex::new(crate::session::TaskManager::new()));

        let (results, has_tools) = execute_tool_calls_for_tui(
            &[tool_call],
            None,
            None,
            &[],
            None,
            task_mgr,
            Some("s"),
            &tx,
        )
        .await;

        assert!(has_tools);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["is_error"], true);
        assert!(
            results[0]["content"]
                .as_str()
                .is_some_and(|content| content.contains("Invalid tool arguments JSON")),
            "tool result should carry the parse error: {}",
            results[0]
        );

        let mut saw_tool_done = false;
        let mut saw_permission_request = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::ToolDone { content, .. } => {
                    saw_tool_done = content.contains("Invalid tool arguments JSON");
                }
                AppEvent::PermissionRequest { .. } => saw_permission_request = true,
                _ => {}
            }
        }

        assert!(saw_tool_done, "TUI should receive the parse failure");
        assert!(
            !saw_permission_request,
            "malformed arguments must not trigger a permission prompt"
        );
    }

    #[tokio::test]
    async fn execute_tool_calls_for_tui_one_time_allow_executes_without_nested_prompt() {
        use std::sync::mpsc as std_mpsc;
        use std::time::Duration;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let mgr = Arc::new(PermissionManager::new_with_web_fetch_preapproved(
            dir.path().join("permissions.json"),
            true,
            Vec::new(),
            Vec::new(),
        ));
        let tool_call = ToolCall {
            id: "call_allow_once".to_string(),
            call_type: "function".to_string(),
            function: tools::FunctionCall {
                name: "bash".to_string(),
                arguments: r#"{"command":"printf tui-permission-ok"}"#.to_string(),
            },
        };
        let (tx, rx) = std_mpsc::channel::<AppEvent>();
        let task_mgr = Arc::new(Mutex::new(crate::session::TaskManager::new()));

        let handle = tokio::spawn({
            let task_mgr = Arc::clone(&task_mgr);
            let mgr = Arc::clone(&mgr);
            async move {
                let tool_calls = vec![tool_call];
                execute_tool_calls_for_tui(
                    &tool_calls,
                    None,
                    Some(mgr),
                    &[],
                    None,
                    task_mgr,
                    Some("s"),
                    &tx,
                )
                .await
            }
        });

        let reply = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.try_recv() {
                    Ok(AppEvent::PermissionRequest { reply, .. }) => break reply,
                    Ok(_) | Err(std_mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(std_mpsc::TryRecvError::Disconnected) => {
                        panic!("tool runner disconnected before permission prompt")
                    }
                }
            }
        })
        .await
        .expect("permission prompt should arrive");

        reply
            .send(PermissionResponse::Allow)
            .expect("tool runner should still be awaiting permission reply");

        let (results, has_tools) = handle.await.expect("tool runner should not panic");
        assert!(has_tools);
        assert_eq!(results.len(), 1);
        let content = results[0]["content"].as_str().unwrap_or_default();
        assert!(
            content.contains("tui-permission-ok"),
            "one-time Allow should execute the tool, got: {content}"
        );
        assert!(
            !content.contains("PERMISSION_PROMPT"),
            "one-time Allow must not leak nested legacy permission prompts: {content}"
        );
        assert_eq!(results[0]["is_error"], false);
    }

    #[test]
    fn extract_google_text_concatenates_text_parts_and_allows_tool_calls() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "hello "},
                        {"functionCall": {"name": "bash", "args": {"command": "pwd"}}},
                        {"text": "world"}
                    ]
                }
            }]
        });

        let parts = google_response_parts(&body).expect("parts should parse");
        let text = extract_google_text(parts).expect("mixed text/tool response should parse");

        assert_eq!(text, "hello world");
    }

    #[test]
    fn google_response_parts_rejects_missing_parts() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {}
            }]
        });

        let err = google_response_parts(&body).expect_err("missing parts must fail");

        assert!(err.contains("content.parts"), "{err}");
    }

    #[test]
    fn extract_google_text_rejects_non_string_text_part() {
        let parts = vec![serde_json::json!({"text": 123})];

        let err = extract_google_text(&parts).expect_err("non-string text must fail");

        assert!(err.contains("'text'"), "{err}");
    }

    #[test]
    fn extract_google_text_rejects_unsupported_part_shape() {
        let parts = vec![serde_json::json!({
            "inlineData": {"mimeType": "image/png", "data": "..."}
        })];

        let err = extract_google_text(&parts).expect_err("unsupported part must fail");

        assert!(err.contains("supported text or functionCall"), "{err}");
    }

    #[test]
    fn extract_google_tool_calls_accepts_valid_function_call() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "using a tool"},
                        {"functionCall": {"name": "bash", "args": {"command": "pwd"}}}
                    ]
                }
            }]
        });

        let calls = extract_google_tool_calls(&body).expect("valid Gemini tool call should parse");

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "bash");
        assert_eq!(calls[0].function.arguments, r#"{"command":"pwd"}"#);
    }

    #[test]
    fn extract_google_tool_calls_rejects_missing_name() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"args": {"command": "pwd"}}}
                    ]
                }
            }]
        });

        let err = extract_google_tool_calls(&body).expect_err("missing Gemini tool name must fail");

        assert!(err.contains("functionCall"), "{err}");
        assert!(err.contains("name"), "{err}");
    }

    #[test]
    fn extract_google_tool_calls_rejects_missing_args() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "bash"}}
                    ]
                }
            }]
        });

        let err = extract_google_tool_calls(&body).expect_err("missing Gemini tool args must fail");

        assert!(err.contains("functionCall"), "{err}");
        assert!(err.contains("args"), "{err}");
    }

    #[test]
    fn extract_google_tool_calls_rejects_non_object_args() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "bash", "args": []}}
                    ]
                }
            }]
        });

        let err =
            extract_google_tool_calls(&body).expect_err("non-object Gemini tool args must fail");

        assert!(err.contains("args"), "{err}");
        assert!(err.contains("object"), "{err}");
    }

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
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None)
            .expect("anthropic request should build");
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
        )
        .expect("anthropic request should build");
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
        )
        .expect("anthropic request should build");
        let sys = req["system"].as_array().unwrap();
        // Empty suffix collapses to single cached block
        assert_eq!(sys.len(), 1);
        assert!(sys[0]["cache_control"].is_object());
    }

    #[test]
    fn test_build_request_dispatches() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let req = build_request("openai", "gpt-4", &messages, "medium", None, None)
            .expect("openai request should build");
        assert_eq!(req["model"], "gpt-4");

        let req = build_request(
            "anthropic",
            "claude-sonnet-4-6",
            &messages,
            "medium",
            None,
            None,
        )
        .expect("anthropic request should build");
        assert_eq!(req["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn build_request_openai_high_effort_uses_reasoning_models_only() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let gpt5 = build_request("openai", "gpt-5.5", &messages, "high", None, None)
            .expect("gpt-5 request should build");
        assert_eq!(gpt5["reasoning_effort"], "high");

        let gpt4 = build_request("openai", "gpt-4o", &messages, "high", None, None)
            .expect("gpt-4o request should build");
        assert!(
            gpt4.get("reasoning_effort").is_none(),
            "non-reasoning OpenAI models must not receive reasoning_effort: {gpt4}"
        );
    }

    #[test]
    fn build_request_provider_specific_thinking_fields_are_used() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];

        let deepseek = build_request("deepseek", "deepseek-v4-pro", &messages, "high", None, None)
            .expect("deepseek request should build");
        assert_eq!(deepseek["thinking"]["type"], "enabled");
        assert_eq!(deepseek["reasoning_effort"], "high");
        assert!(
            deepseek.get("enable_thinking").is_none(),
            "DeepSeek must not receive legacy enable_thinking: {deepseek}"
        );

        let deepseek_max =
            build_request("deepseek", "deepseek-v4-pro", &messages, "max", None, None)
                .expect("deepseek max request should build");
        assert_eq!(deepseek_max["reasoning_effort"], "max");

        let qwen = build_request("qwen", "qwen3.7-plus", &messages, "high", None, None)
            .expect("qwen request should build");
        assert_eq!(qwen["enable_thinking"], true);
        assert!(
            qwen.get("reasoning_effort").is_none(),
            "Qwen must not receive OpenAI reasoning_effort: {qwen}"
        );

        let zai = build_request("zai", "glm-5.2", &messages, "high", None, None)
            .expect("zai request should build");
        assert_eq!(zai["thinking"]["type"], "enabled");
        assert_eq!(zai["reasoning_effort"], "high");

        let zai_legacy = build_request("zai", "glm-4.7", &messages, "max", None, None)
            .expect("zai legacy request should build");
        assert_eq!(zai_legacy["thinking"]["type"], "enabled");
        assert!(
            zai_legacy.get("reasoning_effort").is_none(),
            "non-GLM-5.2 Z.AI models must not receive reasoning_effort: {zai_legacy}"
        );

        let minimax = build_request("minimax", "MiniMax-M3", &messages, "high", None, None)
            .expect("minimax request should build");
        assert_eq!(minimax["thinking"]["type"], "adaptive");
        assert_eq!(minimax["reasoning_split"], true);
        assert!(
            minimax.get("reasoning_effort").is_none(),
            "MiniMax must not receive OpenAI reasoning_effort: {minimax}"
        );
    }

    #[test]
    fn build_request_omits_unsupported_generic_thinking_fields() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        for (provider, model) in [("kimi", "kimi-k2.7-code"), ("moonshot", "kimi-k2.7-code")] {
            let body = build_request(provider, model, &messages, "high", None, None)
                .expect("request should build");
            for field in [
                "reasoning_effort",
                "enable_thinking",
                "thinking",
                "clear_thinking",
            ] {
                assert!(
                    body.get(field).is_none(),
                    "{provider} must not receive unsupported field {field}: {body}"
                );
            }
        }
    }

    #[test]
    fn build_request_routes_provider_aliases_to_native_shapes() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];

        let gemini = build_request(
            "gemini",
            "gemini-3.5-flash",
            &messages,
            "medium",
            None,
            None,
        )
        .expect("gemini alias request should build");
        assert!(gemini.get("contents").is_some());
        assert!(
            gemini.get("messages").is_none(),
            "gemini alias must use native Gemini request shape: {gemini}"
        );

        let ollama = build_request("ollama", "llama3", &messages, "medium", None, None)
            .expect("ollama request should build");
        assert_eq!(ollama["stream"], true);
        assert!(ollama["options"]["num_predict"].is_number());
        assert!(
            ollama.get("max_tokens").is_none(),
            "Ollama must use native options.num_predict, not OpenAI max_tokens: {ollama}"
        );
    }

    #[test]
    fn build_request_errors_on_unknown_provider() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let err = build_request("anthrpic", "gpt-5.5", &messages, "medium", None, None)
            .expect_err("unknown provider must not silently fall back to OpenAI");
        assert!(err.contains("Unknown provider"), "{err}");
        assert!(err.contains("anthrpic"), "{err}");
    }

    #[test]
    fn build_request_errors_on_malformed_anthropic_tool_call_arguments() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "run a tool"}),
            serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "toolu_bad",
                    "type": "function",
                    "function": {"name": "bash", "arguments": "{not json"}
                }]
            }),
        ];
        let err = build_request(
            "anthropic",
            "claude-sonnet-4-6",
            &messages,
            "medium",
            None,
            None,
        )
        .expect_err("malformed tool_call arguments must reject Anthropic request build");
        assert!(err.contains("function.arguments"), "{err}");
        assert!(err.contains("invalid JSON"), "{err}");
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
        let msg = build_assistant_message_with_tools("hello", None, &tool_calls, "anthropic");
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "hello");
        assert!(msg["tool_calls"].is_array());
        assert_eq!(msg["tool_calls"][0]["id"], "call_123");
    }

    #[test]
    fn build_assistant_message_with_tools_preserves_reasoning_content() {
        let msg = build_assistant_message_with_tools("hello", Some("thought"), &[], "kimi");
        assert_eq!(msg["reasoning_content"], "thought");
    }

    #[test]
    fn merge_reasoning_delta_deduplicates_cumulative_chunks() {
        let mut buffer = String::new();

        assert_eq!(merge_reasoning_delta(&mut buffer, "abc"), "abc");
        assert_eq!(merge_reasoning_delta(&mut buffer, "abcdef"), "def");
        assert_eq!(buffer, "abcdef");
        assert_eq!(merge_reasoning_delta(&mut buffer, " + next"), " + next");
        assert_eq!(buffer, "abcdef + next");
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

        let high = build_anthropic_request("claude-sonnet-4-6", &messages, "high", None, None)
            .expect("high effort anthropic request should build");
        assert_eq!(
            high["thinking"]["budget_tokens"],
            crate::thinking::ULTRATHINK_BUDGET_TOKENS,
        );
        assert_eq!(high["max_tokens"], 40_000);

        let maxr = build_anthropic_request("claude-sonnet-4-6", &messages, "max", None, None)
            .expect("max effort anthropic request should build");
        assert_eq!(
            maxr["thinking"]["budget_tokens"],
            crate::thinking::ULTRATHINK_BUDGET_TOKENS,
        );

        let low = build_anthropic_request("claude-sonnet-4-6", &messages, "low", None, None)
            .expect("low effort anthropic request should build");
        assert!(low.get("thinking").is_none());
        assert_eq!(low["max_tokens"], 2048);

        let med = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None)
            .expect("medium effort anthropic request should build");
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
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None)
            .expect("medium effort anthropic request should build");
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
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "high", None, None)
            .expect("high effort anthropic request should build");
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
    /// Gemini thinking is capped at 32768.
    #[test]
    fn b2_google_request_thinking_budget_capped() {
        const GEMINI_CAP: u64 = 32_768;
        let prev = std::env::var("MAX_THINKING_TOKENS").ok();
        // SAFETY: single-threaded test, no concurrent writers.
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
        }
        let messages = vec![serde_json::json!({"role": "user", "content": "think"})];
        let req =
            build_google_request(&messages, "high").expect("google high-effort request builds");
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

    #[test]
    fn google_request_rejects_malformed_message_history() {
        let missing_role = vec![serde_json::json!({"content": "hi"})];
        let err = build_google_request(&missing_role, "medium")
            .expect_err("missing message role must fail");
        assert!(err.contains("'role'"), "{err}");
        assert!(err.contains("index 0"), "{err}");

        let missing_content = vec![serde_json::json!({"role": "user"})];
        let err = build_google_request(&missing_content, "medium")
            .expect_err("missing message content must fail");
        assert!(err.contains("'content'"), "{err}");
        assert!(err.contains("index 0"), "{err}");

        let unsupported_role = vec![serde_json::json!({"role": "developer", "content": "hi"})];
        let err = build_google_request(&unsupported_role, "medium")
            .expect_err("unsupported role must fail");
        assert!(err.contains("unsupported role"), "{err}");
        assert!(err.contains("developer"), "{err}");
    }

    #[test]
    fn google_request_concatenates_all_system_messages() {
        let messages = vec![
            serde_json::json!({"role": "system", "content": "first"}),
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "system", "content": "second"}),
        ];

        let req = build_google_request(&messages, "medium").expect("google request should build");

        assert_eq!(
            req["systemInstruction"]["parts"][0]["text"],
            "first\n\nsecond"
        );
        let contents = req["contents"].as_array().expect("contents array");
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
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

    /// B1 — request builders keep the streaming flag contract separate from
    /// retry classification.
    #[test]
    fn b1_build_request_stream_flag_always_set() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let req = build_openai_request("gpt-4", &messages, "medium");
        assert_eq!(
            req["stream"], true,
            "stream must always be true in OC requests"
        );
        let req = build_anthropic_request("claude-sonnet-4-6", &messages, "medium", None, None)
            .expect("anthropic request should build");
        assert_eq!(req["stream"], true);
        let req = build_google_request(&messages, "medium").expect("google medium request builds");
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
    #[tokio::test]
    async fn issue_724_check_tool_permission_uses_session_always_allowed() {
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
            &[],
            None,
            None,
            &tx,
        )
        .await;
        assert!(
            matches!(outcome, PermissionOutcome::Allowed { checked: true }),
            "#724: a prior 'always allow' must short-circuit to Allowed without a prompt"
        );
        // No PermissionRequest event should have been emitted.
        assert!(
            rx.try_recv().is_err(),
            "#724: no PermissionRequest event must be sent when the session cache allows"
        );
    }

    /// #603: `web_fetch` is gated, but a configured preapproved host should
    /// be allowed by the permission manager without bothering the TUI.
    #[tokio::test]
    async fn issue_603_check_tool_permission_allows_preapproved_web_fetch_without_prompt() {
        use std::sync::mpsc as std_mpsc;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let mgr = PermissionManager::new_with_web_fetch_preapproved(
            dir.path().join("permissions.json"),
            true,
            Vec::new(),
            vec!["docs.python.org".to_string()],
        );
        let mut always_allowed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut always_denied: std::collections::HashSet<String> = std::collections::HashSet::new();

        let (tx, rx) = std_mpsc::channel::<AppEvent>();
        let outcome = check_tool_permission(
            "web_fetch",
            "call_web",
            r#"{"url":"https://docs.python.org/3/"}"#,
            &mut always_allowed,
            &mut always_denied,
            Some(&mgr),
            &[],
            None,
            None,
            &tx,
        )
        .await;
        assert!(
            matches!(outcome, PermissionOutcome::Allowed { checked: true }),
            "#603: preapproved web_fetch URL must be allowed without a prompt"
        );
        assert!(
            rx.try_recv().is_err(),
            "#603: no PermissionRequest event should be sent for preapproved web_fetch"
        );
    }

    #[tokio::test]
    async fn check_tool_permission_allows_matching_transient_rule_without_prompt() {
        use crate::permissions::{PermissionDecision, PermissionRule};
        use std::sync::mpsc as std_mpsc;
        use tempfile::TempDir;

        let dir = TempDir::new().expect("tempdir");
        let mgr = PermissionManager::new(dir.path().join("permissions.json"), true, Vec::new());
        let transient = [PermissionRule {
            tool: "Bash".to_string(),
            pattern: "git status *".to_string(),
            decision: PermissionDecision::Allow,
        }];
        let mut always_allowed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut always_denied: std::collections::HashSet<String> = std::collections::HashSet::new();

        let (tx, rx) = std_mpsc::channel::<AppEvent>();
        let outcome = check_tool_permission(
            "bash",
            "call_git",
            r#"{"command":"git status --short"}"#,
            &mut always_allowed,
            &mut always_denied,
            Some(&mgr),
            &transient,
            None,
            None,
            &tx,
        )
        .await;

        assert!(
            matches!(outcome, PermissionOutcome::Allowed { checked: true }),
            "matching transient allowed-tools rule must allow without prompting"
        );
        assert!(
            rx.try_recv().is_err(),
            "transient allowed-tools rule must not emit a PermissionRequest"
        );
    }

    #[tokio::test]
    async fn permission_request_hook_can_deny_before_tui_prompt() {
        use crate::config::{Hook, HookEntry, HooksConfig};
        use crate::hooks::HookEngine;
        use std::sync::mpsc as std_mpsc;

        let mut hooks = HooksConfig::default();
        hooks.permission_request.push(HookEntry {
            matcher: Some("bash".to_string()),
            hooks: vec![Hook::Command {
                command: r#"printf '{"decision":"deny","reason":"hook veto"}'"#.to_string(),
                shell: false,
                timeout: 5,
            }],
        });
        let engine = HookEngine::new(hooks);
        let mut always_allowed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut always_denied: std::collections::HashSet<String> = std::collections::HashSet::new();
        let (tx, rx) = std_mpsc::channel::<AppEvent>();

        let outcome = check_tool_permission(
            "bash",
            "call_hook",
            r#"{"command":"rm -rf /tmp/openclaudia-hook-test"}"#,
            &mut always_allowed,
            &mut always_denied,
            None,
            &[],
            Some(&engine),
            Some("session-1"),
            &tx,
        )
        .await;

        let PermissionOutcome::DeniedWithResult(result) = outcome else {
            panic!("permission hook denial must return DeniedWithResult");
        };
        assert_eq!(result["tool_call_id"], "call_hook");
        assert!(
            result["content"]
                .as_str()
                .is_some_and(|content| content.contains("hook veto")),
            "model-facing denial must include hook reason: {result}"
        );

        let mut saw_permission_request = false;
        let mut saw_tool_done = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                AppEvent::PermissionRequest { reply, .. } => {
                    saw_permission_request = true;
                    let _ = reply.send(PermissionResponse::Deny);
                }
                AppEvent::ToolDone {
                    name,
                    success,
                    content,
                } => {
                    saw_tool_done = true;
                    assert_eq!(name, "bash");
                    assert!(!success);
                    assert!(content.contains("hook veto"), "{content}");
                }
                _ => {}
            }
        }

        assert!(
            saw_tool_done,
            "hook denial must emit a ToolDone failure event"
        );
        assert!(
            !saw_permission_request,
            "hook denial must short-circuit before the TUI permission prompt"
        );
    }

    /// crosslink #724 — symmetric to the above: a session-scoped "always deny"
    /// short-circuits to `DeniedWithResult` without prompting the user again.
    #[tokio::test]
    async fn issue_724_check_tool_permission_uses_session_always_denied() {
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
            &[],
            None,
            None,
            &tx,
        )
        .await;
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
        )
        .expect("anthropic request should build");
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

    // === Crosslink #592 #595 #596 #597 retry-classifier regression =========

    /// #597: every status in the CC-parity transient set retries.
    #[test]
    fn issue_597_retryable_statuses_match_cc_set() {
        for status in [408, 409, 429, 500, 502, 503, 504, 529] {
            assert!(
                is_retryable_status(status),
                "{status} must be classified retryable"
            );
        }
        // Non-transient — must NOT retry (4xx that are caller-bug, 2xx happy).
        for status in [200, 201, 400, 401, 403, 404, 422] {
            assert!(
                !is_retryable_status(status),
                "{status} must NOT be classified retryable"
            );
        }
    }

    /// #596: jitter spread must be non-zero so concurrent retriers don't
    /// land on identical waits. Sample a window of attempts and assert
    /// at least two of them differ.
    #[test]
    fn issue_596_backoff_jitter_produces_non_constant_output() {
        let mut seen = std::collections::HashSet::new();
        // 200 samples at attempt=3 → base=16 → jitter ±4 → 9..=24 range.
        // With a healthy nanos source the set should have at least 3
        // distinct values long before we exhaust the loop.
        for _ in 0..200 {
            seen.insert(backoff_with_jitter(3));
            if seen.len() >= 3 {
                break;
            }
        }
        assert!(
            seen.len() >= 2,
            "backoff_with_jitter must produce >=2 distinct waits across 200 samples, saw {seen:?}"
        );
    }

    /// #596: backoff is always at least 1 second even at attempt=0
    /// (saturating arithmetic must never yield 0 sleep — that would
    /// spin-burn the CPU on a stuck transient).
    #[test]
    fn issue_596_backoff_floor_is_one_second() {
        for _ in 0..50 {
            let wait = backoff_with_jitter(0);
            assert!(wait >= 1, "wait must be >=1, got {wait}");
        }
    }

    #[test]
    fn issue_596_retry_after_zero_keeps_zero_delay() {
        assert_eq!(
            retry_after_with_jitter_from(0, u64::MAX),
            std::time::Duration::ZERO,
            "Retry-After: 0 must stay zero so deterministic tests and immediate retry semantics hold"
        );
    }

    #[test]
    fn issue_596_retry_after_jitter_is_additive_and_bounded() {
        let base = std::time::Duration::from_secs(4);
        let max = std::time::Duration::from_secs(5);

        let no_jitter = retry_after_with_jitter_from(4, 0);
        let max_jitter = retry_after_with_jitter_from(4, 1_000);

        assert_eq!(no_jitter, base);
        assert_eq!(max_jitter, max);
        for seed in [1, 42, 999, 1_001, u64::MAX] {
            let wait = retry_after_with_jitter_from(4, seed);
            assert!(
                (base..=max).contains(&wait),
                "Retry-After jitter must stay within 0-25%; seed={seed}, wait={wait:?}"
            );
        }
    }

    #[test]
    fn issue_596_retry_delay_label_formats_fractional_seconds() {
        assert_eq!(retry_delay_label(std::time::Duration::from_secs(3)), "3s");
        assert_eq!(
            retry_delay_label(std::time::Duration::from_millis(1_250)),
            "1.25s"
        );
    }

    /// #592: `max_retries` cap is the CC-parity value (10). Pins via the
    /// constant being public-via-classifier; if the loop's `MAX_RETRIES`
    /// drifts, future test failures will name the value.
    #[test]
    fn issue_592_retry_classifier_and_helper_are_publicly_accessible() {
        // These are exercise tests for the public surface of #595-#597.
        // The actual MAX_RETRIES const lives inside run_turn but the
        // helpers must be callable from elsewhere so they're testable
        // and reusable across seams.
        assert!(is_retryable_status(429));
        let _ = backoff_with_jitter(0);
    }

    // ── crosslink #598 — overload fallback hint ──────────────────────────────

    /// `#598-a`: opus models fall back to sonnet, sonnet to haiku.
    /// Pins the descent through the Claude tiers.
    #[test]
    fn issue_598_claude_family_downgrade_path() {
        assert_eq!(
            overload_fallback_for("claude-opus-4-8"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            overload_fallback_for("claude-sonnet-4-6"),
            "claude-haiku-4-5"
        );
        // Haiku has no further downgrade — empty hint, but the event
        // is still emitted by send_with_retry so log consumers see it.
        assert_eq!(overload_fallback_for("claude-haiku-4-5"), "");
    }

    /// `#598-b`: GPT-5 degrades to mini, GPT-4 / o-series degrade to
    /// gpt-4o-mini, Gemini Pro degrades to Gemini Flash, and unknown
    /// model families return the empty hint.
    #[test]
    fn issue_598_cross_provider_fallback_map() {
        assert_eq!(overload_fallback_for("gpt-5.5"), "gpt-5.4-mini");
        assert_eq!(overload_fallback_for("gpt-5.4"), "gpt-5.4-mini");
        assert_eq!(overload_fallback_for("gpt-5"), "gpt-5-mini");
        assert_eq!(overload_fallback_for("gpt-4-turbo"), "gpt-4o-mini");
        assert_eq!(overload_fallback_for("gpt-4o"), "gpt-4o-mini");
        assert_eq!(overload_fallback_for("o1-preview"), "gpt-4o-mini");
        assert_eq!(overload_fallback_for("o3-mini"), "gpt-4o-mini");
        assert_eq!(
            overload_fallback_for("gemini-3.1-pro-preview"),
            "gemini-3.5-flash"
        );
        // Unknown family — empty hint, distinct from a known mapping.
        assert_eq!(overload_fallback_for("llama-3-70b"), "");
        assert_eq!(overload_fallback_for(""), "");
    }

    /// `#598-c`: the `OverloadFallback` `AppEvent` variant carries a
    /// `String` `model_hint` and can round-trip through a channel. Acts
    /// as the type-level pin so a future enum change that drops the
    /// variant (or its field shape) breaks one test instead of cascading
    /// through every match site silently.
    #[test]
    fn issue_598_overload_fallback_event_round_trips() {
        let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
        tx.send(AppEvent::OverloadFallback {
            model_hint: "claude-haiku-4-5".to_string(),
        })
        .expect("send must succeed on a live channel");
        match rx.recv().expect("event must arrive") {
            AppEvent::OverloadFallback { model_hint } => {
                assert_eq!(model_hint, "claude-haiku-4-5");
            }
            other => panic!(
                "expected OverloadFallback, got {}",
                describe_app_event(&other)
            ),
        }
    }

    fn describe_app_event(ev: &AppEvent) -> &'static str {
        match ev {
            AppEvent::OverloadFallback { .. } => "OverloadFallback",
            _ => "other",
        }
    }
}
