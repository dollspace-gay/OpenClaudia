//! Provider Adapters - Translate between OpenAI-compatible format and provider APIs.
//!
//! Supports:
//! - Anthropic Messages API
//! - `OpenAI` Chat Completions API
//! - Google Gemini API
//! - `DeepSeek` API (with thinking/reasoning support)
//! - Qwen/Alibaba API (with thinking support)
//! - Z.AI/GLM API (with thinking support)
//! - Ollama (local LLM inference)
//! - Any OpenAI-compatible server (LM Studio, `LocalAI`, etc.)
//!
//! Handles message format translation and tool/function calling conversion.

mod anthropic;
pub mod api_key;
mod deepseek;
mod google;
mod ollama;
mod openai;
mod openai_compat;
mod qwen;
mod zai;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;
use crate::session::TokenUsage;

// Re-export all adapter types and public functions
pub use anthropic::{
    build_system_blocks, build_system_blocks_from_string, convert_messages_to_anthropic,
    convert_tools_to_anthropic, AnthropicAdapter,
};
pub use api_key::{ApiKey, ApiKeyError};
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

    /// Transform an OpenAI-compatible request to provider format.
    ///
    /// # Errors
    ///
    /// Returns a `ProviderError` if the request cannot be transformed.
    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError>;

    /// Transform request with thinking config applied.
    ///
    /// # Errors
    ///
    /// Returns a `ProviderError` if the request cannot be transformed.
    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        // Default: ignore thinking config, just call transform_request
        let _ = thinking;
        self.transform_request(request)
    }

    /// Transform a provider response to OpenAI-compatible format.
    ///
    /// # Errors
    ///
    /// Returns a `ProviderError` if the response cannot be transformed.
    fn transform_response(&self, response: Value, stream: bool) -> Result<Value, ProviderError>;

    /// Get the endpoint path for chat completions.
    /// The model parameter allows providers like Google to build model-specific URLs.
    fn chat_endpoint(&self, _model: &str) -> String;

    /// Get required headers for this provider.
    ///
    /// The key is passed as an [`ApiKey`] rather than `&str` so that the
    /// only way to reach the raw secret is an explicit `.as_str()` call
    /// at the HTTP-header construction site — `Debug`/`Display` of an
    /// `ApiKey` always redact. See crosslink #256.
    fn get_headers(&self, api_key: &ApiKey) -> Vec<(String, String)>;

    /// Check if this provider supports model listing
    fn supports_model_listing(&self) -> bool {
        false
    }

    /// Get the models endpoint path (for providers that support it)
    fn models_endpoint(&self) -> &'static str {
        "/v1/models"
    }

    /// Extract the assistant text content from a *raw* provider response.
    ///
    /// This is the inverse of [`Self::transform_request`]: it consumes the
    /// upstream provider's native shape and returns the plain-text body
    /// of the assistant turn (no tool calls, no function payloads —
    /// just the text the user would see).
    ///
    /// Default implementation reads the `OpenAI` Chat Completions shape
    /// (`choices[0].message.content`). Providers with a different native
    /// response shape (Anthropic content blocks, Gemini `candidates`,
    /// Ollama `message.content`) override this with their own extractor.
    ///
    /// Returns `None` when no text content can be located — callers must
    /// treat that as "empty response" rather than fabricating a sentinel.
    ///
    /// See crosslink #479 — VDD previously rolled its own multi-shape
    /// extractor in `src/vdd/parsing.rs`, which silently returned an
    /// empty string for any provider it did not recognise (`DeepSeek`,
    /// `Qwen`, Z.AI). Routing through the adapter restores parity with
    /// the main proxy hot path.
    fn extract_response_text(&self, response: &Value) -> Option<String> {
        response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(std::string::ToString::to_string)
    }

    /// Extract token usage from a *raw* provider response.
    ///
    /// Default implementation reads the `OpenAI`/`Anthropic` shared
    /// `usage` object (`prompt_tokens`/`completion_tokens`, with
    /// fallback to `input_tokens`/`output_tokens` for Anthropic).
    /// Providers that use a different envelope (notably Gemini's
    /// `usageMetadata`) override this.
    ///
    /// Returns `None` when no usage data is present. Callers that need
    /// `TokenUsage::default()` semantics should call `.unwrap_or_default()`
    /// at the call site so the absence is visible in code review.
    ///
    /// See crosslink #479.
    fn extract_token_usage(&self, response: &Value) -> Option<TokenUsage> {
        let usage = response.get("usage")?;
        Some(TokenUsage {
            input_tokens: usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
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

/// Typed enum of every provider this proxy knows how to route to.
///
/// Replaces the stringly-typed `if/else-if` chain in `determine_provider`
/// (crosslink #332). All callers that need a wire-format name go through
/// [`ProviderKind::name`], which returns `&'static str` and allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Anthropic,
    OpenAI,
    Google,
    DeepSeek,
    Qwen,
    Zai,
    Unknown,
}

impl ProviderKind {
    /// Classify a model name to its provider. Explicit match arms,
    /// lowercased input, no overlapping prefix heuristics — `"o1"` no
    /// longer matches `"o100"`.
    #[must_use]
    pub fn from_model(model: &str) -> Self {
        let m = model.to_ascii_lowercase();
        if m.starts_with("claude") || m.starts_with("anthropic") {
            return Self::Anthropic;
        }
        if m.starts_with("gpt-") || m == "gpt" {
            return Self::OpenAI;
        }
        for prefix in ["o1-", "o3-", "o4-"] {
            if m.starts_with(prefix) {
                return Self::OpenAI;
            }
        }
        if matches!(m.as_str(), "o1" | "o3" | "o4") {
            return Self::OpenAI;
        }
        if m.starts_with("gemini") {
            return Self::Google;
        }
        if m.starts_with("deepseek") {
            return Self::DeepSeek;
        }
        if m.starts_with("qwen") || m.starts_with("qwq") || m.starts_with("qvq") {
            return Self::Qwen;
        }
        if m.starts_with("glm") {
            return Self::Zai;
        }
        Self::Unknown
    }

    /// Static name used as the key into `AppConfig.providers` and as the
    /// adapter selector in [`get_adapter`]. `Unknown` returns `"unknown"`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAI => "openai",
            Self::Google => "google",
            Self::DeepSeek => "deepseek",
            Self::Qwen => "qwen",
            Self::Zai => "zai",
            Self::Unknown => "unknown",
        }
    }
}

