use openclaudia::{config, providers};

const ANTHROPIC_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-mythos-5",
    "claude-mythos-preview",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    "claude-sonnet-4-5-20250929",
    "claude-opus-4-5-20251101",
    "claude-opus-4-1-20250805",
    "claude-sonnet-4-20250514",
    "claude-opus-4-20250514",
];

const OPENAI_MODELS: &[&str] = &[
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.5-2026-04-23",
    "gpt-5.4",
    "gpt-5.4-pro",
    "gpt-5.4-mini",
    "gpt-5.4-mini-2026-03-17",
    "gpt-5.4-nano",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-pro",
    "gpt-5.2-codex",
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
    "gpt-5",
    "gpt-5-pro",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-4.1",
    "gpt-4.1-mini",
    "o3-pro",
    "o3",
    "o4-mini",
    "gpt-4o-mini",
];

const GOOGLE_MODELS: &[&str] = &[
    "gemini-3.5-flash",
    "gemini-3.1-pro-preview",
    "gemini-3.1-flash-lite",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
];

const ZAI_MODELS: &[&str] = &[
    "glm-5.2",
    "glm-5.1",
    "glm-5",
    "glm-5-turbo",
    "glm-4.7",
    "glm-4.7-flashx",
    "glm-4.7-flash",
    "glm-4.6",
    "glm-4.5-air",
    "glm-4.5-airx",
    "glm-4.5-flash",
];

const DEEPSEEK_MODELS: &[&str] = &[
    "deepseek-v4-pro",
    "deepseek-v4-flash",
    "deepseek-chat",
    "deepseek-reasoner",
];

const QWEN_MODELS: &[&str] = &[
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

const KIMI_MODELS: &[&str] = &[
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

const MINIMAX_MODELS: &[&str] = &[
    "MiniMax-M3",
    "MiniMax-M2.7",
    "MiniMax-M2.7-highspeed",
    "MiniMax-M2.5",
    "MiniMax-M2.5-highspeed",
    "MiniMax-M2.1",
    "MiniMax-M2.1-highspeed",
    "MiniMax-M2",
];

/// Get static list of models for a provider (fallback when API unavailable)
pub fn get_available_models(provider: &str) -> Vec<&'static str> {
    let provider = match provider {
        "gemini" => "google",
        "glm" | "zhipu" => "zai",
        "alibaba" => "qwen",
        "moonshot" => "kimi",
        other => other,
    };

    match provider {
        "anthropic" => ANTHROPIC_MODELS.to_vec(),
        "openai" => OPENAI_MODELS.to_vec(),
        "google" => GOOGLE_MODELS.to_vec(),
        "zai" => ZAI_MODELS.to_vec(),
        "deepseek" => DEEPSEEK_MODELS.to_vec(),
        "qwen" => QWEN_MODELS.to_vec(),
        "kimi" => KIMI_MODELS.to_vec(),
        "minimax" => MINIMAX_MODELS.to_vec(),
        _ => vec![providers::DEFAULT_MODEL_FALLBACK],
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
        provider_config.api_key.as_ref(),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::get_available_models;

    fn documented_models_for_heading(readme: &str, heading: &str) -> Vec<String> {
        let supported_models = readme
            .split_once("## Supported Models")
            .expect("README must document supported models")
            .1
            .split_once("## Behavioral Modes")
            .expect("supported models section must end before behavioral modes")
            .0;
        let heading_marker = format!("### {heading}");
        let section = supported_models
            .split_once(&heading_marker)
            .unwrap_or_else(|| panic!("README missing supported-model heading {heading:?}"))
            .1
            .split("### ")
            .next()
            .expect("provider model section");

        section
            .lines()
            .flat_map(|line| {
                let mut models = Vec::new();
                let mut rest = line;
                while let Some((_, after_open)) = rest.split_once('`') {
                    let Some((model, after_close)) = after_open.split_once('`') else {
                        break;
                    };
                    if !model.is_empty() {
                        models.push(model.to_string());
                    }
                    rest = after_close;
                }
                models
            })
            .collect()
    }

    #[test]
    fn readme_supported_models_match_static_repl_model_lists() {
        let readme = include_str!("../../../README.md");

        for (heading, provider) in [
            ("Anthropic", "anthropic"),
            ("OpenAI", "openai"),
            ("Google Gemini", "google"),
            ("DeepSeek", "deepseek"),
            ("Qwen", "qwen"),
            ("Z.AI (GLM)", "zai"),
            ("Kimi", "kimi"),
            ("MiniMax", "minimax"),
        ] {
            let documented = documented_models_for_heading(readme, heading);
            let static_models: Vec<String> = get_available_models(provider)
                .into_iter()
                .map(str::to_string)
                .collect();
            assert_eq!(
                documented, static_models,
                "README supported models for {heading} must match get_available_models({provider:?})"
            );
        }
    }

    #[test]
    fn static_model_lists_do_not_contain_duplicates() {
        for provider in [
            "anthropic",
            "openai",
            "google",
            "deepseek",
            "qwen",
            "zai",
            "kimi",
            "minimax",
        ] {
            let models = get_available_models(provider);
            let unique: BTreeSet<_> = models.iter().copied().collect();
            assert_eq!(
                models.len(),
                unique.len(),
                "static model list for {provider} must not contain duplicates"
            );
        }
    }

    #[test]
    fn provider_aliases_return_canonical_static_model_lists() {
        for (alias, canonical) in [
            ("gemini", "google"),
            ("glm", "zai"),
            ("zhipu", "zai"),
            ("alibaba", "qwen"),
            ("moonshot", "kimi"),
        ] {
            assert_eq!(
                get_available_models(alias),
                get_available_models(canonical),
                "static model list for alias {alias:?} must match canonical provider {canonical:?}"
            );
        }
    }
}
