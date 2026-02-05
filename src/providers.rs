//! Provider Adapters - Translate between OpenAI-compatible format and provider APIs.
//!
//! Supports:
//! - Anthropic Messages API
//! - OpenAI Chat Completions API
//! - Google Gemini API
//! - DeepSeek API (with thinking/reasoning support)
//! - Qwen/Alibaba API (with thinking support)
//! - Z.AI/GLM API (with thinking support)
//! - Ollama (local LLM inference)
//! - Any OpenAI-compatible server (LM Studio, LocalAI, etc.)
//!
//! Handles message format translation and tool/function calling conversion.

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

/// Errors that can occur during provider operations
#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Request failed: {0}")]
    RequestFailed(String),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Unsupported feature: {0}")]
    Unsupported(String),
}

/// Model information returned from provider
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub owned_by: Option<String>,
    #[serde(default)]
    pub created: Option<i64>,
}

/// Trait for provider adapters
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Get the provider name
    fn name(&self) -> &str;

    /// Transform an OpenAI-compatible request to provider format
    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError>;

    /// Transform request with thinking config applied
    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        // Default: ignore thinking config, just call transform_request
        let _ = thinking;
        self.transform_request(request)
    }

    /// Transform a provider response to OpenAI-compatible format
    fn transform_response(&self, response: Value, stream: bool) -> Result<Value, ProviderError>;

    /// Get the endpoint path for chat completions
    fn chat_endpoint(&self) -> &str;

    /// Get required headers for this provider
    fn get_headers(&self, api_key: &str) -> Vec<(String, String)>;

    /// Check if this provider supports model listing
    fn supports_model_listing(&self) -> bool {
        false
    }

    /// Get the models endpoint path (for providers that support it)
    fn models_endpoint(&self) -> &str {
        "/v1/models"
    }
}

/// Anthropic Messages API adapter
pub struct AnthropicAdapter;

