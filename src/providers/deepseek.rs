//! `DeepSeek` API adapter (OpenAI-compatible with thinking support).

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

use super::{ProviderAdapter, ProviderError};

/// `DeepSeek` API adapter (OpenAI-compatible with thinking support)
pub struct DeepSeekAdapter;

impl DeepSeekAdapter {
    #[must_use]
    pub const fn new() -> Self {
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
    fn name(&self) -> &'static str {
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

    fn chat_endpoint(&self, _model: &str) -> String {
        "/v1/chat/completions".to_string()
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {api_key}")),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}
