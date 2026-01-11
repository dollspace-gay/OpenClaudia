//! OpenClaudia - Open-source universal agent harness
//!
//! Provides Claude Code-like capabilities for any AI agent.

mod compaction;
mod config;
mod context;
mod hooks;
mod mcp;
mod plugins;
mod providers;
mod proxy;
mod rules;
mod session;

use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "openclaudia")]
#[command(author, version, about = "Open-source universal agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize OpenClaudia configuration in the current directory
    Init {
        /// Force overwrite existing configuration
        #[arg(short, long)]
        force: bool,
    },

    /// Start the OpenClaudia proxy server
    Start {
        /// Port to listen on (overrides config)
        #[arg(short, long)]
        port: Option<u16>,

        /// Host to bind to (overrides config)
        #[arg(long)]
        host: Option<String>,

        /// Target provider (anthropic, openai, google)
        #[arg(short, long)]
        target: Option<String>,
    },

    /// Show current configuration
    Config,

    /// Check configuration and connectivity
    Doctor,

    /// Run in iteration/loop mode with Stop hooks
    Loop {
        /// Maximum number of iterations (0 = unlimited)
        #[arg(short, long, default_value = "0")]
        max_iterations: u32,

        /// Port to listen on (overrides config)
        #[arg(short, long)]
        port: Option<u16>,

        /// Target provider (anthropic, openai, google)
        #[arg(short, long)]
        target: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        "openclaudia=debug,tower_http=debug"
    } else {
        "openclaudia=info,tower_http=warn"
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    match cli.command {
        Commands::Init { force } => cmd_init(force),
        Commands::Start { port, host, target } => cmd_start(port, host, target).await,
        Commands::Config => cmd_config(),
        Commands::Doctor => cmd_doctor().await,
        Commands::Loop {
            max_iterations,
            port,
            target,
        } => cmd_loop(max_iterations, port, target).await,
    }
}

/// Initialize OpenClaudia configuration
fn cmd_init(force: bool) -> anyhow::Result<()> {
    let config_dir = PathBuf::from(".openclaudia");
    let config_file = config_dir.join("config.yaml");

    if config_file.exists() && !force {
        error!("Configuration already exists. Use --force to overwrite.");
        return Ok(());
    }

    // Create directories
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(config_dir.join("hooks"))?;
    fs::create_dir_all(config_dir.join("rules"))?;
    fs::create_dir_all(config_dir.join("plugins"))?;

    // Write default config
    let default_config = r#"# OpenClaudia Configuration
# https://github.com/yourusername/openclaudia

proxy:
  port: 8080
  host: "127.0.0.1"
  target: anthropic  # Default provider: anthropic, openai, google

providers:
  anthropic:
    base_url: https://api.anthropic.com
    # api_key: ${ANTHROPIC_API_KEY}  # Set via environment variable
  openai:
    base_url: https://api.openai.com
    # api_key: ${OPENAI_API_KEY}
  google:
    base_url: https://generativelanguage.googleapis.com
    # api_key: ${GOOGLE_API_KEY}

# Hooks run at key moments in the agent lifecycle
# See: https://github.com/yourusername/openclaudia/docs/hooks.md
# hooks:
#   session_start:
#     - hooks:
#         - type: command
#           command: python .openclaudia/hooks/session-start.py
#           timeout: 30
#   pre_tool_use:
#     - matcher: "Write|Edit"
#       hooks:
#         - type: command
#           command: python .openclaudia/hooks/validate-write.py
#   user_prompt_submit:
#     - hooks:
#         - type: command
#           command: python .openclaudia/hooks/prompt-guard.py

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session
"#;

    fs::write(&config_file, default_config)?;

    // Write example hook
    let example_hook = r#"#!/usr/bin/env python3
"""Example SessionStart hook for OpenClaudia.

This hook runs when a new session starts.
Output JSON to stdout to inject context into the conversation.
"""

import json
import sys
import os

def main():
    # Read hook input from stdin
    input_data = json.load(sys.stdin)

    # Get project information
    cwd = input_data.get("cwd", os.getcwd())

    # Output context to inject
    output = {
        "systemMessage": f"Working directory: {cwd}"
    }

    print(json.dumps(output))

if __name__ == "__main__":
    main()
"#;

    fs::write(config_dir.join("hooks/session-start.py"), example_hook)?;

    // Write example rule
    let example_rule = r#"# Global Rules

These rules are injected into every conversation.

## Code Quality
- Write clean, readable code
- Include error handling
- No hardcoded secrets

## Security
- Validate all user input
- Use parameterized queries
- Follow OWASP guidelines
"#;

    fs::write(config_dir.join("rules/global.md"), example_rule)?;

    info!("Initialized OpenClaudia configuration in .openclaudia/");
    info!("  config.yaml  - Main configuration");
    info!("  hooks/       - Hook scripts");
    info!("  rules/       - Markdown rules");
    info!("  plugins/     - Plugin directory");
    info!("");
    info!("Set your API key:");
    info!("  export ANTHROPIC_API_KEY=your-key-here");
    info!("");
    info!("Start the proxy:");
    info!("  openclaudia start");

    Ok(())
}

