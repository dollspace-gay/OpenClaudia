use openclaudia::{
    config, guardrails,
    hooks::{HookEngine, HookEvent, HookInput},
    proxy,
    session::{EndSessionError, SessionManager},
};
use tokio::sync::watch;
use tracing::{error, info};

#[allow(clippy::too_many_lines)]
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

    if provider.api_key.is_none() {
        let env_var = match config.proxy.target.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            "google" | "gemini" => "GOOGLE_API_KEY",
            "zai" | "glm" | "zhipu" => "ZAI_API_KEY",
            "deepseek" => "DEEPSEEK_API_KEY",
            "qwen" | "alibaba" => "QWEN_API_KEY",
            "kimi" | "moonshot" => "KIMI_API_KEY or MOONSHOT_API_KEY",
            "minimax" => "MINIMAX_API_KEY",
            _ => "API_KEY",
        };
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

    let session_dir = config.session.persist_path.clone();
    let mut session_manager = SessionManager::new(&session_dir);
    let session = session_manager.get_or_create_session();
    let session_id = session.id.clone();

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

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        if matches!(tokio::signal::ctrl_c().await, Ok(())) {
            info!("Received Ctrl+C, initiating shutdown...");
            let _ = shutdown_tx_clone.send(true);
        }
    });

    let mut iteration: u32 = 0;
    let mut shutdown_rx_loop = shutdown_rx.clone();

    loop {
        iteration += 1;

        if max_iterations > 0 && iteration > max_iterations {
            info!("Reached maximum iterations ({})", max_iterations);
            break;
        }

        if *shutdown_rx_loop.borrow() {
            info!("Shutdown signal received");
            break;
        }

        info!("=== Iteration {} ===", iteration);

        if let Some(session) = session_manager.get_session_mut() {
            session.increment_requests();
        }

        let config_clone = config.clone();
        let shutdown_rx_server = shutdown_rx.clone();

        let server_handle = tokio::spawn(async move {
            proxy::start_server_with_shutdown(config_clone, shutdown_rx_server).await
        });

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

        if shutdown_rx_loop.changed().await.is_err() || *shutdown_rx_loop.borrow() {
            info!("Shutdown requested between iterations");
            break;
        }

        info!("Continuing to next iteration...");
    }

    let handoff = format!(
        "Loop mode completed after {iteration} iterations.\nSession ended at iteration {iteration}."
    );
    // #356: end_session is now fallible.  Log persist failures here rather
    // than propagating, since the loop has already done its work — but do
    // NOT swallow them silently as the old implementation did.
    match session_manager.end_session(Some(&handoff)) {
        Ok(_) | Err(EndSessionError::NotFound) => {}
        Err(e @ EndSessionError::PersistFailed { .. }) => {
            error!(error = %e, "Failed to persist session at end of loop mode");
        }
    }

    info!("Loop mode ended after {} iterations", iteration);
    Ok(())
}