impl AnthropicAdapter {
    pub fn new() -> Self {
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

    /// Convert OpenAI messages to Anthropic format
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
                                if let Some(text) = &p.text {
                                    json!({"type": "text", "text": text})
                                } else if let Some(image) = &p.image_url {
                                    // Convert OpenAI image format to Anthropic
                                    json!({
                                        "type": "image",
                                        "source": image
                                    })
                                } else {
                                    json!({"type": "text", "text": ""})
                                }
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

    /// Convert OpenAI tools to Anthropic format with optional prompt caching
    /// If cache_last is true, adds cache_control to the last tool for prompt caching
    fn convert_tools(tools: &[Value], cache_last: bool) -> Vec<Value> {
        let len = tools.len();
        tools
            .iter()
            .enumerate()
            .filter_map(|(i, tool)| {
                let func = tool.get("function")?;
                let mut tool_def = json!({
                    "name": func.get("name")?,
                    "description": func.get("description").unwrap_or(&json!("")),
                    "input_schema": func.get("parameters").unwrap_or(&json!({}))
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

/// Convert tools from OpenAI format to Anthropic format
///
/// OpenAI format: `{ "type": "function", "function": { "name": ..., "parameters": ... } }`
/// Anthropic format: `{ "name": ..., "description": ..., "input_schema": ... }`
pub fn convert_tools_to_anthropic(tools: &[Value]) -> Vec<Value> {
    AnthropicAdapter::convert_tools(tools, true)
}

/// Convert messages from OpenAI format to Anthropic format
///
/// Handles the critical differences:
/// - OpenAI `role: "tool"` → Anthropic `role: "user"` with `type: "tool_result"` content
/// - OpenAI `tool_calls` array → Anthropic `type: "tool_use"` content blocks
/// - System messages are filtered out (handled separately at top level)
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
                .and_then(|v| v.as_bool())
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
                    let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                    content_blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                }

                result.push(json!({
                    "role": "assistant",
                    "content": content_blocks
                }));
                continue;
            }
        }

        // Regular user or assistant message - convert content to array format
        let content = msg
            .get("content")
            .map(|c| {
                if c.is_string() {
                    json!([{"type": "text", "text": c.as_str().unwrap_or("")}])
                } else if c.is_array() {
                    c.clone()
                } else {
                    json!([{"type": "text", "text": ""}])
                }
            })
            .unwrap_or_else(|| json!([{"type": "text", "text": ""}]));

        result.push(json!({
            "role": role,
            "content": content
        }));
    }

    result
}

impl Default for AnthropicAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        let mut body = json!({
            "model": &request.model,
            "messages": Self::convert_messages(&request.messages),
            "max_tokens": request.max_tokens.unwrap_or(4096)
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
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        let tool_calls: Option<Vec<Value>> = response
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|block| {
                        if block.get("type")?.as_str()? == "tool_use" {
                            Some(json!({
                                "id": block.get("id")?,
                                "type": "function",
                                "function": {
                                    "name": block.get("name")?,
                                    "arguments": serde_json::to_string(block.get("input")?).ok()?
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

        Ok(json!({
            "id": response.get("id").unwrap_or(&json!("msg_unknown")),
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": response.get("model").unwrap_or(&json!("unknown")),
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": match response.get("stop_reason").and_then(|s| s.as_str()) {
                    Some("end_turn") => "stop",
                    Some("tool_use") => "tool_calls",
                    Some("max_tokens") => "length",
                    _ => "stop"
                }
            }],
            "usage": {
                "prompt_tokens": response.get("usage").and_then(|u| u.get("input_tokens")).unwrap_or(&json!(0)),
                "completion_tokens": response.get("usage").and_then(|u| u.get("output_tokens")).unwrap_or(&json!(0)),
                "total_tokens": response.get("usage").map(|u| {
                    u.get("input_tokens").and_then(|i| i.as_u64()).unwrap_or(0) +
                    u.get("output_tokens").and_then(|o| o.as_u64()).unwrap_or(0)
                }).unwrap_or(0)
            }
        }))
    }

    fn chat_endpoint(&self) -> &str {
        "/v1/messages"
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), api_key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

/// OpenAI API adapter (mostly passthrough)
pub struct OpenAIAdapter;

impl OpenAIAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenAIAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for OpenAIAdapter {
    fn name(&self) -> &str {
        "openai"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        // OpenAI format is our canonical format, so minimal transformation
        serde_json::to_value(request).map_err(|e| ProviderError::RequestFailed(e.to_string()))
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = serde_json::to_value(request)
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        // Add OpenAI o1/o3 reasoning_effort if enabled
        // See: https://platform.openai.com/docs/guides/reasoning
        if thinking.enabled {
            // Use configured effort or default to "medium"
            let effort = thinking.reasoning_effort.as_deref().unwrap_or("medium");
            body["reasoning_effort"] = json!(effort);
            debug!("Added OpenAI reasoning params: effort={}", effort);
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Response is already in OpenAI format
        Ok(response)
    }

    fn chat_endpoint(&self) -> &str {
        "/v1/chat/completions"
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }

    fn supports_model_listing(&self) -> bool {
        true
    }
}

/// Google Gemini API adapter
pub struct GoogleAdapter;

impl GoogleAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Convert OpenAI messages to Gemini format
    fn convert_messages(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .filter(|m| m.role != "system") // System handled via systemInstruction
            .map(|m| {
                let role = match m.role.as_str() {
                    "assistant" => "model",
                    _ => "user",
                };

                let parts = match &m.content {
                    MessageContent::Text(t) => json!([{"text": t}]),
                    MessageContent::Parts(parts) => {
                        let converted: Vec<Value> = parts
                            .iter()
                            .map(|p| {
                                if let Some(text) = &p.text {
                                    json!({"text": text})
                                } else if let Some(image) = &p.image_url {
                                    json!({"inlineData": image})
                                } else {
                                    json!({"text": ""})
                                }
                            })
                            .collect();
                        Value::Array(converted)
                    }
                };

                json!({
                    "role": role,
                    "parts": parts
                })
            })
            .collect()
    }

    /// Convert OpenAI tools to Gemini function declarations
    fn convert_tools(tools: &[Value]) -> Value {
        let functions: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                Some(json!({
                    "name": func.get("name")?,
                    "description": func.get("description").unwrap_or(&json!("")),
                    "parameters": func.get("parameters").unwrap_or(&json!({}))
                }))
            })
            .collect();

        json!([{"functionDeclarations": functions}])
    }

    /// Extract system instruction
    fn extract_system(messages: &[ChatMessage]) -> Option<Value> {
        messages.iter().find(|m| m.role == "system").map(|m| {
            let text = match &m.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| p.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            json!({"parts": [{"text": text}]})
        })
    }
}

impl Default for GoogleAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for GoogleAdapter {
    fn name(&self) -> &str {
        "google"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        let mut body = json!({
            "contents": Self::convert_messages(&request.messages)
        });

        // Add system instruction if present
        if let Some(system) = Self::extract_system(&request.messages) {
            body["systemInstruction"] = system;
        }

        // Add generation config
        let mut gen_config = json!({});
        if let Some(temp) = request.temperature {
            gen_config["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            gen_config["maxOutputTokens"] = json!(max_tokens);
        }
        if gen_config != json!({}) {
            body["generationConfig"] = gen_config;
        }

        // Convert tools
        if let Some(tools) = &request.tools {
            body["tools"] = Self::convert_tools(tools);
        }

        debug!(body = %body, "Transformed request for Google");
        Ok(body)
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = self.transform_request(request)?;

        // Add Google Gemini 2.5 thinking config if enabled
        // See: https://ai.google.dev/gemini-api/docs/thinking
        if thinking.enabled {
            // Budget range: 0-32768, default to 8192
            let budget = thinking.budget_tokens.unwrap_or(8192).min(32768);

            // Ensure generationConfig exists
            if body.get("generationConfig").is_none() {
                body["generationConfig"] = json!({});
            }

            body["generationConfig"]["thinkingConfig"] = json!({
                "thinkingBudget": budget
            });
            debug!("Added Google thinking params: budget={}", budget);
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Extract content from Gemini response
        let candidate = response
            .get("candidates")
            .and_then(|c| c.get(0))
            .ok_or_else(|| {
                ProviderError::InvalidResponse("No candidates in response".to_string())
            })?;

        let content = candidate
            .get("content")
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

        // Extract function calls
        let tool_calls: Option<Vec<Value>> = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| {
                        let func_call = p.get("functionCall")?;
                        Some(json!({
                            "id": format!("call_{}", uuid::Uuid::new_v4()),
                            "type": "function",
                            "function": {
                                "name": func_call.get("name")?,
                                "arguments": serde_json::to_string(func_call.get("args")?).ok()?
                            }
                        }))
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

        let finish_reason = candidate
            .get("finishReason")
            .and_then(|r| r.as_str())
            .map(|r| match r {
                "STOP" => "stop",
                "MAX_TOKENS" => "length",
                "SAFETY" => "content_filter",
                _ => "stop",
            })
            .unwrap_or("stop");

        Ok(json!({
            "id": format!("gemini-{}", uuid::Uuid::new_v4()),
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": "gemini",
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": response.get("usageMetadata").and_then(|u| u.get("promptTokenCount")).unwrap_or(&json!(0)),
                "completion_tokens": response.get("usageMetadata").and_then(|u| u.get("candidatesTokenCount")).unwrap_or(&json!(0)),
                "total_tokens": response.get("usageMetadata").and_then(|u| u.get("totalTokenCount")).unwrap_or(&json!(0))
            }
        }))
    }

    fn chat_endpoint(&self) -> &str {
        // Gemini uses model name in the path
        "/v1beta/models/gemini-pro:generateContent"
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-goog-api-key".to_string(), api_key.to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

/// Z.AI/GLM API adapter (OpenAI-compatible with different endpoint path)
pub struct ZaiAdapter;

impl ZaiAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ZaiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for ZaiAdapter {
    fn name(&self) -> &str {
        "zai"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        // Z.AI uses OpenAI-compatible format
        serde_json::to_value(request).map_err(|e| ProviderError::RequestFailed(e.to_string()))
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = serde_json::to_value(request)
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        // Add GLM-4.7 thinking params if enabled
        // See: https://docs.z.ai/guides/llm/glm-4.7
        if thinking.enabled {
            body["thinking"] = json!({
                "type": "enabled"
            });

            // Preserve thinking across turns if configured
            if thinking.preserve_across_turns {
                body["clear_thinking"] = json!(false);
            }

            debug!(
                "Added GLM thinking params: enabled=true, preserve={}",
                thinking.preserve_across_turns
            );
        } else {
            body["thinking"] = json!({
                "type": "disabled"
            });
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Response is already in OpenAI format
        // Note: reasoning_content field contains the thinking output
        Ok(response)
    }

    fn chat_endpoint(&self) -> &str {
        // Z.AI base URL includes version, so no /v1/ prefix needed
        "/chat/completions"
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

/// DeepSeek API adapter (OpenAI-compatible with thinking support)
pub struct DeepSeekAdapter;

impl DeepSeekAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DeepSeekAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for DeepSeekAdapter {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        serde_json::to_value(request).map_err(|e| ProviderError::RequestFailed(e.to_string()))
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = serde_json::to_value(request)
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        // Add DeepSeek R1 thinking params if enabled
        // See: https://api-docs.deepseek.com/guides/reasoning_model
        if thinking.enabled {
            body["enable_thinking"] = json!(true);
            debug!("Added DeepSeek thinking params: enable_thinking=true");
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Response is OpenAI format, reasoning_content contains thinking
        Ok(response)
    }

    fn chat_endpoint(&self) -> &str {
        "/v1/chat/completions"
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

/// Qwen/Alibaba API adapter (OpenAI-compatible with thinking support)
pub struct QwenAdapter;

impl QwenAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for QwenAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for QwenAdapter {
    fn name(&self) -> &str {
        "qwen"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        serde_json::to_value(request).map_err(|e| ProviderError::RequestFailed(e.to_string()))
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = serde_json::to_value(request)
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;

        // Add Qwen QwQ thinking params if enabled
        // See: https://help.aliyun.com/zh/model-studio/user-guide/qwq
        if thinking.enabled {
            body["enable_thinking"] = json!(true);
            debug!("Added Qwen thinking params: enable_thinking=true");
        } else {
            body["enable_thinking"] = json!(false);
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Response is OpenAI format
        Ok(response)
    }

    fn chat_endpoint(&self) -> &str {
        "/v1/chat/completions"
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

/// Ollama API adapter for local LLM inference
/// See: https://github.com/ollama/ollama/blob/main/docs/api.md
pub struct OllamaAdapter;

impl OllamaAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Convert OpenAI messages to Ollama format
    fn convert_messages(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| {
                let content = match &m.content {
                    MessageContent::Text(t) => t.clone(),
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .filter_map(|p| p.text.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                };

                json!({
                    "role": m.role,
                    "content": content
                })
            })
            .collect()
    }
}

impl Default for OllamaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for OllamaAdapter {
    fn name(&self) -> &str {
        "ollama"
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        let mut body = json!({
            "model": &request.model,
            "messages": Self::convert_messages(&request.messages),
            "stream": request.stream.unwrap_or(false)
        });

        // Add options for temperature and other settings
        let mut options = json!({});
        if let Some(temp) = request.temperature {
            options["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            options["num_predict"] = json!(max_tokens);
        }
        if options != json!({}) {
            body["options"] = options;
        }

        // Convert tools to Ollama format if present
        if let Some(tools) = &request.tools {
            let ollama_tools: Vec<Value> = tools
                .iter()
                .filter_map(|tool| {
                    let func = tool.get("function")?;
                    Some(json!({
                        "type": "function",
                        "function": {
                            "name": func.get("name")?,
                            "description": func.get("description").unwrap_or(&json!("")),
                            "parameters": func.get("parameters").unwrap_or(&json!({}))
                        }
                    }))
                })
                .collect();
            if !ollama_tools.is_empty() {
                body["tools"] = json!(ollama_tools);
            }
        }

        debug!(body = %body, "Transformed request for Ollama");
        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Ollama response format:
        // {"model": "...", "message": {"role": "assistant", "content": "..."}, "done": true, ...}
        let message = response.get("message").ok_or_else(|| {
            ProviderError::InvalidResponse("No message in Ollama response".to_string())
        })?;

        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("");

        // Handle tool calls if present
        let tool_calls: Option<Vec<Value>> = message
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|calls| {
                calls
                    .iter()
                    .enumerate()
                    .filter_map(|(i, call)| {
                        let func = call.get("function")?;
                        Some(json!({
                            "id": format!("call_{}", i),
                            "type": "function",
                            "function": {
                                "name": func.get("name")?,
                                "arguments": func.get("arguments")
                                    .map(|a| {
                                        if a.is_string() {
                                            a.as_str().unwrap_or("{}").to_string()
                                        } else {
                                            serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string())
                                        }
                                    })
                                    .unwrap_or_else(|| "{}".to_string())
                            }
                        }))
                    })
                    .collect()
            })
            .filter(|v: &Vec<Value>| !v.is_empty());

        let mut openai_message = json!({
            "role": "assistant",
            "content": content
        });

        if let Some(calls) = tool_calls {
            openai_message["tool_calls"] = json!(calls);
        }

        // Determine finish reason
        let done = response
            .get("done")
            .and_then(|d| d.as_bool())
            .unwrap_or(true);
        let finish_reason = if !done {
            "length"
        } else if openai_message.get("tool_calls").is_some() {
            "tool_calls"
        } else {
            "stop"
        };

        // Extract token counts if available
        let prompt_tokens = response
            .get("prompt_eval_count")
            .and_then(|c| c.as_u64())
            .unwrap_or(0);
        let completion_tokens = response
            .get("eval_count")
            .and_then(|c| c.as_u64())
            .unwrap_or(0);

        Ok(json!({
            "id": format!("ollama-{}", uuid::Uuid::new_v4()),
            "object": "chat.completion",
            "created": chrono::Utc::now().timestamp(),
            "model": response.get("model").and_then(|m| m.as_str()).unwrap_or("unknown"),
            "choices": [{
                "index": 0,
                "message": openai_message,
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens
            }
        }))
    }

    fn chat_endpoint(&self) -> &str {
        "/api/chat"
    }

    fn get_headers(&self, _api_key: &str) -> Vec<(String, String)> {
        // Ollama doesn't require authentication by default
        vec![("content-type".to_string(), "application/json".to_string())]
    }

    fn supports_model_listing(&self) -> bool {
        true
    }

    fn models_endpoint(&self) -> &str {
        // Ollama uses /api/tags for model listing, but also supports /v1/models
        "/v1/models"
    }
}

/// Get the appropriate adapter for a provider name
pub fn get_adapter(provider: &str) -> Box<dyn ProviderAdapter> {
    match provider.to_lowercase().as_str() {
        "anthropic" => Box::new(AnthropicAdapter::new()),
        "google" | "gemini" => Box::new(GoogleAdapter::new()),
        "zai" | "glm" | "zhipu" => Box::new(ZaiAdapter::new()),
        "deepseek" => Box::new(DeepSeekAdapter::new()),
        "qwen" | "alibaba" => Box::new(QwenAdapter::new()),
        "ollama" => Box::new(OllamaAdapter::new()),
        // OpenAI-compatible providers (default)
        // Includes: openai, local, lmstudio, localai, text-generation-webui, etc.
        "openai" | "local" | "lmstudio" | "localai" => Box::new(OpenAIAdapter::new()),
        // Default fallback for unknown providers (assume OpenAI-compatible)
        _ => Box::new(OpenAIAdapter::new()),
    }
}

/// Fetch available models from a provider's /v1/models endpoint
/// Works with OpenAI-compatible APIs (LM Studio, LocalAI, Ollama, etc.)
pub async fn fetch_models(
    base_url: &str,
    api_key: Option<&str>,
    adapter: &dyn ProviderAdapter,
) -> Result<Vec<ModelInfo>, ProviderError> {
    if !adapter.supports_model_listing() {
        return Err(ProviderError::Unsupported(format!(
            "Provider '{}' does not support model listing",
            adapter.name()
        )));
    }

    let client = reqwest::Client::new();

    // Normalize base_url: strip trailing slash and /v1 suffix to avoid double /v1/v1
    let normalized_base = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');
    let url = format!("{}{}", normalized_base, adapter.models_endpoint());

    let mut request = client.get(&url);

    // Add auth header if API key provided
    if let Some(key) = api_key {
        if !key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProviderError::RequestFailed(format!("Failed to fetch models: {}", e)))?;

    if !response.status().is_success() {
        return Err(ProviderError::RequestFailed(format!(
            "Models endpoint returned status {}",
            response.status()
        )));
    }

    let body: Value = response.json().await.map_err(|e| {
        ProviderError::InvalidResponse(format!("Failed to parse models response: {}", e))
    })?;

    // Parse OpenAI-style response: { "data": [...], "object": "list" }
    let models = body["data"]
        .as_array()
        .ok_or_else(|| {
            ProviderError::InvalidResponse("Expected 'data' array in response".to_string())
        })?
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            Some(ModelInfo {
                id,
                owned_by: m["owned_by"].as_str().map(String::from),
                created: m["created"].as_i64(),
            })
        })
        .collect();

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text("You are helpful.".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text("Hello!".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(1000),
            stream: None,
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_anthropic_transform_request() {
        let adapter = AnthropicAdapter::new();
        let request = create_test_request();
        let result = adapter.transform_request(&request).unwrap();

        assert_eq!(result["model"], "gpt-4");
        assert_eq!(result["max_tokens"], 1000);
        // Float comparison with tolerance
        let temp = result["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 0.01);

        // System should be array format with cache_control for prompt caching
        let system = result["system"].as_array().unwrap();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["type"], "text");
        assert_eq!(system[0]["text"], "You are helpful.");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");

        // Messages should not include system
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_anthropic_transform_response() {
        let adapter = AnthropicAdapter::new();
        let response = json!({
            "id": "msg_123",
            "model": "claude-3-sonnet",
            "content": [{"type": "text", "text": "Hello!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let result = adapter.transform_response(response, false).unwrap();

        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_anthropic_tool_caching() {
        // Test that tools have cache_control on the last tool
        let adapter = AnthropicAdapter::new();
        let mut request = create_test_request();
        request.tools = Some(vec![
            json!({
                "type": "function",
                "function": {
                    "name": "tool1",
                    "description": "First tool",
                    "parameters": {}
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "tool2",
                    "description": "Second tool",
                    "parameters": {}
                }
            }),
        ]);

        let result = adapter.transform_request(&request).unwrap();
        let tools = result["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 2);

        // First tool should NOT have cache_control
        assert!(tools[0].get("cache_control").is_none());

        // Last tool SHOULD have cache_control for prompt caching
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn test_openai_passthrough() {
        let adapter = OpenAIAdapter::new();
        let request = create_test_request();
        let result = adapter.transform_request(&request).unwrap();

        // Should preserve original structure
        assert_eq!(result["model"], "gpt-4");
        assert!(result["messages"].is_array());
    }

    #[test]
    fn test_google_transform_request() {
        let adapter = GoogleAdapter::new();
        let request = create_test_request();
        let result = adapter.transform_request(&request).unwrap();

        assert!(result["contents"].is_array());
        assert!(result["systemInstruction"].is_object());
        // Float comparison with tolerance
        let temp = result["generationConfig"]["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 0.01);
        assert_eq!(result["generationConfig"]["maxOutputTokens"], 1000);
    }

    #[test]
    fn test_get_adapter() {
        assert_eq!(get_adapter("anthropic").name(), "anthropic");
        assert_eq!(get_adapter("google").name(), "google");
        assert_eq!(get_adapter("openai").name(), "openai");
        assert_eq!(get_adapter("zai").name(), "zai");
        assert_eq!(get_adapter("glm").name(), "zai");
        assert_eq!(get_adapter("zhipu").name(), "zai");
        // DeepSeek and Qwen have dedicated adapters for thinking support
        assert_eq!(get_adapter("deepseek").name(), "deepseek");
        assert_eq!(get_adapter("qwen").name(), "qwen");
        assert_eq!(get_adapter("alibaba").name(), "qwen");
        // Ollama for local LLM inference
        assert_eq!(get_adapter("ollama").name(), "ollama");
        // OpenAI-compatible local providers
        assert_eq!(get_adapter("local").name(), "openai");
        assert_eq!(get_adapter("lmstudio").name(), "openai");
        assert_eq!(get_adapter("localai").name(), "openai");
        assert_eq!(get_adapter("unknown").name(), "openai"); // Default
    }

    #[test]
    fn test_ollama_adapter() {
        let adapter = OllamaAdapter::new();
        assert_eq!(adapter.name(), "ollama");
        assert_eq!(adapter.chat_endpoint(), "/api/chat");
    }

    #[test]
    fn test_ollama_transform_request() {
        let adapter = OllamaAdapter::new();
        let request = create_test_request();
        let result = adapter.transform_request(&request).unwrap();

        assert_eq!(result["model"], "gpt-4");
        assert!(result["messages"].is_array());
        // Ollama uses "options" for settings
        let temp = result["options"]["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 0.01);
        assert_eq!(result["options"]["num_predict"], 1000);
    }

    #[test]
    fn test_ollama_transform_response() {
        let adapter = OllamaAdapter::new();
        let response = json!({
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "Hello from Ollama!"
            },
            "done": true,
            "prompt_eval_count": 10,
            "eval_count": 5
        });

        let result = adapter.transform_response(response, false).unwrap();
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["model"], "llama3");
        assert_eq!(
            result["choices"][0]["message"]["content"],
            "Hello from Ollama!"
        );
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["usage"]["prompt_tokens"], 10);
        assert_eq!(result["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn test_zai_adapter() {
        let adapter = ZaiAdapter::new();
        assert_eq!(adapter.name(), "zai");
        // Z.AI uses /chat/completions without /v1/ prefix
        assert_eq!(adapter.chat_endpoint(), "/chat/completions");
    }

    #[test]
    fn test_zai_transform_request() {
        let adapter = ZaiAdapter::new();
        let request = create_test_request();
        let result = adapter.transform_request(&request).unwrap();

        // Should preserve OpenAI-compatible structure
        assert_eq!(result["model"], "gpt-4");
        assert!(result["messages"].is_array());
    }

    #[test]
    fn test_provider_error_variants() {
        // Test InvalidResponse variant
        let err = ProviderError::InvalidResponse("missing field".to_string());
        assert!(err.to_string().contains("missing field"));

        // Test Unsupported variant
        let err = ProviderError::Unsupported("streaming not available".to_string());
        assert!(err.to_string().contains("streaming"));

        // Test RequestFailed variant
        let err = ProviderError::RequestFailed("connection refused".to_string());
        assert!(err.to_string().contains("connection"));
    }

    #[test]
    fn test_openai_transform_response() {
        let adapter = OpenAIAdapter::new();
        let response = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }]
        });

        let result = adapter.transform_response(response.clone(), false).unwrap();
        // OpenAI adapter passes through unchanged
        assert_eq!(result, response);
    }

    #[test]
    fn test_google_transform_response() {
        let adapter = GoogleAdapter::new();
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello from Gemini!"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        });

        let result = adapter.transform_response(response, false).unwrap();
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(
            result["choices"][0]["message"]["content"],
            "Hello from Gemini!"
        );
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_google_transform_response_no_candidates() {
        let adapter = GoogleAdapter::new();
        let response = json!({"candidates": []});

        let result = adapter.transform_response(response, false);
        assert!(matches!(result, Err(ProviderError::InvalidResponse(_))));
    }

    #[test]
    fn test_convert_tool_result_with_error_flag() {
        let messages = vec![
            json!({"role": "user", "content": "test"}),
            json!({
                "role": "assistant",
                "content": "Let me try.",
                "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]
            }),
            json!({"role": "tool", "tool_call_id": "t1", "content": "[ERROR] command not found", "is_error": true}),
        ];
        let result = convert_messages_to_anthropic(&messages);
        // result[0]=user, result[1]=assistant+tool_use, result[2]=user+tool_result
        assert_eq!(result.len(), 3);
        let tool_msg = &result[2];
        assert_eq!(tool_msg["role"], "user");
        let content = tool_msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["is_error"], true);
    }

    #[test]
    fn test_convert_tool_result_without_error_flag() {
        let messages = vec![
            json!({"role": "user", "content": "test"}),
            json!({
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": [{"id": "t2", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}}]
            }),
            json!({"role": "tool", "tool_call_id": "t2", "content": "file contents here"}),
        ];
        let result = convert_messages_to_anthropic(&messages);
        assert_eq!(result.len(), 3);
        let tool_msg = &result[2];
        let content = tool_msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        // is_error should not be present for successful results
        assert!(content[0].get("is_error").is_none());
    }
}