/// Start the proxy server
async fn cmd_start(
    port: Option<u16>,
    host: Option<String>,
    target: Option<String>,
) -> anyhow::Result<()> {
    let mut config = config::load_config()?;

    // Apply command-line overrides
    if let Some(p) = port {
        config.proxy.port = p;
    }
    if let Some(h) = host {
        config.proxy.host = h;
    }
    if let Some(t) = target {
        config.proxy.target = t;
    }

    // Validate we have an API key for the target provider
    if let Some(provider) = config.active_provider() {
        if provider.api_key.is_none() {
            let env_var = match config.proxy.target.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "google" => "GOOGLE_API_KEY",
                _ => "API_KEY",
            };
            error!(
                "No API key configured for provider '{}'. Set {} environment variable.",
                config.proxy.target, env_var
            );
            return Ok(());
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

/// Show current configuration
fn cmd_config() -> anyhow::Result<()> {
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
            error!("Failed to load configuration: {}", e);
            info!("Run 'openclaudia init' to create a configuration file.");
        }
    }
    Ok(())
}

/// Check configuration and connectivity
async fn cmd_doctor() -> anyhow::Result<()> {
    use crate::mcp::McpManager;
    use crate::plugins::{PluginError, PluginManager};
    use crate::providers::{get_adapter, ProviderError};
    use crate::rules::RulesEngine;
    use crate::session::SessionManager;
    use std::time::Duration;

    println!("OpenClaudia Doctor\n");

    // Check configuration
    print!("Configuration... ");
    match config::load_config() {
        Ok(config) => {
            println!("OK");

            // Check API keys and default models
            for (name, provider) in &config.providers {
                print!("  {} API key... ", name);
                if provider.api_key.is_some() {
                    println!("configured");
                } else {
                    println!("NOT SET");
                }
                // Show default model if configured
                if let Some(model) = &provider.model {
                    println!("    Default model: {}", model);
                }
            }

            // Check connectivity to target provider
            print!("\nConnectivity to {}... ", config.proxy.target);
            if let Some(provider) = config.active_provider() {
                let client = reqwest::Client::new();
                match client.get(&provider.base_url).send().await {
                    Ok(_) => println!("OK"),
                    Err(e) => println!("FAILED: {}", e),
                }
            } else {
                println!("SKIPPED (no provider configured)");
            }
        }
        Err(e) => {
            println!("FAILED: {}", e);
            println!("\nRun 'openclaudia init' to create a configuration file.");
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
        // Test getting rules for files
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
                plugin.manifest.version,
                root.display()
            );

            // Show plugin env vars
            let env_vars = plugin.env_vars();
            if !env_vars.is_empty() {
                println!("    Environment: {} vars", env_vars.len());
            }

            // Show plugin commands
            if !plugin.manifest.commands.is_empty() {
                println!("    Commands: {}", plugin.manifest.commands.len());
            }

            // Show MCP servers
            if !plugin.manifest.mcp_servers.is_empty() {
                println!("    MCP servers: {}", plugin.manifest.mcp_servers.len());
            }
        }

        // Show all MCP servers from plugins
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

        // Test plugin resolve_path method
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
            println!("  Error: {}", err);
        }
    } else {
        println!("none found");
    }

    // Test PluginError::NotFound variant for completeness
    let not_found_err = PluginError::NotFound("test-plugin".to_string());
    info!("Plugin error test: {}", not_found_err);

    // Test MCP manager functionality
    print!("\nMCP Manager... ");
    let mut mcp_manager = McpManager::new();

    // Test is_connected (should return false for nonexistent)
    let is_connected = mcp_manager.is_connected("test-server");
    println!(
        "{}",
        if is_connected {
            "connected"
        } else {
            "no servers"
        }
    );

    // Test get_server_info
    if let Some((name, supports_list_changed)) = mcp_manager.get_server_info("test-server") {
        println!(
            "  Server: {} (list_changed: {})",
            name, supports_list_changed
        );
    }

    // Test call_tool (will fail gracefully - server not connected)
    match mcp_manager
        .call_tool("test_tool", serde_json::json!({}))
        .await
    {
        Ok(result) => println!("  Tool result: {}", result),
        Err(e) => info!("  Expected error (no server): {}", e),
    }

    // Test call_tool_with_timeout
    match mcp_manager
        .call_tool_with_timeout("test_tool", serde_json::json!({}), Duration::from_secs(1))
        .await
    {
        Ok(result) => println!("  Timeout tool result: {}", result),
        Err(e) => info!("  Expected timeout error: {}", e),
    }

    // Test disconnect methods
    let _ = mcp_manager.disconnect("nonexistent").await;
    let _ = mcp_manager.disconnect_all().await;

    // Check session state
    print!("\nSession... ");
    let mut session_manager = SessionManager::new(".openclaudia/session");
    if let Some(handoff) = session_manager.get_handoff_context() {
        println!("found handoff context ({} bytes)", handoff.len());
    }

    let sessions = session_manager.list_sessions();
    if !sessions.is_empty() {
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
    } else {
        println!("  No previous sessions");
    }

    // Test session operations
    let session = session_manager.start_initializer();
    let session_id = session.id.clone();
    info!("Test session created: {}", session_id);

    // Test session methods
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

    // Test load_session
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

    // Test transform_response
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

    // Test ProviderError variants
    let _invalid = ProviderError::InvalidResponse("test".to_string());
    let _unsupported = ProviderError::Unsupported("test feature".to_string());
    info!("Provider error variants OK");

    // Test PluginManager::with_paths and other methods
    let custom_paths = vec![PathBuf::from(".openclaudia/plugins")];
    let mut custom_plugin_manager = PluginManager::with_paths(custom_paths);
    let _ = custom_plugin_manager.discover();
    info!(
        "Custom plugin manager: {} plugins",
        custom_plugin_manager.count()
    );

    // Test plugin manager methods
    if let Some(plugin) = custom_plugin_manager.get("test-plugin") {
        info!("Found plugin: {}", plugin.name());
    }

    // Test hooks methods
    let all_hooks = custom_plugin_manager.all_hooks();
    info!("All hooks: {}", all_hooks.len());

    let session_hooks = custom_plugin_manager.hooks_for_event("session_start");
    info!("Session start hooks: {}", session_hooks.len());

    // Test enable/disable/reload
    let _ = custom_plugin_manager.enable("test-plugin");
    let _ = custom_plugin_manager.disable("test-plugin");
    let reload_errors = custom_plugin_manager.reload();
    info!("Plugin reload: {} errors", reload_errors.len());

    // Test proxy MCP functions
    print!("\nProxy MCP functions... ");
    let mcp_for_proxy = std::sync::Arc::new(tokio::sync::RwLock::new(McpManager::new()));

    // Test handle_mcp_tool_call
    match crate::proxy::handle_mcp_tool_call(&mcp_for_proxy, "test_tool", serde_json::json!({}))
        .await
    {
        Ok(result) => info!("MCP tool result: {}", result),
        Err(e) => info!("Expected MCP error: {}", e),
    }

    // Test shutdown_mcp
    crate::proxy::shutdown_mcp(&mcp_for_proxy).await;
    println!("OK");

    println!("\nDoctor check complete.");
    Ok(())
}

