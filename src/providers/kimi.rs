//! Kimi (Moonshot) API adapter (OpenAI-compatible).
//!
//! Thin newtype around [`OpenAiCompatibleAdapter`]. Kimi K2.7 is
//! OpenAI-compatible with standard chat completions.

use async_trait::async_trait;
use serde_json::Value;

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

use super::openai_compat::{OpenAiCompatibleAdapter, ThinkingInjector};
use super::{ApiKey, ProviderAdapter, ProviderError};

/// Kimi (Moonshot) API adapter (OpenAI-compatible).
pub struct KimiAdapter(OpenAiCompatibleAdapter);

impl KimiAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self(OpenAiCompatibleAdapter::new(
            "kimi",
            "/v1/chat/completions",
            ThinkingInjector::OpenAiReasoningEffort,
            false,
        ))
    }
}

impl Default for KimiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderAdapter for KimiAdapter {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        self.0.transform_request(request)
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        self.0.transform_request_with_thinking(request, thinking)
    }

    fn transform_response(&self, response: Value, stream: bool) -> Result<Value, ProviderError> {
        self.0.transform_response(response, stream)
    }

    fn chat_endpoint(&self, model: &str) -> String {
        self.0.chat_endpoint(model)
    }

    fn get_headers(&self, api_key: &ApiKey) -> Vec<(String, String)> {
        self.0.get_headers(api_key)
    }

    fn supports_model_listing(&self) -> bool {
        self.0.supports_model_listing()
    }
}
