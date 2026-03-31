use openclaudia::config;
use tracing::{error, info};

/// Show current configuration
pub fn cmd_config() -> anyhow::Result<()> {
    match config::load_config() {
        Ok(config) => {
            println!("OpenClaudia Configuration\n");
            println!("Proxy:");
            println!("  Host: {}", config.proxy.host);
            println!("  Port: {}", config.proxy.port);
            println!("  Target: {}", config.proxy.target);
            println!();
            println!("Providers:");
            for (name, provider) in &config.providers {
                let has_key = provider.api_key.is_some();
                println!(
                    "  {}: {} (API key: {})",
                    name,
                    provider.base_url,
                    if has_key { "configured" } else { "not set" }
                );
            }
            println!();
            println!("Session:");
            println!("  Timeout: {} minutes", config.session.timeout_minutes);
            println!("  Persist path: {:?}", config.session.persist_path);
        }
        Err(e) => {
            if config::config_file_exists() {
                error!("Failed to parse configuration: {}", e);
                info!("Check your .openclaudia/config.yaml for syntax errors.");
            } else {
                error!("No configuration found.");
                info!("Run 'openclaudia init' to create a configuration file.");
            }
        }
    }
    Ok(())
}