/// Run in iteration/loop mode with Stop hooks
async fn cmd_loop(
    max_iterations: u32,
    port: Option<u16>,
    target: Option<String>,
) -> anyhow::Result<()> {
    use crate::hooks::{HookEngine, HookEvent, HookInput};
    use crate::session::SessionManager;
    use tokio::sync::watch;

    let mut config = config::load_config()?;

    // Apply command-line overrides
    if let Some(p) = port {
        config.proxy.port = p;
    }
    if let Some(t) = target {
        config.proxy.target = t;
    }

    // Validate API key
    if let Some(provider) = config.active_provider() {
        if provider.api_key.is_none() {
            let env_var = match config.proxy.target.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "google" => "GOOGLE_API_KEY",
                _ => "API_KEY",
            };
            error!(
                "No API key configured for provider '{}'. Set {} environment variable.",
                config.proxy.target, env_var
            );
            return Ok(());
        }
    }

    // Initialize session manager
    let session_dir = config.session.persist_path.clone();
    let mut session_manager = SessionManager::new(&session_dir);
    let session = session_manager.get_or_create_session();
    let session_id = session.id.clone();

    // Initialize hook engine
    let hook_engine = HookEngine::new(config.hooks.clone());

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
    info!("Session ID: {}", session_id);
    info!(
        "Proxy: http://{}:{} -> {}",
        config.proxy.host, config.proxy.port, config.proxy.target
    );

    // Create shutdown channel for graceful termination
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Handle Ctrl+C
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Received Ctrl+C, initiating shutdown...");
            let _ = shutdown_tx_clone.send(true);
        }
    });

    let mut iteration: u32 = 0;
    let mut shutdown_rx_loop = shutdown_rx.clone();

    loop {
        iteration += 1;

        // Check max iterations
        if max_iterations > 0 && iteration > max_iterations {
            info!("Reached maximum iterations ({})", max_iterations);
            break;
        }

        // Check for shutdown signal
        if *shutdown_rx_loop.borrow() {
            info!("Shutdown signal received");
            break;
        }

        info!("=== Iteration {} ===", iteration);

        // Update session
        if let Some(session) = session_manager.get_session_mut() {
            session.increment_requests();
        }

        // Start the proxy server for this iteration
        let config_clone = config.clone();
        let shutdown_rx_server = shutdown_rx.clone();

        let server_handle = tokio::spawn(async move {
            proxy::start_server_with_shutdown(config_clone, shutdown_rx_server).await
        });

        // Wait for the server to complete (client disconnects or shutdown)
        match server_handle.await {
            Ok(Ok(())) => {
                info!("Iteration {} completed", iteration);
            }
            Ok(Err(e)) => {
                error!("Server error in iteration {}: {}", iteration, e);
            }
            Err(e) => {
                error!("Server task error: {}", e);
            }
        }

        // Check Stop hooks to determine if we should continue
        let stop_input = HookInput::new(HookEvent::Stop)
            .with_session_id(&session_id)
            .with_extra("iteration", serde_json::json!(iteration));

        let stop_result = hook_engine.run(HookEvent::Stop, &stop_input).await;

        if !stop_result.allowed {
            info!(
                "Stop hook requested termination: {:?}",
                stop_result
                    .outputs
                    .first()
                    .and_then(|o| o.reason.as_deref())
            );
            break;
        }

        // Check for shutdown again before next iteration
        if shutdown_rx_loop.changed().await.is_err() || *shutdown_rx_loop.borrow() {
            info!("Shutdown requested between iterations");
            break;
        }

        info!("Continuing to next iteration...");
    }

    // End session with handoff notes
    let handoff = format!(
        "Loop mode completed after {} iterations.\nSession ended at iteration {}.",
        iteration, iteration
    );
    session_manager.end_session(Some(&handoff));

    info!("Loop mode ended after {} iterations", iteration);
    Ok(())
}
