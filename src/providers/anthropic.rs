//! Anthropic Messages API adapter.

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::config::ThinkingConfig;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use crate::session::TokenUsage;

use super::{ApiKey, ProviderAdapter, ProviderError};

/// Anthropic Messages API adapter
pub struct AnthropicAdapter;

impl AnthropicAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Extract system message from messages array
    fn extract_system(messages: &[ChatMessage]) -> Option<String> {
        messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| p.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
    }

    /// Convert `ChatMessage`s to the shape [`convert_messages_to_anthropic`]
    /// expects, then dispatch — the richer converter correctly handles
    /// `role="tool"` → `tool_result` blocks with `tool_use_id` linkage and
    /// assistant `tool_calls` → `tool_use` blocks. Previously this method
    /// had its own simpler conversion that silently dropped tool-result
    /// semantics, causing every agentic tool-loop request to fail with
    /// Anthropic 400 "each `tool_use` must have a matching `tool_result`"
    /// (crosslink #475).
    fn convert_messages(messages: &[ChatMessage]) -> Vec<Value> {
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        convert_messages_to_anthropic(&as_values)
    }

    /// Convert `OpenAI` tools to Anthropic format with optional prompt caching.
    ///
    /// Surfaces malformed entries as `ProviderError::RequestFailed` rather than
    /// silently filtering them out (crosslink #413). Each tool MUST contain a
    /// `function` object with a non-empty string `name`; anything else is an
    /// API contract violation that the caller needs to know about.
    ///
    /// If `cache_last` is true, the last entry gets a `cache_control` marker
    /// for prompt caching.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RequestFailed` if any tool is missing the
    /// `function` object or `function.name` is missing/non-string/empty.
    pub(crate) fn convert_tools_checked(
        tools: &[Value],
        cache_last: bool,
    ) -> Result<Vec<Value>, ProviderError> {
        let len = tools.len();
        let mut out = Vec::with_capacity(len);
        for (i, tool) in tools.iter().enumerate() {
            let func = tool.get("function").ok_or_else(|| {
                ProviderError::RequestFailed(format!(
                    "Tool at index {i} missing required 'function' object: {tool}"
                ))
            })?;
            let name = func
                .get("name")
                .and_then(|n| n.as_str())
                .filter(|n| !n.is_empty())
                .ok_or_else(|| {
                    ProviderError::RequestFailed(format!(
                        "Tool at index {i} missing required 'function.name' string field: {tool}"
                    ))
                })?;

            let mut tool_def = json!({
                "name": name,
                "description": func.get("description").cloned().unwrap_or_else(|| Value::String(String::new())),
                "input_schema": func.get("parameters").cloned().unwrap_or_else(|| Value::Object(serde_json::Map::default()))
            });

            // Add cache_control to the last tool for prompt caching.
            // This caches all tools since cache applies to everything before the marker.
            if cache_last && i + 1 == len {
                tool_def["cache_control"] = json!({"type": "ephemeral"});
            }

            out.push(tool_def);
        }
        Ok(out)
    }

    /// Backwards-compatible infallible wrapper around [`Self::convert_tools_checked`].
    ///
    /// Used by trusted internal call sites that build tool definitions from
    /// [`crate::tools::get_all_tool_definitions`] (known well-formed). On the
    /// off-chance an internal tool definition is malformed, the entry is
    /// logged at `WARN` rather than silently dropped — so that a regression
    /// in our own builder produces forensic evidence in the logs.
    pub(crate) fn convert_tools(tools: &[Value], cache_last: bool) -> Vec<Value> {
        match Self::convert_tools_checked(tools, cache_last) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    error = %e,
                    "convert_tools encountered malformed tool definition from a trusted internal site (crosslink #413); falling back to best-effort filter"
                );
                // Best-effort: keep well-formed entries, surface dropped ones in logs.
                let len = tools.len();
                tools
                    .iter()
                    .enumerate()
                    .filter_map(|(i, tool)| {
                        let func = tool.get("function").or_else(|| {
                            warn!(index = i, tool = %tool, "dropping tool missing 'function' object (crosslink #413)");
                            None
                        })?;
                        let name = func
                            .get("name")
                            .and_then(|n| n.as_str())
                            .filter(|n| !n.is_empty())
                            .or_else(|| {
                                warn!(index = i, tool = %tool, "dropping tool missing 'function.name' (crosslink #413)");
                                None
                            })?;
                        let mut tool_def = json!({
                            "name": name,
                            "description": func.get("description").cloned().unwrap_or_else(|| Value::String(String::new())),
                            "input_schema": func.get("parameters").cloned().unwrap_or_else(|| Value::Object(serde_json::Map::default()))
                        });
                        if cache_last && i + 1 == len {
                            tool_def["cache_control"] = json!({"type": "ephemeral"});
                        }
                        Some(tool_def)
                    })
                    .collect()
            }
        }
    }
}

