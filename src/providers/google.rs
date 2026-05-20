//! Google Gemini API adapter.

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use crate::session::TokenUsage;

use super::{ProviderAdapter, ProviderError};

/// Build a deterministic Gemini tool-call id from `(ordinal, function name)`.
///
/// Crosslink #785: parsing the same Gemini response twice must produce the
/// same `tool_calls[i].id` so callers can cache / diff / log-correlate
/// without burning an entry per re-parse. The ordinal prefix disambiguates
/// repeated calls to the same function in a single turn.
fn gemini_tool_call_id(ordinal: usize, function_name: &str) -> String {
    format!("call_{ordinal}_{function_name}")
}

/// Google Gemini API adapter
pub struct GoogleAdapter;

impl GoogleAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Convert `OpenAI` messages to Gemini format
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
                        // Crosslink #850: a `ContentPart` with neither `text`
                        // nor `image_url` used to fall through to an empty
                        // `{"text": ""}` part — silently dropping video / audio
                        // / file / any future variant the user sent. Emit a
                        // `tracing::warn` naming the unknown content type so
                        // the gap is observable in logs, then skip the part
                        // instead of fabricating empty text.
                        let converted: Vec<Value> = parts
                            .iter()
                            .filter_map(|p| {
                                p.text.as_ref().map(|t| json!({"text": t})).or_else(|| {
                                    p.image_url
                                        .as_ref()
                                        .map(|image| json!({"inlineData": image}))
                                        .or_else(|| {
                                            tracing::warn!(
                                                content_type = ?p.content_type,
                                                role = %m.role,
                                                "dropping unknown content type in Google adapter \
                                                 (not text / image_url) — see crosslink #850"
                                            );
                                            None
                                        })
                                })
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

    /// Convert `OpenAI` tools to Gemini function declarations
    fn convert_tools(tools: &[Value]) -> Value {
        let functions: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                Some(json!({
                    "name": func.get("name")?,
                    "description": func.get("description").cloned().unwrap_or_else(|| json!("")),
                    "parameters": func.get("parameters").cloned().unwrap_or_else(|| json!({}))
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
    fn name(&self) -> &'static str {
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
        // Check for API error responses before extracting candidates
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            let code = error
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            return Err(ProviderError::InvalidResponse(format!(
                "Gemini API error ({code}): {message}"
            )));
        }

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

        // Extract function calls.
        //
        // Crosslink #785: tool-call ids are derived deterministically from
        // `(ordinal, function_name)` so re-parsing the same upstream payload
        // produces identical ids — restores parse idempotency that the prior
        // `Uuid::new_v4()` formulation broke. The ordinal disambiguates
        // multiple calls to the same function in one turn (e.g. two `bash`
        // calls in a row do NOT collide on id `call_0_bash` / `call_1_bash`).
        let tool_calls: Option<Vec<Value>> = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("functionCall"))
                    .enumerate()
                    .filter_map(|(i, func_call)| {
                        let name = func_call.get("name")?.as_str()?;
                        let args = serde_json::to_string(func_call.get("args")?).ok()?;
                        Some(json!({
                            "id": gemini_tool_call_id(i, name),
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": args,
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
            .map_or("stop", |r| match r {
                "MAX_TOKENS" => "length",
                "SAFETY" => "content_filter",
                _ => "stop",
            });

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
                "prompt_tokens": response.get("usageMetadata").and_then(|u| u.get("promptTokenCount")).cloned().unwrap_or_else(|| json!(0)),
                "completion_tokens": response.get("usageMetadata").and_then(|u| u.get("candidatesTokenCount")).cloned().unwrap_or_else(|| json!(0)),
                "total_tokens": response.get("usageMetadata").and_then(|u| u.get("totalTokenCount")).cloned().unwrap_or_else(|| json!(0))
            }
        }))
    }

    fn chat_endpoint(&self, model: &str) -> String {
        // Gemini uses model name in the URL path
        format!("/v1beta/models/{model}:generateContent")
    }

    fn get_headers(&self, api_key: &super::ApiKey) -> Vec<(String, String)> {
        vec![
            ("x-goog-api-key".to_string(), api_key.as_str().to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }

    /// Gemini native shape: `candidates[0].content.parts[].text`. Text
    /// parts are concatenated so the result matches what
    /// [`Self::transform_response`] would surface to the proxy hot
    /// path. See crosslink #479.
    fn extract_response_text(&self, response: &Value) -> Option<String> {
        let parts = response
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())?;
        let joined: String = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    /// Gemini `usageMetadata` envelope: `promptTokenCount`,
    /// `candidatesTokenCount`, `cachedContentTokenCount` (mapped to
    /// `cache_read_tokens`). Gemini exposes no cache-write counter, so
    /// that field is reported as zero rather than fabricated.
    /// See crosslink #479.
    fn extract_token_usage(&self, response: &Value) -> Option<TokenUsage> {
        let usage = response.get("usageMetadata")?;
        Some(TokenUsage {
            input_tokens: usage
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read_tokens: usage
                .get("cachedContentTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write_tokens: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::ContentPart;

    /// #785: parsing the same Gemini response twice must yield identical
    /// `tool_calls[*].id` so callers can correlate / cache / diff across
    /// re-parses. The pre-fix code generated a fresh `Uuid::new_v4()`
    /// every time, so two parses of the same payload never matched.
    #[test]
    fn tool_call_ids_are_deterministic_across_reparses() {
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "bash", "args": {"command": "ls"}}},
                        {"functionCall": {"name": "read", "args": {"path": "src/lib.rs"}}}
                    ]
                }
            }]
        });
        let adapter = GoogleAdapter::new();
        let a = adapter.transform_response(body.clone(), false).unwrap();
        let b = adapter.transform_response(body, false).unwrap();
        let ids_a: Vec<&str> = a["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        let ids_b: Vec<&str> = b["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids_a, ids_b, "#785: re-parse must yield identical ids");
        // The shape is `call_<ordinal>_<name>`.
        assert_eq!(ids_a, vec!["call_0_bash", "call_1_read"]);
    }

    /// #785: two consecutive calls to the same function in a single turn
    /// must produce distinct ids — the ordinal disambiguates.
    #[test]
    fn repeated_function_calls_get_distinct_ordinal_ids() {
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "bash", "args": {"command": "ls"}}},
                        {"functionCall": {"name": "bash", "args": {"command": "pwd"}}}
                    ]
                }
            }]
        });
        let parsed = GoogleAdapter::new()
            .transform_response(body, false)
            .unwrap();
        let calls = parsed["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_0_bash");
        assert_eq!(calls[1]["id"], "call_1_bash");
        assert_ne!(calls[0]["id"], calls[1]["id"]);
    }

    /// #850: a `ContentPart` with neither `text` nor `image_url` must be
    /// dropped (and warned about) rather than silently coerced to an
    /// empty text part — the latter loses the multimodal contract.
    #[test]
    fn unknown_content_part_is_dropped_not_emitted_as_empty_text() {
        use crate::proxy::{ChatMessage, MessageContent};
        let msg = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Parts(vec![
                ContentPart {
                    content_type: "text".to_string(),
                    text: Some("hello".to_string()),
                    image_url: None,
                },
                ContentPart {
                    // Unrecognized variant — neither text nor image_url set.
                    content_type: "video_url".to_string(),
                    text: None,
                    image_url: None,
                },
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };
        let out = GoogleAdapter::convert_messages(std::slice::from_ref(&msg));
        // One message, with `parts` containing only the recognized text.
        let parts = out[0]["parts"].as_array().expect("parts is array");
        assert_eq!(parts.len(), 1, "#850: unknown content type must be dropped");
        assert_eq!(parts[0]["text"], "hello");
        // The pre-fix code emitted `{"text": ""}` — must NOT appear.
        assert!(
            !parts.iter().any(|p| p["text"] == ""),
            "#850: must not coerce unknown variant to empty text"
        );
    }
}
