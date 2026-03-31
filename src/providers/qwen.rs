//! Qwen/Alibaba API adapter (OpenAI-compatible with thinking support).

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

use super::{ProviderAdapter, ProviderError};

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

    fn chat_endpoint(&self, _model: &str) -> String {
        "/v1/chat/completions".to_string()
    }

    fn get_headers(&self, api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", api_key)),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}