impl Default for AnthropicAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        let mut body = json!({
            "model": &request.model,
            "messages": Self::convert_messages(&request.messages),
            "max_tokens": request.max_tokens.unwrap_or(crate::DEFAULT_MAX_TOKENS)
        });

        // Add system message if present - use array format with cache_control for prompt caching
        // See: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
        if let Some(system) = Self::extract_system(&request.messages) {
            body["system"] = build_system_blocks_from_string(&system);
        }

        // Add temperature if specified
        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        // Convert tools with cache_control on last tool for prompt caching.
        // Use the *checked* variant: caller-supplied tools that fail validation
        // must surface as ProviderError::RequestFailed rather than be silently
        // dropped (crosslink #413).
        if let Some(tools) = &request.tools {
            let converted = Self::convert_tools_checked(tools, true)?;
            if !converted.is_empty() {
                body["tools"] = json!(converted);
            }
        }

        // Add streaming flag
        if request.stream.unwrap_or(false) {
            body["stream"] = json!(true);
        }

        debug!(body = %body, "Transformed request for Anthropic");
        Ok(body)
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = self.transform_request(request)?;

        // Add Anthropic extended thinking params if enabled
        // See: https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking
        if thinking.enabled {
            // Budget tokens must be at least 1024 for Anthropic
            let budget = thinking.budget_tokens.unwrap_or(10000).max(1024);
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget
            });
            debug!(
                "Added Anthropic thinking params: enabled=true, budget={}",
                budget
            );
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Convert Anthropic response to OpenAI format.
        //
        // Crosslink #413: previously this function injected `"msg_unknown"`,
        // `"unknown"`, and `0` sentinels for missing top-level fields, and
        // used `filter_map` + `?` to silently drop malformed content blocks.
        // Both behaviours masked upstream API contract violations. The
        // refactor now refuses to manufacture data — every missing required
        // field surfaces as `ProviderError::InvalidResponse` via the small
        // helpers defined further down this module.
        let id = require_nonempty_str(&response, "id")?.to_string();
        let model = require_nonempty_str(&response, "model")?.to_string();
        let stop_reason_str = require_str(&response, "stop_reason")?;
        let finish_reason = map_stop_reason(stop_reason_str);

        let content_arr = response
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| {
                warn!(response = %response, "Anthropic response missing required 'content' array (crosslink #413)");
                ProviderError::InvalidResponse(
                    "Anthropic response missing required 'content' array".to_string(),
                )
            })?;

        let (text_buf, tool_calls) = walk_content_blocks(content_arr)?;

        let mut message = json!({
            "role": "assistant",
            "content": text_buf
        });
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }

        let (prompt_tokens, completion_tokens) = extract_usage(&response);

        Ok(json!({
            "id": id,
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason,
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens,
            }
        }))
    }

    fn chat_endpoint(&self, _model: &str) -> String {
        "/v1/messages".to_string()
    }

    fn get_headers(&self, api_key: &ApiKey) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), api_key.as_str().to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }

    /// Anthropic native shape: `content` is an array of typed blocks;
    /// the assistant text lives in the first `{"type": "text",
    /// "text": "..."}` block. The default `OpenAI` extractor would
    /// return `None` here because the response has no `choices` array.
    /// See crosslink #479.
    fn extract_response_text(&self, response: &Value) -> Option<String> {
        response
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
            })
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .map(std::string::ToString::to_string)
    }

    /// Anthropic native usage envelope: `input_tokens` /
    /// `output_tokens` / `cache_read_input_tokens` /
    /// `cache_creation_input_tokens`. The trait default already handles
    /// these fields, but overriding here makes the intent explicit and
    /// future-proofs against the default drifting toward `OpenAI`-only
    /// keys.
    fn extract_token_usage(&self, response: &Value) -> Option<TokenUsage> {
        let usage = response.get("usage")?;
        Some(TokenUsage {
            input_tokens: usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
    }
}

impl AnthropicAdapter {
    /// Headers to send when authenticating with an OAuth access token
    /// (Claude Max / Pro subscriptions) rather than a static API key.
    ///
    /// Takes `&str` rather than `&ApiKey` because the bearer token is a
    /// different secret type with different lifetime semantics than the
    /// API key; it is sourced from the OAuth session (`session.credentials
    /// .access_token`) and never flows through config deserialization.
    ///
    /// Replaces the inline magic strings previously embedded in
    /// `proxy::proxy_anthropic_messages` — every Anthropic-specific header
    /// literal now lives in one place. See crosslink #338.
    #[must_use]
    pub fn oauth_headers(bearer_token: &str) -> Vec<(String, String)> {
        vec![
            (
                "authorization".to_string(),
                format!("Bearer {bearer_token}"),
            ),
            (
                "anthropic-beta".to_string(),
                // Single source of truth — see crosslink #272.
                crate::claude_credentials::claude_code_beta_header_value(),
            ),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

// --- Crosslink #413 helpers --------------------------------------------------
//
// Extracted from `transform_response` so each shape-validation step is its own
// named, testable unit instead of an opaque 150-line function. Every helper
// here surfaces missing/malformed fields as `ProviderError::InvalidResponse`
// and logs the offending payload at `WARN` for forensic traceability.

/// Extract a string field that MUST be present (any string value).
fn require_str<'a>(response: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    response
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            warn!(response = %response, field, "Anthropic response missing required field (crosslink #413)");
            ProviderError::InvalidResponse(format!(
                "Anthropic response missing required '{field}' field"
            ))
        })
}

/// Extract a string field that MUST be present AND non-empty.
fn require_nonempty_str<'a>(response: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    let s = require_str(response, field)?;
    if s.is_empty() {
        warn!(response = %response, field, "Anthropic response has empty required field (crosslink #413)");
        return Err(ProviderError::InvalidResponse(format!(
            "Anthropic response missing required '{field}' field"
        )));
    }
    Ok(s)
}