/// Get the appropriate adapter for a provider name
#[must_use]
pub fn get_adapter(provider: &str) -> Box<dyn ProviderAdapter> {
    match provider.to_lowercase().as_str() {
        "anthropic" => Box::new(AnthropicAdapter::new()),
        "google" | "gemini" => Box::new(GoogleAdapter::new()),
        "zai" | "glm" | "zhipu" => Box::new(ZaiAdapter::new()),
        "deepseek" => Box::new(DeepSeekAdapter::new()),
        "qwen" | "alibaba" => Box::new(QwenAdapter::new()),
        "ollama" => Box::new(OllamaAdapter::new()),
        // OpenAI-compatible providers: explicitly named
        "openai" | "local" | "lmstudio" | "localai" | "text-generation-webui" => {
            Box::new(OpenAIAdapter::new())
        }
        // Unknown provider: warn and fall back to OpenAI-compatible
        other => {
            tracing::warn!(
                provider = other,
                "Unknown provider — falling back to OpenAI-compatible adapter. Check config if this is a typo."
            );
            Box::new(OpenAIAdapter::new())
        }
    }
}

/// Fetch available models from a provider's `/v1/models` endpoint.
/// Works with OpenAI-compatible APIs (LM Studio, `LocalAI`, Ollama, etc.)
///
/// # Errors
///
/// Returns a `ProviderError` if the provider does not support model listing or the request fails.
pub async fn fetch_models(
    base_url: &str,
    api_key: Option<&ApiKey>,
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

    // Add auth header if API key provided. Unredacted access is confined to
    // `.as_str()` at the request boundary.
    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key.as_str()));
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProviderError::RequestFailed(format!("Failed to fetch models: {e}")))?;

    if !response.status().is_success() {
        return Err(ProviderError::RequestFailed(format!(
            "Models endpoint returned status {}",
            response.status()
        )));
    }

    let body: Value = response.json().await.map_err(|e| {
        ProviderError::InvalidResponse(format!("Failed to parse models response: {e}"))
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

    // ── Crosslink #332: ProviderKind typed dispatch ─────────────────────────

    #[test]
    fn provider_kind_name_round_trip() {
        assert_eq!(ProviderKind::Anthropic.name(), "anthropic");
        assert_eq!(ProviderKind::OpenAI.name(), "openai");
        assert_eq!(ProviderKind::Google.name(), "google");
        assert_eq!(ProviderKind::DeepSeek.name(), "deepseek");
        assert_eq!(ProviderKind::Qwen.name(), "qwen");
        assert_eq!(ProviderKind::Zai.name(), "zai");
        assert_eq!(ProviderKind::Unknown.name(), "unknown");
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAI,
            ProviderKind::Google,
            ProviderKind::DeepSeek,
            ProviderKind::Qwen,
            ProviderKind::Zai,
        ] {
            let adapter = get_adapter(kind.name());
            assert_eq!(
                adapter.name(),
                kind.name(),
                "adapter name drift for {kind:?}"
            );
        }
    }

    #[test]
    fn provider_kind_from_model_known_prefixes() {
        assert_eq!(
            ProviderKind::from_model("claude-opus-4"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::from_model("anthropic/claude-3"),
            ProviderKind::Anthropic
        );
        assert_eq!(ProviderKind::from_model("gpt-4o"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from_model("o1-preview"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from_model("o3-mini"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from_model("o4-pro"), ProviderKind::OpenAI);
        assert_eq!(
            ProviderKind::from_model("gemini-2.5-pro"),
            ProviderKind::Google
        );
        assert_eq!(
            ProviderKind::from_model("deepseek-r1"),
            ProviderKind::DeepSeek
        );
        assert_eq!(ProviderKind::from_model("qwen-long"), ProviderKind::Qwen);
        assert_eq!(ProviderKind::from_model("qwq-32b"), ProviderKind::Qwen);
        assert_eq!(ProviderKind::from_model("qvq-72b"), ProviderKind::Qwen);
        assert_eq!(ProviderKind::from_model("glm-4"), ProviderKind::Zai);
    }

    #[test]
    fn provider_kind_from_model_unknown_returns_unknown_variant() {
        assert_eq!(
            ProviderKind::from_model("some-unknown-model-xyz"),
            ProviderKind::Unknown
        );
        assert_eq!(
            ProviderKind::from_model("mistral-large"),
            ProviderKind::Unknown
        );
        assert_eq!(ProviderKind::from_model(""), ProviderKind::Unknown);
    }

    #[test]
    fn provider_kind_from_model_is_case_insensitive() {
        assert_eq!(
            ProviderKind::from_model("CLAUDE-3-OPUS"),
            ProviderKind::Anthropic
        );
        assert_eq!(ProviderKind::from_model("GPT-4"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from_model("Gemini-Pro"), ProviderKind::Google);
    }

    #[test]
    fn provider_kind_from_model_no_false_positive_on_o_prefix() {
        assert_eq!(ProviderKind::from_model("o100"), ProviderKind::Unknown);
        assert_eq!(
            ProviderKind::from_model("observer-1"),
            ProviderKind::Unknown
        );
        assert_eq!(ProviderKind::from_model("o1"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from_model("o3"), ProviderKind::OpenAI);
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

    // ── Crosslink #479: ProviderAdapter response-text / token-usage ─────────
    //
    // These methods replaced the free functions in `src/vdd/parsing.rs` that
    // hardcoded OpenAI/Anthropic/Gemini response shapes and silently returned
    // empty defaults for any other provider. The tests below pin the new
    // contract: each adapter understands its OWN native response shape, and
    // unsupported providers fall back to the trait default (OpenAI shape).

    #[test]
    fn anthropic_extract_response_text_reads_native_content_blocks() {
        let adapter = AnthropicAdapter::new();
        let response = json!({
            "content": [
                {"type": "text", "text": "hello from Claude"},
                {"type": "tool_use", "id": "tu_1", "name": "x", "input": {}}
            ]
        });
        assert_eq!(
            adapter.extract_response_text(&response),
            Some("hello from Claude".to_string())
        );
    }

    #[test]
    fn anthropic_extract_token_usage_reads_native_envelope_with_cache() {
        let adapter = AnthropicAdapter::new();
        let response = json!({
            "usage": {
                "input_tokens": 123,
                "output_tokens": 45,
                "cache_read_input_tokens": 17,
                "cache_creation_input_tokens": 8
            }
        });
        let usage = adapter
            .extract_token_usage(&response)
            .expect("usage present");
        assert_eq!(usage.input_tokens, 123);
        assert_eq!(usage.output_tokens, 45);
        assert_eq!(usage.cache_read_tokens, 17);
        assert_eq!(usage.cache_write_tokens, 8);
    }

    #[test]
    fn google_extract_response_text_concatenates_parts() {
        let adapter = GoogleAdapter::new();
        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "hello "},
                        {"text": "from Gemini"}
                    ]
                }
            }]
        });
        assert_eq!(
            adapter.extract_response_text(&response),
            Some("hello from Gemini".to_string())
        );
    }

    #[test]
    fn google_extract_token_usage_reads_usage_metadata() {
        let adapter = GoogleAdapter::new();
        let response = json!({
            "usageMetadata": {
                "promptTokenCount": 200,
                "candidatesTokenCount": 90,
                "cachedContentTokenCount": 30
            }
        });
        let usage = adapter
            .extract_token_usage(&response)
            .expect("usage present");
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 90);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn ollama_extract_response_text_reads_message_content() {
        let adapter = OllamaAdapter::new();
        let response = json!({
            "model": "llama3",
            "message": {"role": "assistant", "content": "hi from Ollama"},
            "done": true
        });
        assert_eq!(
            adapter.extract_response_text(&response),
            Some("hi from Ollama".to_string())
        );
    }

    #[test]
    fn ollama_extract_token_usage_reads_top_level_counters() {
        let adapter = OllamaAdapter::new();
        let response = json!({
            "prompt_eval_count": 22,
            "eval_count": 11
        });
        let usage = adapter
            .extract_token_usage(&response)
            .expect("usage present");
        assert_eq!(usage.input_tokens, 22);
        assert_eq!(usage.output_tokens, 11);
        // Ollama has no cache layer
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    /// The default trait impl reads the `OpenAI` Chat Completions shape.
    /// `DeepSeek`/`Qwen`/Z.AI all share that shape so they now succeed
    /// where the old hand-rolled extractor would also have succeeded — but
    /// any *future* OpenAI-compatible provider added to `get_adapter` will
    /// keep working without VDD-specific patches.
    #[test]
    fn deepseek_extract_response_text_via_default_openai_shape() {
        let adapter = DeepSeekAdapter::new();
        let response = json!({
            "choices": [{"message": {"content": "deepseek reply"}}]
        });
        assert_eq!(
            adapter.extract_response_text(&response),
            Some("deepseek reply".to_string())
        );
    }

    #[test]
    fn qwen_extract_token_usage_via_default_openai_shape() {
        let adapter = QwenAdapter::new();
        let response = json!({
            "usage": {"prompt_tokens": 7, "completion_tokens": 3}
        });
        let usage = adapter
            .extract_token_usage(&response)
            .expect("usage present");
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 3);
    }

    /// A response with NO recognisable usage envelope must return `None`,
    /// not silently fabricate a zero-token record. Forces callers to make
    /// a conscious choice (e.g. `.unwrap_or_default()`).
    #[test]
    fn extract_token_usage_returns_none_for_unknown_shape() {
        let adapter = GoogleAdapter::new();
        let response = json!({"unrelated": "payload"});
        assert!(adapter.extract_token_usage(&response).is_none());

        let adapter = OllamaAdapter::new();
        let response = json!({"message": {"content": "x"}}); // no counters
        assert!(adapter.extract_token_usage(&response).is_none());
    }

    /// A response with NO recognisable text content must return `None` —
    /// callers see the absence rather than an empty string sentinel.
    #[test]
    fn extract_response_text_returns_none_for_unknown_shape() {
        let adapter = AnthropicAdapter::new();
        let response = json!({"id": "msg_1", "model": "x"});
        assert!(adapter.extract_response_text(&response).is_none());

        let adapter = OllamaAdapter::new();
        let response = json!({"model": "llama3"});
        assert!(adapter.extract_response_text(&response).is_none());
    }
}
