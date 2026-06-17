use openclaudia::{
    config,
    mcp::McpManager,
    plugins::PluginManager,
    providers::{get_adapter, ProviderAdapter, ProviderError},
    rules::RulesEngine,
    session::SessionManager,
};
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

const DOCTOR_ADAPTER_PROVIDER: &str = "anthropic";

fn lookup_doctor_adapter(
    provider_name: &str,
) -> Result<&'static dyn ProviderAdapter, ProviderError> {
    get_adapter(provider_name)
}

#[allow(clippy::too_many_lines)]
/// Check configuration and connectivity
pub async fn cmd_doctor() -> anyhow::Result<()> {
    println!("OpenClaudia Doctor\n");

    let mut has_failures = false;

    // Check configuration
    print!("Configuration... ");
    let loaded_config = if config::config_file_exists() {
        match config::load_config() {
            Ok(config) => {
                println!("OK");

                for (name, provider) in &config.providers {
                    print!("  {name} API key... ");
                    if provider.api_key.is_some() {
                        println!("configured");
                    } else {
                        println!("NOT SET");
                    }
                    if let Some(model) = &provider.model {
                        println!("    Default model: {model}");
                    }
                }

                Some(config)
            }
            Err(e) => {
                println!("FAILED: {e}");
                println!(
                    "\nConfig file exists but has errors. Check your .openclaudia/config.yaml for syntax errors."
                );
                has_failures = true;
                None
            }
        }
    } else {
        println!("MISSING (No configuration found)");
        println!("\nRun 'openclaudia init' to create a configuration file.");
        has_failures = true;
        None
    };

    if let Some(config) = &loaded_config {
        print!("\nConnectivity to {}... ", config.proxy.target);
        if let Some(provider) = config.active_provider() {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()?;
            match client.get(&provider.base_url).send().await {
                Ok(_) => println!("OK"),
                Err(e) => {
                    println!("FAILED: {e}");
                    has_failures = true;
                }
            }
        } else {
            println!("FAILED (no provider configured)");
            has_failures = true;
        }
    }

    // Check for hooks directory
    print!("\nHooks directory... ");
    if PathBuf::from(".openclaudia/hooks").exists() {
        println!("OK");
    } else {
        println!("NOT FOUND");
    }

    // Check for rules directory and load rules
    print!("Rules directory... ");
    if PathBuf::from(".openclaudia/rules").exists() {
        println!("OK");
        let rules_engine = RulesEngine::new(".openclaudia/rules");
        let all_rules = rules_engine.all_rules();
        if !all_rules.is_empty() {
            println!("  Loaded rules: {}", all_rules.len());
            for rule in all_rules {
                println!("    - {} (languages: {:?})", rule.name, rule.languages);
            }
        }
        let test_files = ["src/main.rs", "test.py"];
        let matched = rules_engine.get_rules_for_files(&test_files);
        println!("  Rules for test files: {} matched", matched.len());
    } else {
        println!("NOT FOUND");
    }

    // Check plugins
    print!("\nPlugins... ");
    // crosslink #893: try_new surfaces missing-$HOME loudly in `doctor`
    // since that is the exact UX a confused user is checking.
    let mut plugin_manager = match PluginManager::try_new() {
        Ok(pm) => pm,
        Err(e) => {
            println!("WARN ({e}); using project-only search");
            PluginManager::new()
        }
    };
    let errors = plugin_manager.discover();
    if plugin_manager.count() > 0 {
        println!("OK ({} loaded)", plugin_manager.count());
        for plugin in plugin_manager.all() {
            let root = plugin.root();
            println!(
                "  - {} v{} ({})",
                plugin.name(),
                plugin.manifest.version.as_deref().unwrap_or("0.0.0"),
                root.display()
            );

            let env_vars = plugin.env_vars();
            if !env_vars.is_empty() {
                println!("    Environment: {} vars", env_vars.len());
            }

            let resolved_cmds = plugin.resolved_commands();
            if !resolved_cmds.is_empty() {
                println!("    Commands: {}", resolved_cmds.len());
                for cmd in &resolved_cmds {
                    let desc = cmd.description.as_deref().unwrap_or("(no description)");
                    let extras = [
                        cmd.argument_hint.as_ref().map(|h| format!("args: {h}")),
                        cmd.model.as_ref().map(|m| format!("model: {m}")),
                        cmd.allowed_tools
                            .as_ref()
                            .map(|t| format!("tools: {}", t.len())),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();

                    if extras.is_empty() {
                        println!("      /{} - {}", cmd.name, desc);
                    } else {
                        println!("      /{} - {} [{}]", cmd.name, desc, extras.join(", "));
                    }
                }
            }

            if !plugin.mcp_configs.is_empty() {
                println!("    MCP servers: {}", plugin.mcp_configs.len());
            }
        }

        let all_mcp = plugin_manager.all_mcp_servers();
        if !all_mcp.is_empty() {
            println!("\n  MCP Servers from plugins:");
            for (plugin, server) in all_mcp {
                println!(
                    "    - {} from {} ({})",
                    server.name,
                    plugin.name(),
                    server.transport
                );
            }
        }

        for plugin in plugin_manager.all() {
            let resolved = plugin.resolve_path("scripts/init.sh");
            info!(
                "Plugin {} script path: {}",
                plugin.name(),
                resolved.display()
            );
        }
    } else if !errors.is_empty() {
        println!("ERRORS");
        for err in errors {
            println!("  Error: {err}");
        }
        has_failures = true;
    } else {
        println!("none found");
    }

    // Test MCP manager functionality
    print!("\nMCP Manager... ");
    let mcp_manager = McpManager::new();

    let is_connected = mcp_manager.is_connected("test-server").await;
    println!(
        "{}",
        if is_connected {
            "connected"
        } else {
            "no servers"
        }
    );

    if let Some((name, supports_list_changed)) = mcp_manager.get_server_info("test-server").await {
        println!("  Server: {name} (list_changed: {supports_list_changed})");
    }

    // Check session state
    print!("\nSession... ");
    let session_dir = PathBuf::from(".openclaudia/session");
    if session_dir.exists() {
        let session_manager = SessionManager::new(&session_dir);
        match session_manager.get_handoff_context() {
            Ok(Some(handoff)) => println!("found handoff context ({} bytes)", handoff.len()),
            Ok(None) => {}
            Err(err) => {
                println!("handoff unreadable: {err}");
                has_failures = true;
            }
        }

        let sessions = session_manager.list_sessions();
        if sessions.is_empty() {
            println!("  No previous sessions");
        } else {
            println!("  Previous sessions: {}", sessions.len());
            for session in sessions.iter().take(3) {
                println!(
                    "    - {} ({:?}, {} requests)",
                    session.id, session.mode, session.request_count
                );
            }
            if sessions.len() > 10 {
                println!("  Note: Consider running cleanup (>10 sessions stored)");
            }
        }
    } else {
        println!("  No previous sessions");
    }

    // Test rules reload and rules_dir
    print!("\nRules engine... ");
    let mut rules_engine = RulesEngine::new(".openclaudia/rules");
    let rules_path = rules_engine.rules_dir().to_path_buf();
    println!("path: {}", rules_path.display());
    rules_engine.reload();
    info!("Rules reloaded from {}", rules_path.display());

    // Test provider adapters and error variants
    print!("\nProvider adapters... ");
    match lookup_doctor_adapter(DOCTOR_ADAPTER_PROVIDER) {
        Ok(adapter) => {
            println!("{} adapter OK", adapter.name());

            let test_response = serde_json::json!({
                "id": "test",
                "content": [{"type": "text", "text": "test"}],
                "model": "test-model",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            });
            match adapter.transform_response(test_response, false) {
                Ok(transformed) => info!("Response transformed: {}", transformed["object"]),
                Err(e) => info!("Transform error (expected): {}", e),
            }
        }
        Err(e) => {
            println!("FAILED: {e}");
            info!("Provider adapter lookup failed: {}", e);
            has_failures = true;
        }
    }

    let custom_paths = vec![PathBuf::from(".openclaudia/plugins")];
    let mut custom_plugin_manager = PluginManager::with_paths(custom_paths);
    let _ = custom_plugin_manager.discover();
    info!(
        "Custom plugin manager: {} plugins",
        custom_plugin_manager.count()
    );

    if let Some(plugin) = custom_plugin_manager.get("test-plugin") {
        info!("Found plugin: {}", plugin.name());
    }

    let all_hooks = custom_plugin_manager.all_hooks();
    info!("All hooks: {}", all_hooks.len());

    let session_hooks = custom_plugin_manager.hooks_for_event("session_start");
    info!("Session start hooks: {}", session_hooks.len());

    let reload_errors = custom_plugin_manager.reload();
    info!("Plugin reload: {} errors", reload_errors.len());

    println!("\nDoctor check complete.");
    if has_failures {
        anyhow::bail!("doctor found one or more failures");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_doctor_adapter_resolves_builtin_provider() {
        let adapter = lookup_doctor_adapter(DOCTOR_ADAPTER_PROVIDER).expect("doctor provider");
        assert_eq!(adapter.name(), "anthropic");
    }

    #[test]
    fn lookup_doctor_adapter_returns_provider_errors() {
        match lookup_doctor_adapter("missing-provider") {
            Ok(adapter) => panic!("unexpected adapter {}", adapter.name()),
            Err(ProviderError::UnknownProvider { name, supported }) => {
                assert_eq!(name, "missing-provider");
                assert!(supported.contains(&"anthropic"));
            }
            Err(err) => panic!("unexpected provider error: {err}"),
        }
    }
}
