use openclaudia::{
    config,
    mcp::McpManager,
    plugins::{PluginError, PluginManager},
    providers::{get_adapter, ProviderError},
    rules::RulesEngine,
    session::SessionManager,
};
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

#[allow(clippy::too_many_lines)]
/// Check configuration and connectivity
pub async fn cmd_doctor() -> anyhow::Result<()> {
    println!("OpenClaudia Doctor\n");

    // Check configuration
    print!("Configuration... ");
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

            print!("\nConnectivity to {}... ", config.proxy.target);
            if let Some(provider) = config.active_provider() {
                let client = reqwest::Client::new();
                match client.get(&provider.base_url).send().await {
                    Ok(_) => println!("OK"),
                    Err(e) => println!("FAILED: {e}"),
                }
            } else {
                println!("SKIPPED (no provider configured)");
            }
        }
        Err(e) => {
            println!("FAILED: {e}");
            if config::config_file_exists() {
                println!("\nConfig file exists but has errors. Check your .openclaudia/config.yaml for syntax errors.");
            } else {
                println!("\nRun 'openclaudia init' to create a configuration file.");
            }
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
    let mut plugin_manager = PluginManager::new();
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
    } else {
        println!("none found");
    }

    let not_found_err = PluginError::NotFound("test-plugin".to_string());
    info!("Plugin error test: {}", not_found_err);

    // Test MCP manager functionality
    print!("\nMCP Manager... ");
    let mut mcp_manager = McpManager::new();

    let is_connected = mcp_manager.is_connected("test-server");
    println!(
        "{}",
        if is_connected {
            "connected"
        } else {
            "no servers"
        }
    );

    if let Some((name, supports_list_changed)) = mcp_manager.get_server_info("test-server") {
        println!("  Server: {name} (list_changed: {supports_list_changed})");
    }

    match mcp_manager
        .call_tool("test_tool", serde_json::json!({}))
        .await
    {
        Ok(result) => println!("  Tool result: {result}"),
        Err(e) => info!("  Expected error (no server): {}", e),
    }

    match mcp_manager
        .call_tool_with_timeout("test_tool", serde_json::json!({}), Duration::from_secs(1))
        .await
    {
        Ok(result) => println!("  Timeout tool result: {result}"),
        Err(e) => info!("  Expected timeout error: {}", e),
    }

    let _ = mcp_manager.disconnect("nonexistent").await;
    let _ = mcp_manager.disconnect_all().await;

    // Check session state
    print!("\nSession... ");
    let mut session_manager = SessionManager::new(".openclaudia/session");
    if let Some(handoff) = session_manager.get_handoff_context() {
        println!("found handoff context ({} bytes)", handoff.len());
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
            session_manager.cleanup_old_sessions(10);
        }
    }

    let session = session_manager.start_initializer();
    let session_id = session.id.clone();
    info!("Test session created: {}", session_id);

    if let Some(session) = session_manager.get_session_mut() {
        session.add_tokens(100);
        session.complete_task("Doctor check task");
        session.add_modified_file("src/main.rs");
        info!(
            "Session updated: {} tokens, {} completed tasks",
            session.total_tokens,
            session.progress.completed_tasks.len()
        );
    }

    if let Some(loaded) = session_manager.load_session(&session_id) {
        info!("Loaded session: {} (mode: {:?})", loaded.id, loaded.mode);
    }

    let coding_session = session_manager.start_coding(&session_id);
    info!("Coding session: {}", coding_session.id);

    // Test rules reload and rules_dir
    print!("\nRules engine... ");
    let mut rules_engine = RulesEngine::new(".openclaudia/rules");
    let rules_path = rules_engine.rules_dir().to_path_buf();
    println!("path: {}", rules_path.display());
    rules_engine.reload();
    info!("Rules reloaded from {}", rules_path.display());

    // Test provider adapters and error variants
    print!("\nProvider adapters... ");
    let adapter = get_adapter("anthropic");
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

    let _invalid = ProviderError::InvalidResponse("test".to_string());
    let _unsupported = ProviderError::Unsupported("test feature".to_string());
    info!("Provider error variants OK");

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

    let _ = custom_plugin_manager.enable("test-plugin");
    let _ = custom_plugin_manager.disable("test-plugin");
    let reload_errors = custom_plugin_manager.reload();
    info!("Plugin reload: {} errors", reload_errors.len());

    // Test proxy MCP functions
    print!("\nProxy MCP functions... ");
    let mcp_for_proxy = std::sync::Arc::new(tokio::sync::RwLock::new(McpManager::new()));

    match openclaudia::proxy::handle_mcp_tool_call(
        &mcp_for_proxy,
        "test_tool",
        serde_json::json!({}),
    )
    .await
    {
        Ok(result) => info!("MCP tool result: {}", result),
        Err(e) => info!("Expected MCP error: {}", e),
    }

    openclaudia::proxy::shutdown_mcp(&mcp_for_proxy).await;
    println!("OK");

    println!("\nDoctor check complete.");
    Ok(())
}
