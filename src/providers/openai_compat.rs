//! Shared `OpenAI`-compatible adapter for providers that accept Chat Completions
//! requests on the wire (`OpenAI`, `DeepSeek`, Qwen, Z.AI/GLM, plus the generic
//! `LM Studio` / `LocalAI` / `text-generation-webui` fallbacks).
//!
//! All four upstream services accept the same canonical `OpenAI`
//! `ChatCompletionRequest` body and return responses in `OpenAI` shape.
//! They differ only on:
//!
//! - the provider `name`,
//! - the chat-completions URL suffix (most use `/v1/chat/completions`;
//!   Z.AI uses `/chat/completions` because its base URL already includes the
//!   API version),
//! - the thinking/reasoning toggle keyed into the request body
//!   (different providers spell the toggle differently — `enable_thinking`,
//!   `thinking: { type: enabled }`, `reasoning_effort`, etc.),
//! - whether `/v1/models` listing is supported.
//!
//! Rather than carry four near-identical copies of `ProviderAdapter`
//! impls, those variations are parameterized on this struct.
//!
//! Behavioural preservation: every request emitted by this adapter is
//! byte-identical to what the prior per-provider adapter produced. See
//! crosslink #281.

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::config::ThinkingConfig;
use crate::proxy::ChatCompletionRequest;

use super::{ApiKey, ProviderAdapter, ProviderError};

/// How the provider expects the "thinking mode" toggle to be encoded in
/// the request body.
///
/// Each variant captures the exact JSON shape the corresponding upstream
/// API expects. The variants are chosen narrowly: keeping a discriminated
/// enum (rather than `fn(&mut Value, &ThinkingConfig, &str)`) keeps the
/// behavioural matrix inspectable in one place, keeps the per-adapter
/// constructors `const fn`, and avoids handing out raw function pointers.
#[derive(Debug, Clone, Copy)]
pub(super) enum ThinkingInjector {
    /// `OpenAI` o1/o3/o4 reasoning models.
    ///
    /// Sets `reasoning_effort` to the configured value (default
    /// `"medium"`) if and only if `thinking.enabled` is true AND the
    /// target model is in the reasoning family. Non-reasoning models
    /// (e.g. `gpt-4`) get no change — matching prior `OpenAIAdapter`
    /// behaviour.
    OpenAiReasoningEffort,
    /// `DeepSeek` R1 / reasoner.
    ///
    /// Sets `enable_thinking: true` if and only if `thinking.enabled`.
    /// When disabled, no field is added — matching prior
    /// `DeepSeekAdapter` behaviour (silent no-op).
    DeepSeekEnableThinking,
    /// Qwen `QwQ` / Alibaba.
    ///
    /// Always sets `enable_thinking` to the value of `thinking.enabled`
    /// (i.e. `false` is explicitly written, unlike `DeepSeek`). This
    /// matches prior `QwenAdapter` behaviour.
    QwenEnableThinking,
    /// Z.AI / GLM-4.7.
    ///
    /// Sets the `thinking` object to `{type:"enabled"}` or
    /// `{type:"disabled"}`. When enabled and `preserve_across_turns` is
    /// set, also emits `clear_thinking: false`. Matches prior
    /// `ZaiAdapter` behaviour.
    GlmThinking,
}

impl ThinkingInjector {
    /// Mutates `body` to add the provider-specific thinking parameters.
    ///
    /// Pre-condition: `body` is the value produced by
    /// `serde_json::to_value(request)` (an object). The `model` argument
    /// is supplied for variants that gate behaviour on the model name
    /// (currently only `OpenAiReasoningEffort`).
    fn inject(self, body: &mut Value, thinking: &ThinkingConfig, model: &str) {
        match self {
            Self::OpenAiReasoningEffort => {
                if thinking.enabled {
                    let is_reasoning_model = model.starts_with("o1")
                        || model.starts_with("o3")
                        || model.starts_with("o4");
                    let effort = thinking.reasoning_effort.as_deref().unwrap_or("medium");
                    if is_reasoning_model {
                        body["reasoning_effort"] = json!(effort);
                        debug!("Added OpenAI reasoning params: effort={}", effort);
                    } else {
                        // Crosslink #779: previously a `debug!` log silently
                        // swallowed the user's `reasoning_effort` config for
                        // non-reasoning models. Operators who set
                        // `effort=high` for `gpt-4` had no signal their
                        // request was downgraded. Now we surface it as a
                        // warning that names both the model and the ignored
                        // effort value.
                        warn!(
                            model = %model,
                            ignored_reasoning_effort = %effort,
                            "ignoring reasoning_effort: model is not in the o1/o3/o4 reasoning family — \
                             configured thinking is a no-op for this model",
                        );
                    }
                }
            }
            Self::DeepSeekEnableThinking => {
                if thinking.enabled {
                    body["enable_thinking"] = json!(true);
                    debug!("Added DeepSeek thinking params: enable_thinking=true");
                }
            }
            Self::QwenEnableThinking => {
                if thinking.enabled {
                    body["enable_thinking"] = json!(true);
                    debug!("Added Qwen thinking params: enable_thinking=true");
                } else {
                    body["enable_thinking"] = json!(false);
                }
            }
            Self::GlmThinking => {
                if thinking.enabled {
                    body["thinking"] = json!({ "type": "enabled" });
                    if thinking.preserve_across_turns {
                        body["clear_thinking"] = json!(false);
                    }
                    debug!(
                        "Added GLM thinking params: enabled=true, preserve={}",
                        thinking.preserve_across_turns
                    );
                } else {
                    body["thinking"] = json!({ "type": "disabled" });
                }
            }
        }
    }
}

