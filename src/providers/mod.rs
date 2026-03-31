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

mod anthropic;
mod deepseek;
mod google;
mod ollama;
mod openai;
mod qwen;
mod zai;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

// Re-export all adapter types and public functions
pub use anthropic::{convert_messages_to_anthropic, convert_tools_to_anthropic, AnthropicAdapter};
pub use deepseek::DeepSeekAdapter;
pub use google::GoogleAdapter;
pub use ollama::OllamaAdapter;
pub use openai::OpenAIAdapter;
pub use qwen::QwenAdapter;
pub use zai::ZaiAdapter;

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

    /// Get the endpoint path for chat completions.
    /// The model parameter allows providers like Google to build model-specific URLs.
    fn chat_endpoint(&self, _model: &str) -> String;

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
    use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
    use serde_json::json;

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
        assert_eq!(adapter.chat_endpoint("llama3"), "/api/chat");
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
        assert_eq!(adapter.chat_endpoint("glm-4"), "/chat/completions");
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
