use openclaudia::{config, guardrails, proxy};
use tracing::{error, info};

/// Start the proxy server
pub async fn cmd_start(
    port: Option<u16>,
    host: Option<String>,
    target: Option<String>,
) -> anyhow::Result<()> {
    let mut config = config::load_config()?;

    if let Some(p) = port {
        config.proxy.port = p;
    }
    if let Some(h) = host {
        config.proxy.host = h;
    }
    if let Some(t) = target {
        config.proxy.target = t;
    }

    guardrails::configure(&config.guardrails);

    if let Some(provider) = config.active_provider() {
        if provider.api_key.is_none() {
            let env_var = match config.proxy.target.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "google" => "GOOGLE_API_KEY",
                "zai" => "ZAI_API_KEY",
                "deepseek" => "DEEPSEEK_API_KEY",
                "qwen" => "QWEN_API_KEY",
                _ => "API_KEY",
            };
            if config.proxy.target == "anthropic" {
                tracing::warn!(
                    "No API key configured for '{}'. OAuth authentication is available.",
                    config.proxy.target
                );
                info!(
                    "Visit http://localhost:{}/auth/device to authenticate with Claude Max",
                    config.proxy.port
                );
            } else {
                error!(
                    "No API key configured for provider '{}'. Set {} environment variable.",
                    config.proxy.target, env_var
                );
                return Ok(());
            }
        }
    }

    info!("OpenClaudia v{} starting...", env!("CARGO_PKG_VERSION"));
    info!(
        "Proxy: http://{}:{} -> {}",
        config.proxy.host, config.proxy.port, config.proxy.target
    );
    info!(
        "Point your AI client at: http://localhost:{}",
        config.proxy.port
    );

    proxy::start_server(config).await
}
