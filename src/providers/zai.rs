//! Z.AI/GLM API adapter (OpenAI-compatible with thinking support).

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

use super::{ProviderAdapter, ProviderError};

/// Z.AI/GLM API adapter (OpenAI-compatible with different endpoint path)
pub struct ZaiAdapter;

impl ZaiAdapter {
    #[must_use]
    pub const fn new() -> Self {
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
    fn name(&self) -> &'static str {
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

    fn chat_endpoint(&self, _model: &str) -> String {
        // Z.AI base URL includes version, so no /v1/ prefix needed
        "/chat/completions".to_string()
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {api_key}")),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}
