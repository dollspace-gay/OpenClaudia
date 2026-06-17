use openclaudia::config;

/// ACP server mode -- stdin/stdout JSON-RPC for acpx interoperability
pub async fn cmd_acp(
    target_override: Option<String>,
    model_override: Option<String>,
) -> anyhow::Result<()> {
    if !config::config_file_exists() {
        eprintln!("No configuration found. Run 'openclaudia init' first.");
        anyhow::bail!("no configuration found; run `openclaudia init` first");
    }

    let config = match config::load_config() {
        Ok(mut c) => {
            if let Some(ref target) = target_override {
                c.proxy.target.clone_from(target);
            }
            c
        }
        Err(e) => {
            eprintln!("Failed to parse configuration: {e}");
            eprintln!("Check your .openclaudia/config.yaml for syntax errors.");
            anyhow::bail!("invalid configuration: {e}");
        }
    };

    let Some(provider) = config.active_provider() else {
        eprintln!(
            "No provider configured for target '{}'",
            config.proxy.target
        );
        anyhow::bail!(
            "no provider configured for target '{}'",
            config.proxy.target
        );
    };

    let api_key = if let Some(k) = &provider.api_key {
        Some(k.clone())
    } else if config::is_local_provider_name(&config.proxy.target) {
        None
    } else {
        let env_var = super::provider_api_key_env_var(&config.proxy.target);
        eprintln!(
            "No API key configured for '{}'. Set {} or add to config.",
            config.proxy.target, env_var
        );
        anyhow::bail!(
            "no API key configured for '{}'; set {} or add to config",
            config.proxy.target,
            env_var
        );
    };

    let model = model_override
        .or_else(|| provider.model.clone())
        .unwrap_or_else(|| {
            openclaudia::providers::default_model_for_target(&config.proxy.target).to_string()
        });

    openclaudia::acp::run_acp_server(config, model, api_key).await
}