/// Shared `OpenAI`-compatible adapter parameterized on the provider's
/// name, endpoint, thinking-mode encoding, and capabilities.
///
/// Provider-specific adapters (`OpenAIAdapter`, `DeepSeekAdapter`,
/// `QwenAdapter`, `ZaiAdapter`) are thin newtypes around an instance of
/// this struct.
pub(super) struct OpenAiCompatibleAdapter {
    name: &'static str,
    /// Path returned from [`ProviderAdapter::chat_endpoint`]. Stored as
    /// `&'static str` (rather than a closure) because every observed
    /// variation is a string constant — three providers use
    /// `/v1/chat/completions`, Z.AI uses `/chat/completions`.
    chat_path: &'static str,
    thinking: ThinkingInjector,
    supports_models: bool,
}

impl OpenAiCompatibleAdapter {
    pub(super) const fn new(
        name: &'static str,
        chat_path: &'static str,
        thinking: ThinkingInjector,
        supports_models: bool,
    ) -> Self {
        Self {
            name,
            chat_path,
            thinking,
            supports_models,
        }
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiCompatibleAdapter {
    fn name(&self) -> &str {
        self.name
    }

    fn transform_request(&self, request: &ChatCompletionRequest) -> Result<Value, ProviderError> {
        // OpenAI Chat Completions is our canonical wire format, so the
        // body is just the serialized request.
        serde_json::to_value(request).map_err(|e| ProviderError::RequestFailed(e.to_string()))
    }

    fn transform_request_with_thinking(
        &self,
        request: &ChatCompletionRequest,
        thinking: &ThinkingConfig,
    ) -> Result<Value, ProviderError> {
        let mut body = serde_json::to_value(request)
            .map_err(|e| ProviderError::RequestFailed(e.to_string()))?;
        self.thinking.inject(&mut body, thinking, &request.model);
        Ok(body)
    }

    fn transform_response(&self, response: Value, _stream: bool) -> Result<Value, ProviderError> {
        // All OpenAI-compatible providers return responses already in
        // OpenAI shape. `reasoning_content` (when present) is carried
        // through verbatim for DeepSeek/Qwen/Z.AI thinking output.
        Ok(response)
    }

    fn chat_endpoint(&self, _model: &str) -> String {
        self.chat_path.to_string()
    }

    fn get_headers(&self, api_key: &ApiKey) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", api_key.as_str()),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }

    fn supports_model_listing(&self) -> bool {
        self.supports_models
    }
}

#[cfg(test)]
mod tests {
    //! Behaviour-preservation tests for the shared adapter.
    //!
    //! Each test pins the JSON shape that the prior per-provider adapter
    //! emitted, so any regression in the shared transform is caught
    //! independently of the provider-level test surface in `mod.rs`.

    use super::*;
    use crate::proxy::{ChatMessage, MessageContent};
    use std::collections::HashMap;

    fn req(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: MessageContent::Text("hi".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: HashMap::new(),
        }
    }

    fn thinking_on() -> ThinkingConfig {
        ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            preserve_across_turns: false,
            reasoning_effort: None,
        }
    }

    fn thinking_off() -> ThinkingConfig {
        ThinkingConfig {
            enabled: false,
            ..thinking_on()
        }
    }

    #[test]
    fn openai_reasoning_effort_set_for_reasoning_model() {
        let injector = ThinkingInjector::OpenAiReasoningEffort;
        let mut body = serde_json::to_value(req("o3-mini")).unwrap();
        injector.inject(&mut body, &thinking_on(), "o3-mini");
        assert_eq!(body["reasoning_effort"], "medium");
    }

    #[test]
    fn openai_reasoning_effort_honours_configured_level() {
        let mut t = thinking_on();
        t.reasoning_effort = Some("high".to_string());
        let mut body = serde_json::to_value(req("o1-preview")).unwrap();
        ThinkingInjector::OpenAiReasoningEffort.inject(&mut body, &t, "o1-preview");
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn openai_reasoning_effort_absent_for_non_reasoning_model() {
        let mut body = serde_json::to_value(req("gpt-4")).unwrap();
        ThinkingInjector::OpenAiReasoningEffort.inject(&mut body, &thinking_on(), "gpt-4");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn openai_reasoning_effort_absent_when_disabled() {
        let mut body = serde_json::to_value(req("o3-mini")).unwrap();
        ThinkingInjector::OpenAiReasoningEffort.inject(&mut body, &thinking_off(), "o3-mini");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn deepseek_thinking_enabled_only_writes_when_on() {
        let mut on = serde_json::to_value(req("deepseek-reasoner")).unwrap();
        ThinkingInjector::DeepSeekEnableThinking.inject(
            &mut on,
            &thinking_on(),
            "deepseek-reasoner",
        );
        assert_eq!(on["enable_thinking"], true);

        let mut off = serde_json::to_value(req("deepseek-reasoner")).unwrap();
        ThinkingInjector::DeepSeekEnableThinking.inject(
            &mut off,
            &thinking_off(),
            "deepseek-reasoner",
        );
        // DeepSeek prior behaviour: no field at all when disabled.
        assert!(off.get("enable_thinking").is_none());
    }

    #[test]
    fn qwen_thinking_always_writes_explicit_bool() {
        let mut on = serde_json::to_value(req("qwq-32b")).unwrap();
        ThinkingInjector::QwenEnableThinking.inject(&mut on, &thinking_on(), "qwq-32b");
        assert_eq!(on["enable_thinking"], true);

        let mut off = serde_json::to_value(req("qwq-32b")).unwrap();
        ThinkingInjector::QwenEnableThinking.inject(&mut off, &thinking_off(), "qwq-32b");
        // Qwen prior behaviour: explicit false when disabled.
        assert_eq!(off["enable_thinking"], false);
    }

    #[test]
    fn glm_thinking_encodes_enabled_disabled_object() {
        let mut on = serde_json::to_value(req("glm-4.7")).unwrap();
        ThinkingInjector::GlmThinking.inject(&mut on, &thinking_on(), "glm-4.7");
        assert_eq!(on["thinking"]["type"], "enabled");
        // No preserve flag by default.
        assert!(on.get("clear_thinking").is_none());

        let mut off = serde_json::to_value(req("glm-4.7")).unwrap();
        ThinkingInjector::GlmThinking.inject(&mut off, &thinking_off(), "glm-4.7");
        assert_eq!(off["thinking"]["type"], "disabled");
    }

    #[test]
    fn glm_thinking_preserve_emits_clear_thinking_false() {
        let mut t = thinking_on();
        t.preserve_across_turns = true;
        let mut body = serde_json::to_value(req("glm-4.7")).unwrap();
        ThinkingInjector::GlmThinking.inject(&mut body, &t, "glm-4.7");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["clear_thinking"], false);
    }

    #[test]
    fn shared_adapter_endpoint_and_headers() {
        let a = OpenAiCompatibleAdapter::new(
            "openai",
            "/v1/chat/completions",
            ThinkingInjector::OpenAiReasoningEffort,
            true,
        );
        assert_eq!(a.name(), "openai");
        assert_eq!(a.chat_endpoint("gpt-4"), "/v1/chat/completions");
        assert!(a.supports_model_listing());

        let key = ApiKey::try_from_string("sk-test-key".to_string()).unwrap();
        let headers = a.get_headers(&key);
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer sk-test-key");
        assert_eq!(headers[1].0, "content-type");
        assert_eq!(headers[1].1, "application/json");
    }

    #[test]
    fn zai_endpoint_omits_v1_prefix() {
        let a = OpenAiCompatibleAdapter::new(
            "zai",
            "/chat/completions",
            ThinkingInjector::GlmThinking,
            false,
        );
        assert_eq!(a.chat_endpoint("glm-4"), "/chat/completions");
        assert!(!a.supports_model_listing());
    }
}
