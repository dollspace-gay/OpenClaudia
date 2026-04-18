//! `OpenAI` Chat Completions API adapter (mostly passthrough).

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

use super::{ProviderAdapter, ProviderError};

/// `OpenAI` API adapter (mostly passthrough)
pub struct OpenAIAdapter;

impl OpenAIAdapter {
    #[must_use]
    pub const fn new() -> Self {
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
    fn name(&self) -> &'static str {
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
        // Only valid for reasoning models (o1, o3, o4 series)
        if thinking.enabled {
            let model = request.model.as_str();
            let is_reasoning_model =
                model.starts_with("o1") || model.starts_with("o3") || model.starts_with("o4");
            if is_reasoning_model {
                let effort = thinking.reasoning_effort.as_deref().unwrap_or("medium");
                body["reasoning_effort"] = json!(effort);
                debug!("Added OpenAI reasoning params: effort={}", effort);
            } else {
                debug!(
                    "Skipping reasoning_effort for non-reasoning model: {}",
                    model
                );
            }
        }

        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // Response is already in OpenAI format
        Ok(response)
    }

    fn chat_endpoint(&self, _model: &str) -> String {
        "/v1/chat/completions".to_string()
    }

    fn get_headers(&self, api_key: &super::ApiKey) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", api_key.as_str()),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }

    fn supports_model_listing(&self) -> bool {
        true
    }
}
