use openclaudia::{config, guardrails, proxy};
use tracing::{error, info};

/// Run in iteration/loop mode with Stop hooks
pub async fn cmd_loop(
    max_iterations: u32,
    port: Option<u16>,
    target: Option<String>,
) -> anyhow::Result<()> {
    if !config::config_file_exists() {
        error!("No configuration found. Run 'openclaudia init' first.");
        anyhow::bail!("no configuration found; run `openclaudia init` first");
    }

    let mut config = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to parse configuration: {}", e);
            eprintln!("Check your .openclaudia/config.yaml for syntax errors.");
            anyhow::bail!("invalid configuration: {e}");
        }
    };

    if let Some(p) = port {
        config.proxy.port = p;
    }
    if let Some(t) = target {
        config.proxy.target = t;
    }

    guardrails::configure(&config.guardrails);

    let Some(provider) = config.active_provider() else {
        error!(
            "No provider configured for target '{}'",
            config.proxy.target
        );
        anyhow::bail!(
            "no provider configured for target '{}'",
            config.proxy.target
        );
    };

    if provider.api_key.is_none() && !super::can_start_without_api_key(&config.proxy.target) {
        let env_var = super::provider_api_key_env_var(&config.proxy.target);
        error!(
            "No API key configured for provider '{}'. Set {} environment variable.",
            config.proxy.target, env_var
        );
        anyhow::bail!(
            "no API key configured for provider '{}'; set {} environment variable",
            config.proxy.target,
            env_var
        );
    }

    info!(
        "OpenClaudia v{} starting in loop mode...",
        env!("CARGO_PKG_VERSION")
    );
    info!(
        "Max iterations: {}",
        if max_iterations == 0 {
            "unlimited".to_string()
        } else {
            max_iterations.to_string()
        }
    );
    info!(
        "Proxy: http://{}:{} -> {}",
        config.proxy.host, config.proxy.port, config.proxy.target
    );

    proxy::start_loop_server(config, max_iterations).await
}
