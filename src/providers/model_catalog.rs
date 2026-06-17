//! Static provider model catalog used when a provider cannot list models.
//!
//! These lists are fallback affordances for `/model list` and the proxy
//! `/v1/models` endpoint. They are not allowlists: callers may still pass any
//! upstream model ID supported by the configured provider endpoint.

pub const STATIC_MODEL_CATALOG_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "deepseek",
    "qwen",
    "zai",
    "kimi",
    "minimax",
];

pub const ANTHROPIC_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-mythos-5",
    "claude-mythos-preview",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    "claude-haiku-4-5",
    "claude-sonnet-4-5-20250929",
    "claude-sonnet-4-5",
    "claude-opus-4-5-20251101",
    "claude-opus-4-5",
    "claude-opus-4-1-20250805",
    "claude-sonnet-4-20250514",
    "claude-opus-4-20250514",
];

pub const OPENAI_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.5-2026-04-23",
    "gpt-5.4",
    "gpt-5.4-pro",
    "gpt-5.4-mini",
    "gpt-5.4-mini-2026-03-17",
    "gpt-5.4-nano",
    "gpt-5.3-codex",
    "gpt-5.3-chat-latest",
    "gpt-5.2",
    "gpt-5.2-pro",
    "gpt-5.2-codex",
    "gpt-5.2-chat-latest",
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
    "gpt-5.1-chat-latest",
    "gpt-5",
    "gpt-5-pro",
    "gpt-5-codex",
    "gpt-5-chat-latest",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "o3-pro",
    "o3",
    "o3-mini",
    "o4-mini",
    "o1-pro",
    "o1",
    "o1-mini",
    "o1-preview",
    "chat-latest",
    "gpt-4o-search-preview",
    "gpt-4o-mini",
    "gpt-4o-mini-search-preview",
    "gpt-4o",
    "gpt-4.5-preview",
    "gpt-4-turbo",
    "gpt-4-turbo-preview",
    "gpt-4",
    "gpt-3.5-turbo",
    "codex-mini-latest",
];

pub const GOOGLE_MODELS: &[&str] = &[
    "gemini-3.5-flash",
    "gemini-3.1-pro-preview",
    "gemini-3.1-flash-lite",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
];

pub const ZAI_MODELS: &[&str] = &[
    "glm-5.2",
    "glm-5.1",
    "glm-5.1-highspeed",
    "glm-5",
    "glm-5-turbo",
    "glm-4.7",
    "glm-4.7-flashx",
    "glm-4.7-flash",
    "glm-4.6",
    "glm-4.5",
    "glm-4.5-air",
    "glm-4.5-x",
    "glm-4.5-airx",
    "glm-4.5-flash",
    "glm-4-32b-0414-128k",
];

pub const DEEPSEEK_MODELS: &[&str] = &[
    "deepseek-v4-pro",
    "deepseek-v4-flash",
    "deepseek-chat",
    "deepseek-reasoner",
];

pub const QWEN_MODELS: &[&str] = &[
    "qwen3.7-plus",
    "qwen3.7-plus-2026-05-26",
    "qwen3.7-max",
    "qwen3.7-max-2026-06-08",
    "qwen3.7-max-preview",
    "qwen3.6-plus",
    "qwen3.6-flash",
    "qwen3.6-35b-a3b",
    "qwen3.5-plus",
    "qwen3.5-flash",
    "qwen3-max",
    "qwen-plus",
    "qwen-turbo",
    "qwq-plus",
    "qwen3-coder-plus",
];

pub const KIMI_MODELS: &[&str] = &[
    "kimi-k2.7-code",
    "kimi-k2.7-code-highspeed",
    "kimi-k2.6",
    "kimi-k2.5",
    "moonshot-v1-128k",
    "moonshot-v1-32k",
    "moonshot-v1-8k",
    "moonshot-v1-128k-vision-preview",
    "moonshot-v1-32k-vision-preview",
    "moonshot-v1-8k-vision-preview",
];

pub const MINIMAX_MODELS: &[&str] = &[
    "MiniMax-M3",
    "MiniMax-M2.7",
    "MiniMax-M2.7-highspeed",
    "MiniMax-M2.5",
    "MiniMax-M2.5-highspeed",
    "MiniMax-M2.1",
    "MiniMax-M2.1-highspeed",
    "MiniMax-M2",
    "M2-her",
];

pub const FALLBACK_MODELS: &[&str] = &[super::DEFAULT_MODEL_FALLBACK];

#[must_use]
pub fn canonical_static_catalog_provider(provider: &str) -> &str {
    match provider {
        "gemini" => "google",
        "glm" | "zhipu" => "zai",
        "alibaba" => "qwen",
        "moonshot" => "kimi",
        other => other,
    }
}

#[must_use]
pub fn static_models_for_provider(provider: &str) -> &'static [&'static str] {
    match canonical_static_catalog_provider(provider) {
        "anthropic" => ANTHROPIC_MODELS,
        "openai" => OPENAI_MODELS,
        "google" => GOOGLE_MODELS,
        "zai" => ZAI_MODELS,
        "deepseek" => DEEPSEEK_MODELS,
        "qwen" => QWEN_MODELS,
        "kimi" => KIMI_MODELS,
        "minimax" => MINIMAX_MODELS,
        _ => FALLBACK_MODELS,
    }
}

#[cfg(test)]
mod tests {
    use super::static_models_for_provider;

    #[test]
    fn anthropic_catalog_includes_claude_opus_4_7() {
        assert!(
            static_models_for_provider("anthropic").contains(&"claude-opus-4-7"),
            "Anthropic static catalog must include claude-opus-4-7"
        );
    }

    #[test]
    fn zai_catalog_includes_glm_5_1_highspeed() {
        assert!(
            static_models_for_provider("zai").contains(&"glm-5.1-highspeed"),
            "Z.AI static catalog must include glm-5.1-highspeed"
        );
    }
}
