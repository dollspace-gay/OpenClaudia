//! Ollama API adapter for local LLM inference.
//!
//! See: <https://github.com/ollama/ollama/blob/main/docs/api.md>

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

use super::{ProviderAdapter, ProviderError};

/// Ollama API adapter for local LLM inference
/// See: <https://github.com/ollama/ollama/blob/main/docs/api.md>
pub struct OllamaAdapter;

impl OllamaAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Convert `OpenAI` messages to Ollama format
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
    fn name(&self) -> &'static str {
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
                    let default_desc = json!("");
                    let default_params = json!({});
                    Some(json!({
                        "type": "function",
                        "function": {
                            "name": func.get("name")?,
                            "description": func.get("description").unwrap_or(&default_desc),
                            "parameters": func.get("parameters").unwrap_or(&default_params)
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
                    .filter_map(|call| {
                        let func = call.get("function")?;
                        Some(json!({
                            "id": format!("call_{}", uuid::Uuid::new_v4()),
                            "type": "function",
                            "function": {
                                "name": func.get("name")?,
                                "arguments": func.get("arguments")
                                    .map_or_else(|| "{}".to_string(), |a| {
                                        if a.is_string() {
                                            a.as_str().unwrap_or("{}").to_string()
                                        } else {
                                            serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string())
                                        }
                                    })
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
            .and_then(serde_json::Value::as_bool)
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
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let completion_tokens = response
            .get("eval_count")
            .and_then(serde_json::Value::as_u64)
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

    fn chat_endpoint(&self, _model: &str) -> String {
        "/api/chat".to_string()
    }

    fn get_headers(&self, _api_key: &str) -> Vec<(String, String)> {
        // Ollama doesn't require authentication by default
        vec![("content-type".to_string(), "application/json".to_string())]
    }

    fn supports_model_listing(&self) -> bool {
        true
    }

    fn models_endpoint(&self) -> &'static str {
        // Ollama uses /api/tags for model listing, but also supports /v1/models
        "/v1/models"
    }
}