/// Map Anthropic `stop_reason` to the `OpenAI` `finish_reason` vocabulary.
fn map_stop_reason(stop_reason: &str) -> &'static str {
    match stop_reason {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        "end_turn" | "stop_sequence" => "stop",
        other => {
            warn!(
                stop_reason = other,
                "Unknown Anthropic stop_reason; mapping to 'stop' (crosslink #413)"
            );
            "stop"
        }
    }
}

/// Walk an Anthropic `content` array, surfacing malformed blocks as errors.
/// Returns `(concatenated_text, tool_call_array)`.
fn walk_content_blocks(content_arr: &[Value]) -> Result<(String, Vec<Value>), ProviderError> {
    let mut text_buf = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for (i, block) in content_arr.iter().enumerate() {
        let block_type = block.get("type").and_then(|t| t.as_str()).ok_or_else(|| {
            warn!(index = i, block = %block, "Anthropic content block missing 'type' (crosslink #413)");
            ProviderError::InvalidResponse(format!(
                "Anthropic content block at index {i} missing 'type' field: {block}"
            ))
        })?;

        match block_type {
            "text" => text_buf.push_str(extract_text_block(i, block)?),
            "tool_use" => tool_calls.push(extract_tool_use_block(i, block)?),
            // Other block types (e.g. `thinking`, `redacted_thinking`) are
            // intentionally not surfaced into the OpenAI shape; this is a
            // schema-level skip, not a defensive-programming swallow.
            _ => debug!(
                block_type = block_type,
                "skipping Anthropic content block of unmapped type"
            ),
        }
    }
    Ok((text_buf, tool_calls))
}

/// Extract the string body of a `text` content block.
fn extract_text_block(index: usize, block: &Value) -> Result<&str, ProviderError> {
    block.get("text").and_then(|t| t.as_str()).ok_or_else(|| {
        warn!(index, block = %block, "Anthropic text block missing string 'text' (crosslink #413)");
        ProviderError::InvalidResponse(format!(
            "Anthropic text block at index {index} missing string 'text' field: {block}"
        ))
    })
}

