//! Google Gemini API adapter.

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

use super::{ProviderAdapter, ProviderError};

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

    fn chat_endpoint(&self, model: &str) -> String {
        // Gemini uses model name in the URL path
        format!("/v1beta/models/{}:generateContent", model)
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("x-goog-api-key".to_string(), api_key.to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}
