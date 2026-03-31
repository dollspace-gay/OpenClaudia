use openclaudia::{config, providers};

/// Get static list of models for a provider (fallback when API unavailable)
pub fn get_available_models(provider: &str) -> Vec<&'static str> {
    match provider {
        "anthropic" => vec![
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-5-20251101",
            "claude-opus-4-1-20250805",
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
        ],
        "openai" => vec![
            "gpt-5.2",
            "gpt-5.2-codex",
            "gpt-5",
            "gpt-5-mini",
            "gpt-5-nano",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.1-nano",
            "o3",
            "o4-mini",
            "gpt-4o",
            "gpt-4o-mini",
        ],
        "google" => vec![
            "gemini-3.1-pro-preview",
            "gemini-3-flash-preview",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ],
        "zai" => vec![
            "glm-5",
            "glm-4.7",
            "glm-4.7-flash",
            "glm-4.6",
            "glm-4.5-flash",
        ],
        "deepseek" => vec!["deepseek-chat", "deepseek-reasoner"],
        "qwen" => vec![
            "qwen3.5-plus",
            "qwen3-max",
            "qwen-plus",
            "qwen-turbo",
            "qwq-plus",
            "qwen3-coder-plus",
        ],
        _ => vec!["gpt-5.2"],
    }
}

/// Fetch models dynamically from provider API (for OpenAI-compatible providers like LM Studio)
pub async fn fetch_dynamic_models(
    provider_config: &config::ProviderConfig,
    adapter: &dyn providers::ProviderAdapter,
) -> Option<Vec<String>> {
    if !adapter.supports_model_listing() {
        return None;
    }

    match providers::fetch_models(
        &provider_config.base_url,
        provider_config.api_key.as_deref(),
        adapter,
    )
    .await
    {
        Ok(models) => {
            let model_ids: Vec<String> = models.into_iter().map(|m| m.id).collect();
            if model_ids.is_empty() {
                None
            } else {
                Some(model_ids)
            }
        }
        Err(e) => {
            tracing::debug!("Failed to fetch models from API: {}", e);
            None
        }
    }
}