/// Extract an `OpenAI`-shaped `tool_call` object from a `tool_use` content block.
fn extract_tool_use_block(index: usize, block: &Value) -> Result<Value, ProviderError> {
    let tool_id = block.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
        warn!(index, block = %block, "Anthropic tool_use block missing 'id' (crosslink #413)");
        ProviderError::InvalidResponse(format!(
            "Anthropic tool_use block at index {index} missing string 'id': {block}"
        ))
    })?;
    let tool_name = block.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
        warn!(index, block = %block, "Anthropic tool_use block missing 'name' (crosslink #413)");
        ProviderError::InvalidResponse(format!(
            "Anthropic tool_use block at index {index} missing string 'name': {block}"
        ))
    })?;
    let input = block.get("input").ok_or_else(|| {
        warn!(index, block = %block, "Anthropic tool_use block missing 'input' (crosslink #413)");
        ProviderError::InvalidResponse(format!(
            "Anthropic tool_use block at index {index} missing 'input' field: {block}"
        ))
    })?;

    // Avoid double-serialization: if input is already a string, use it directly;
    // otherwise serialize the JSON value to a string for the OpenAI format.
    let arguments = if let Some(s) = input.as_str() {
        s.to_string()
    } else {
        serde_json::to_string(input).map_err(|e| {
            ProviderError::InvalidResponse(format!(
                "Anthropic tool_use block at index {index} has unserializable 'input': {e}"
            ))
        })?
    };

    Ok(json!({
        "id": tool_id,
        "type": "function",
        "function": {
            "name": tool_name,
            "arguments": arguments
        }
    }))
}

/// Pull `(input_tokens, output_tokens)` out of an optional `usage` object.
/// When `usage` is absent (e.g. a partial streaming response), zeros are
/// reported but a `DEBUG` log records the case — this is *not* the silent
/// sentinel behaviour the bug report flagged.
fn extract_usage(response: &Value) -> (u64, u64) {
    response.get("usage").map_or_else(
        || {
            debug!("Anthropic response has no 'usage' object; reporting 0/0 token counts");
            (0u64, 0u64)
        },
        |u| {
            let p = u
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let c = u
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            (p, c)
        },
    )
}

/// Build an Anthropic `system` array from a [`SystemPromptBlocks`].
///
/// Returns two blocks for cache efficiency:
/// - Block 0: stable prefix with `cache_control: { type: "ephemeral" }`
/// - Block 1: dynamic suffix without `cache_control` (reprocessed each turn)
///
/// If the dynamic suffix is empty, only one block is returned.
#[must_use]
pub fn build_system_blocks(blocks: &crate::prompt::SystemPromptBlocks) -> Value {
    if blocks.dynamic_suffix.is_empty() {
        json!([{
            "type": "text",
            "text": blocks.stable_prefix,
            "cache_control": {"type": "ephemeral"}
        }])
    } else {
        json!([
            {
                "type": "text",
                "text": blocks.stable_prefix,
                "cache_control": {"type": "ephemeral"}
            },
            {
                "type": "text",
                "text": blocks.dynamic_suffix
            }
        ])
    }
}

/// Build an Anthropic `system` array from a single string (legacy path).
///
/// Used by the proxy adapter which receives pre-assembled strings.
#[must_use]
pub fn build_system_blocks_from_string(system: &str) -> Value {
    json!([{
        "type": "text",
        "text": system,
        "cache_control": {"type": "ephemeral"}
    }])
}

/// Convert tools from `OpenAI` format to Anthropic format
///
/// `OpenAI` format: `{ "type": "function", "function": { "name": ..., "parameters": ... } }`
/// Anthropic format: `{ "name": ..., "description": ..., "input_schema": ... }`
#[must_use]
pub fn convert_tools_to_anthropic(tools: &[Value]) -> Vec<Value> {
    AnthropicAdapter::convert_tools(tools, true)
}

