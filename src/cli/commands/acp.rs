use openclaudia::config;

/// ACP server mode -- stdin/stdout JSON-RPC for acpx interoperability
pub async fn cmd_acp(
    target_override: Option<String>,
    model_override: Option<String>,
) -> anyhow::Result<()> {
    let config = match config::load_config() {
        Ok(mut c) => {
            if let Some(ref target) = target_override {
                c.proxy.target = target.clone();
            }
            c
        }
        Err(e) => {
            if config::config_file_exists() {
                eprintln!("Failed to parse configuration: {}", e);
                eprintln!("Check your .openclaudia/config.yaml for syntax errors.");
            } else {
                eprintln!("No configuration found. Run 'openclaudia init' first.");
            }
            return Ok(());
        }
    };

    let provider = match config.active_provider() {
        Some(p) => p,
        None => {
            eprintln!(
                "No provider configured for target '{}'",
                config.proxy.target
            );
            return Ok(());
        }
    };

    let api_key = if let Some(k) = &provider.api_key {
        k.clone()
    } else {
        let env_var = match config.proxy.target.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            "google" => "GOOGLE_API_KEY",
            "zai" => "ZAI_API_KEY",
            "deepseek" => "DEEPSEEK_API_KEY",
            "qwen" => "QWEN_API_KEY",
            _ => "API_KEY",
        };
        eprintln!(
            "No API key configured for '{}'. Set {} or add to config.",
            config.proxy.target, env_var
        );
        return Ok(());
    };

    let model = model_override
        .or_else(|| provider.model.clone())
        .unwrap_or_else(|| match config.proxy.target.as_str() {
            "anthropic" => "claude-sonnet-4-6".to_string(),
            "openai" => "gpt-5.2".to_string(),
            "google" => "gemini-2.5-flash".to_string(),
            "zai" => "glm-5".to_string(),
            "deepseek" => "deepseek-chat".to_string(),
            "qwen" => "qwen3.5-plus".to_string(),
            _ => "gpt-5.2".to_string(),
        });

    openclaudia::acp::run_acp_server(config, model, api_key).await
}
