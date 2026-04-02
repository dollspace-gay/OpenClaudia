//! Anthropic Messages API adapter.

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

use super::{ProviderAdapter, ProviderError};

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

    /// Convert `OpenAI` messages to Anthropic format
    fn convert_messages(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .filter(|m| m.role != "system") // System is handled separately
            .map(|m| {
                let role = match m.role.as_str() {
                    "assistant" => "assistant",
                    _ => "user", // user, function, tool all become user
                };

                let content = match &m.content {
                    MessageContent::Text(t) => json!([{"type": "text", "text": t}]),
                    MessageContent::Parts(parts) => {
                        let converted: Vec<Value> = parts
                            .iter()
                            .map(|p| {
                                p.text.as_ref().map_or_else(
                                    || {
                                        p.image_url.as_ref().map_or_else(
                                            || json!({"type": "text", "text": ""}),
                                            |image| {
                                                json!({
                                                    "type": "image",
                                                    "source": image
                                                })
                                            },
                                        )
                                    },
                                    |text| json!({"type": "text", "text": text}),
                                )
                            })
                            .collect();
                        Value::Array(converted)
                    }
                };

                json!({
                    "role": role,
                    "content": content
                })
            })
            .collect()
    }

    /// Convert `OpenAI` tools to Anthropic format with optional prompt caching
    /// If `cache_last` is true, adds `cache_control` to the last tool for prompt caching
    pub(crate) fn convert_tools(tools: &[Value], cache_last: bool) -> Vec<Value> {
        let len = tools.len();
        tools
            .iter()
            .enumerate()
            .filter_map(|(i, tool)| {
                let func = tool.get("function")?;
                let mut tool_def = json!({
                    "name": func.get("name")?,
                    "description": func.get("description").unwrap_or(&Value::String(String::new())),
                    "input_schema": func.get("parameters").cloned().unwrap_or_else(|| Value::Object(serde_json::Map::default()))
                });

                // Add cache_control to the last tool for prompt caching
                // This caches all tools since cache applies to everything before the marker
                if cache_last && i == len - 1 {
                    tool_def["cache_control"] = json!({"type": "ephemeral"});
                }

                Some(tool_def)
            })
            .collect()
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
            body["system"] = json!([
                {
                    "type": "text",
                    "text": system,
                    "cache_control": {"type": "ephemeral"}
                }
            ]);
        }

        // Add temperature if specified
        if let Some(temp) = request.temperature {
            body["temperature"] = json!(temp);
        }

        // Convert tools with cache_control on last tool for prompt caching
        if let Some(tools) = &request.tools {
            let converted = Self::convert_tools(tools, true);
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
        // Convert Anthropic response to OpenAI format
        let content = response
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|block| {
                        if block.get("type")?.as_str()? == "text" {
                            Some(block.get("text")?.as_str()?.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();

        let tool_calls: Option<Vec<Value>> = response
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|block| {
                        if block.get("type")?.as_str()? == "tool_use" {
                            // Avoid double-serialization: if input is already a
                            // string, use it directly; otherwise serialize the
                            // JSON value to a string for the OpenAI format.
                            let input = block.get("input")?;
                            let arguments = if let Some(s) = input.as_str() {
                                s.to_string()
                            } else {
                                serde_json::to_string(input).ok()?
                            };
                            Some(json!({
                                "id": block.get("id")?,
                                "type": "function",
                                "function": {
                                    "name": block.get("name")?,
                                    "arguments": arguments
                                }
                            }))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .filter(|v: &Vec<Value>| !v.is_empty());

        let mut message = json!({
            "role": "assistant",
            "content": content
        });

        if let Some(calls) = tool_calls {
            message["tool_calls"] = json!(calls);
        }

        let default_id = json!("msg_unknown");
        let default_model = json!("unknown");
        let default_zero = json!(0);
        Ok(json!({
            "id": response.get("id").unwrap_or(&default_id),
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": response.get("model").unwrap_or(&default_model),
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": match response.get("stop_reason").and_then(|s| s.as_str()) {
                    Some("tool_use") => "tool_calls",
                    Some("max_tokens") => "length",
                    _ => "stop",
                }
            }],
            "usage": {
                "prompt_tokens": response.get("usage").and_then(|u| u.get("input_tokens")).unwrap_or(&default_zero),
                "completion_tokens": response.get("usage").and_then(|u| u.get("output_tokens")).unwrap_or(&default_zero),
                "total_tokens": response.get("usage").map_or(0, |u| {
                    u.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) +
                    u.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0)
                })
            }
        }))
    }

    fn chat_endpoint(&self, _model: &str) -> String {
        "/v1/messages".to_string()
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), api_key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
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