/// Convert messages from `OpenAI` format to Anthropic format
///
/// Handles the critical differences:
/// - `OpenAI` `role: "tool"` -> Anthropic `role: "user"` with `type: "tool_result"` content
/// - `OpenAI` `tool_calls` array -> Anthropic `type: "tool_use"` content blocks
/// - System messages are filtered out (handled separately at top level)
#[must_use]
pub fn convert_messages_to_anthropic(messages: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");

        // Skip system messages (handled separately)
        if role == "system" {
            continue;
        }

        // Handle tool result messages (OpenAI format: role="tool")
        if role == "tool" {
            let tool_use_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let is_error = msg
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            let mut tool_result = json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content
            });
            // Anthropic API supports is_error on tool_result blocks
            if is_error {
                tool_result["is_error"] = json!(true);
            }

            result.push(json!({
                "role": "user",
                "content": [tool_result]
            }));
            continue;
        }

        // Handle assistant messages with tool_calls
        if role == "assistant" {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                let mut content_blocks: Vec<Value> = Vec::new();

                // Add text content if present
                if let Some(text) = msg.get("content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        content_blocks.push(json!({"type": "text", "text": text}));
                    }
                }

                // Convert tool_calls to tool_use blocks
                let empty_obj = json!({});
                for tc in tool_calls {
                    let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let func = tc.get("function").unwrap_or(&empty_obj);
                    let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args_str = func
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");

                    // Parse arguments string to JSON object
                    let input: Value = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));

                    content_blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                }

                // Anthropic requires non-empty content array
                if content_blocks.is_empty() {
                    content_blocks.push(json!({"type": "text", "text": ""}));
                }
                result.push(json!({
                    "role": "assistant",
                    "content": content_blocks
                }));
                continue;
            }
        }

        // Regular user or assistant message - convert content to array format
        let content = msg.get("content").map_or_else(
            || json!([{"type": "text", "text": ""}]),
            |c| {
                if c.is_string() {
                    json!([{"type": "text", "text": c.as_str().unwrap_or("")}])
                } else if c.is_array() {
                    c.clone()
                } else {
                    json!([{"type": "text", "text": ""}])
                }
            },
        );

        result.push(json!({
            "role": role,
            "content": content
        }));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{ChatCompletionRequest, ChatMessage, ContentPart, MessageContent};

    fn text_msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: MessageContent::Text(text.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    // --- Regression test for crosslink #475 ---
    //
    // The hot path (AnthropicAdapter::transform_request) previously went
    // through a private `convert_messages` that mapped role="tool" to a
    // bare role="user" text block, losing the tool_use_id linkage.
    // Anthropic rejects this with 400 ("each tool_use must have a matching
    // tool_result"). After the fix the adapter routes through
    // convert_messages_to_anthropic, which preserves the linkage.

    #[test]
    fn tool_result_role_becomes_tool_result_block_with_id() {
        let msgs = vec![
            text_msg("user", "what is 2+2?"),
            ChatMessage {
                role: "assistant".to_string(),
                content: MessageContent::Text(String::new()),
                name: None,
                tool_calls: Some(vec![json!({
                    "id": "toolu_abc",
                    "type": "function",
                    "function": {"name": "calc", "arguments": "{\"expr\":\"2+2\"}"}
                })]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: MessageContent::Text("4".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: Some("toolu_abc".to_string()),
            },
        ];

        let request = ChatCompletionRequest {
            model: "claude-opus-4-6".to_string(),
            messages: msgs,
            max_tokens: Some(64),
            temperature: None,
            tools: None,
            stream: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };

        let adapter = AnthropicAdapter::new();
        let body = adapter.transform_request(&request).expect("transform ok");
        let messages = body["messages"].as_array().expect("messages is array");

        assert_eq!(messages.len(), 3, "expected 3 messages, got {messages:?}");

        // Assistant message must carry a tool_use block with id toolu_abc.
        let asst = &messages[1];
        assert_eq!(asst["role"], "assistant");
        let asst_content = asst["content"].as_array().expect("assistant content array");
        let tool_use = asst_content
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("assistant message missing tool_use block");
        assert_eq!(tool_use["id"], "toolu_abc");
        assert_eq!(tool_use["name"], "calc");
        assert_eq!(tool_use["input"]["expr"], "2+2");

        // Tool result must be wrapped as a user message with a tool_result
        // block whose tool_use_id matches the preceding tool_use id.
        let tool_result_msg = &messages[2];
        assert_eq!(tool_result_msg["role"], "user");
        let tr_content = tool_result_msg["content"].as_array().expect("tr content");
        let tool_result = tr_content
            .iter()
            .find(|b| b["type"] == "tool_result")
            .expect("tool result block missing — #475 regression");
        assert_eq!(tool_result["tool_use_id"], "toolu_abc");
        assert_eq!(tool_result["content"], "4");
    }

    #[test]
    fn plain_text_user_message_still_works() {
        let request = ChatCompletionRequest {
            model: "claude-opus-4-6".to_string(),
            messages: vec![text_msg("user", "hi")],
            max_tokens: Some(16),
            temperature: None,
            tools: None,
            stream: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };
        let body = AnthropicAdapter::new()
            .transform_request(&request)
            .expect("transform ok");
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn image_content_part_preserved() {
        let parts = vec![
            ContentPart {
                content_type: "text".to_string(),
                text: Some("describe this".to_string()),
                image_url: None,
            },
            ContentPart {
                content_type: "image".to_string(),
                text: None,
                image_url: Some(json!({
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "iVBORw..."
                })),
            },
        ];
        let msg = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Parts(parts),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        let request = ChatCompletionRequest {
            model: "claude-opus-4-6".to_string(),
            messages: vec![msg],
            max_tokens: Some(64),
            temperature: None,
            tools: None,
            stream: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };
        let body = AnthropicAdapter::new()
            .transform_request(&request)
            .expect("transform ok");
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        let content = messages[0]["content"].as_array().expect("content is array");
        assert!(content.len() >= 2, "multimodal parts lost: {content:?}");
    }

    // --- Regression tests for crosslink #338 ---

    #[test]
    fn oauth_headers_contains_all_required_fields() {
        let h = AnthropicAdapter::oauth_headers("access-xyz");
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"authorization"));
        assert!(names.contains(&"anthropic-beta"));
        assert!(names.contains(&"anthropic-version"));
        assert!(names.contains(&"content-type"));

        let auth = h.iter().find(|(k, _)| k == "authorization").unwrap();
        assert_eq!(auth.1, "Bearer access-xyz");

        let beta = h.iter().find(|(k, _)| k == "anthropic-beta").unwrap();
        assert!(beta.1.contains("claude-code-20250219"));
        assert!(beta.1.contains("oauth-2025-04-20"));
        assert!(beta.1.contains("interleaved-thinking-2025-05-14"));
        assert!(beta.1.contains("fine-grained-tool-streaming-2025-05-14"));
    }

    // --- Regression tests for crosslink #413 ---
    //
    // Before the fix, the Anthropic adapter silently swallowed malformed
    // upstream / caller input via `filter_map(|x| x.as_str().map(...))`
    // and substituted `"msg_unknown"` / `"unknown"` / `0` sentinels when
    // top-level fields were missing. That made every contract violation
    // by the Anthropic API (or by an upstream caller) invisible to tests
    // and to downstream clients. The tests below pin the new behaviour:
    // each malformed input must produce a typed error.

    fn anth_request_with_tools(tools: Vec<Value>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "claude-opus-4-7".to_string(),
            messages: vec![text_msg("user", "go")],
            max_tokens: Some(64),
            temperature: None,
            tools: Some(tools),
            stream: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        }
    }

    /// A tool missing `function.name` MUST surface as
    /// `ProviderError::RequestFailed` — never silently dropped from the
    /// request body sent to Anthropic.
    #[test]
    fn transform_request_errors_on_tool_missing_function_name() {
        let bad_tool = json!({
            "type": "function",
            "function": { "description": "no name here" }
        });
        let req = anth_request_with_tools(vec![bad_tool]);
        let err = AnthropicAdapter::new()
            .transform_request(&req)
            .expect_err("malformed tool must surface (#413)");
        match err {
            ProviderError::RequestFailed(msg) => {
                assert!(
                    msg.contains("function.name"),
                    "error must name the missing field, got: {msg}"
                );
                assert!(
                    msg.contains("index 0"),
                    "error must locate the offending tool, got: {msg}"
                );
            }
            other => panic!("expected RequestFailed, got {other:?}"),
        }
    }

    /// A tool missing the `function` object entirely also surfaces — this
    /// previously took the `filter_map`'s short-circuit `?` path silently.
    /// The bad tool is the *second* of two; asserting on index 1 proves we
    /// walk the array rather than rejecting any batch indiscriminately.
    #[test]
    fn transform_request_errors_on_tool_missing_function_object() {
        let good_tool = json!({
            "type": "function",
            "function": {"name": "ok_tool", "parameters": {}}
        });
        let bad_tool = json!({"type": "function"});
        let req = anth_request_with_tools(vec![good_tool, bad_tool]);
        let err = AnthropicAdapter::new()
            .transform_request(&req)
            .expect_err("malformed tool must surface (#413)");
        match err {
            ProviderError::RequestFailed(msg) => {
                assert!(
                    msg.contains("'function' object") && msg.contains("index 1"),
                    "error must name field and index, got: {msg}"
                );
            }
            other => panic!("expected RequestFailed, got {other:?}"),
        }
    }

    /// An empty `function.name` (present but blank string) is just as
    /// malformed as missing — Anthropic would 400 on this anyway and we
    /// should not let it through with a default.
    #[test]
    fn transform_request_errors_on_tool_with_empty_function_name() {
        let bad_tool = json!({
            "type": "function",
            "function": {"name": "", "parameters": {}}
        });
        let req = anth_request_with_tools(vec![bad_tool]);
        let err = AnthropicAdapter::new()
            .transform_request(&req)
            .expect_err("empty function.name must surface (#413)");
        assert!(matches!(err, ProviderError::RequestFailed(_)));
    }

    /// An empty `{}` upstream response — the exact case the issue's
    /// Mandated Refactor item 4 asks us to pin — MUST return Err, not a
    /// successful response laden with `"msg_unknown"` / `"unknown"`.
    #[test]
    fn transform_response_errors_on_empty_object_no_sentinels() {
        let response = json!({});
        let result = AnthropicAdapter::new().transform_response(response, false);
        let err = result.expect_err("empty upstream response must be an error (#413)");
        match err {
            ProviderError::InvalidResponse(msg) => {
                // The first missing required field is `id`; confirm we
                // surface the *field name* not just a generic error.
                assert!(
                    msg.contains("'id'"),
                    "error must name the missing field, got: {msg}"
                );
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    /// A response missing only `model` (but with valid `id`, `stop_reason`,
    /// `content`) must error with the specific missing-field name — proving
    /// we don't fall back to the `"unknown"` sentinel that previously
    /// masked the upstream defect.
    #[test]
    fn transform_response_errors_on_missing_model_no_sentinel() {
        let response = json!({
            "id": "msg_abc",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let err = AnthropicAdapter::new()
            .transform_response(response, false)
            .expect_err("missing 'model' must surface (#413)");
        match err {
            ProviderError::InvalidResponse(msg) => {
                assert!(
                    msg.contains("'model'"),
                    "error must name the missing field, got: {msg}"
                );
                // And critically, no sentinel leaked into the error.
                assert!(!msg.contains("unknown"), "must not fall back to 'unknown'");
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    /// A `tool_use` content block missing `id` or `name` previously got
    /// silently `filter_map`'d out, leaving the assistant message with no
    /// `tool_calls` — a state the downstream `OpenAI`-shape client treats as
    /// "no tools were called", which is a security-relevant lie. Pin the
    /// new behaviour: error, with the offending block index.
    #[test]
    fn transform_response_errors_on_tool_use_block_missing_name() {
        let response = json!({
            "id": "msg_xyz",
            "model": "claude-opus-4-7",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "calling..."},
                {"type": "tool_use", "id": "tu_1", "input": {"x": 1}}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let err = AnthropicAdapter::new()
            .transform_response(response, false)
            .expect_err("malformed tool_use block must surface (#413)");
        match err {
            ProviderError::InvalidResponse(msg) => {
                assert!(
                    msg.contains("tool_use") && msg.contains("'name'") && msg.contains("index 1"),
                    "error must name field and index, got: {msg}"
                );
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    /// A well-formed response still round-trips cleanly — the strictness
    /// must not break the happy path.
    #[test]
    fn transform_response_happy_path_still_works() {
        let response = json!({
            "id": "msg_happy",
            "model": "claude-opus-4-7",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 3, "output_tokens": 2}
        });
        let out = AnthropicAdapter::new()
            .transform_response(response, false)
            .expect("well-formed response transforms");
        assert_eq!(out["id"], "msg_happy");
        assert_eq!(out["model"], "claude-opus-4-7");
        assert_eq!(out["choices"][0]["message"]["content"], "ok");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["prompt_tokens"], 3);
        assert_eq!(out["usage"]["completion_tokens"], 2);
        assert_eq!(out["usage"]["total_tokens"], 5);
    }

    /// `convert_tools_checked` is the fallible primitive that powers the
    /// strict request path; pin its error shape directly so future
    /// refactors of `transform_request` cannot quietly skip validation.
    #[test]
    fn convert_tools_checked_errors_on_malformed_input() {
        let tools = vec![json!({"type": "function", "function": {}})];
        let err = AnthropicAdapter::convert_tools_checked(&tools, true)
            .expect_err("missing function.name must surface (#413)");
        assert!(matches!(err, ProviderError::RequestFailed(_)));
    }
}
