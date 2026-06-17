use openclaudia::tools::safe_truncate;

#[allow(clippy::too_many_lines)]
/// Authenticate with Claude Max subscription via OAuth
pub async fn cmd_auth(status: bool, logout: bool) -> anyhow::Result<()> {
    use openclaudia::oauth::{parse_auth_code, OAuthClient, OAuthStore, PkceParams};
    use std::io::{self, Write};

    let store = OAuthStore::new();

    // Handle --status flag
    if status {
        // Primary: the chat-usable Claude credential store
        // (<config-dir>/.credentials.json) — what the chat/proxy paths use via
        // `load_credentials`, and what `openclaudia auth` now writes.
        let creds_path = openclaudia::claude_credentials::credentials_path().map_or_else(
            || "~/.claude/.credentials.json".to_string(),
            |p| p.display().to_string(),
        );
        match openclaudia::claude_credentials::peek_credentials() {
            Ok(Some(s)) => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let remaining_secs = (s.expires_at_ms - now_ms).max(0) / 1000;
                println!("Claude credentials ({creds_path}):");
                println!(
                    "  subscription : {}",
                    s.subscription_type.as_deref().unwrap_or("unknown")
                );
                println!(
                    "  inference    : {}",
                    if s.has_inference_scope {
                        "yes"
                    } else {
                        "no (chat will fail)"
                    }
                );
                if s.expired {
                    println!("  status       : expired (auto-refreshes on next use)");
                } else if s.expires_soon {
                    println!("  status       : valid, expiring soon (auto-refreshes on next use)");
                } else {
                    println!(
                        "  status       : valid (~{}h{}m remaining)",
                        remaining_secs / 3600,
                        (remaining_secs % 3600) / 60
                    );
                }
            }
            Ok(None) => {
                println!("No Claude credentials at {creds_path}.");
                println!("Run 'openclaudia auth', or log in with Claude Code / openclaude.");
            }
            Err(e) => {
                eprintln!("Could not read {creds_path}: {e}");
            }
        }

        // Secondary: OpenClaudia's own device-flow OAuth session store. Separate
        // from the file above; empty unless `openclaudia auth` has run here.
        let session_count = dirs::data_local_dir()
            .map(|d| d.join("openclaudia").join("oauth_sessions.json"))
            .filter(|path| path.exists())
            .and_then(|path| std::fs::read_to_string(&path).ok())
            .and_then(|content| {
                serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(
                    &content,
                )
                .ok()
            })
            .map_or(0, |sessions| sessions.len());
        println!();
        if session_count == 0 {
            println!("Native OAuth session store: empty.");
        } else {
            println!("Native OAuth session store: {session_count} session(s).");
        }
        return Ok(());
    }

    // Handle --logout flag
    if logout {
        let persist_path =
            dirs::data_local_dir().map(|d| d.join("openclaudia").join("oauth_sessions.json"));

        if let Some(path) = persist_path {
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("Logged out. OAuth sessions cleared.");
            } else {
                println!("No OAuth sessions to clear.");
            }
        }
        return Ok(());
    }

    // Start OAuth device flow
    println!("=== Claude Max OAuth Authentication ===\n");

    let pkce = PkceParams::generate();
    let auth_url = pkce.build_auth_url();

    println!("Step 1: Open this URL in your browser:\n");
    println!("  {auth_url}\n");

    // Try to open browser automatically
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &auth_url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&auth_url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&auth_url)
            .spawn();
    }

    println!("Step 2: Sign in to Claude and authorize the application.");
    println!("Step 3: Copy the code shown (format: CODE#STATE)\n");

    print!("Paste the authorization code here: ");
    io::stdout().flush()?;

    let mut code_input = String::new();
    io::stdin().read_line(&mut code_input)?;
    let code_input = code_input.trim();

    if code_input.is_empty() {
        eprintln!("No code provided. Authentication cancelled.");
        return Ok(());
    }

    let (code, parsed_state) = parse_auth_code(code_input);

    let expected_state = &pkce.state;
    if let Some(ref state) = parsed_state {
        if state != expected_state {
            eprintln!("State mismatch! This could be a CSRF attack. Authentication cancelled.");
            return Ok(());
        }
    }

    println!("\nExchanging code for tokens...");

    let client = OAuthClient::new()?;
    let token_response = client.exchange_code(&code, &pkce).await?;

    let mut session = openclaudia::oauth::OAuthSession::from_token_response(token_response);

    if session.can_create_api_key() {
        println!("Creating API key from OAuth token...");
        match client
            .create_api_key(&session.credentials.access_token)
            .await
        {
            Ok(api_key) => {
                session.api_key = Some(api_key);
                println!("API key created successfully");
            }
            Err(e) => {
                eprintln!("Warning: Failed to create API key: {e}");
                eprintln!("Falling back to Bearer token authentication.");
                session.auth_mode = openclaudia::oauth::AuthMode::BearerToken;
            }
        }
    } else {
        println!("Using Bearer token authentication (personal Claude Max account)");
        println!("  Granted scopes: {}", session.granted_scopes.join(", "));
    }

    // Persist to Claude Code's standard credential store so OpenClaudia's
    // chat/proxy paths (`claude_credentials::load_credentials`) can use this
    // login directly — no Claude Code install required. Gated on the inference
    // scope the chat path requires; the token endpoint omits subscription /
    // rate-limit metadata, so pass None (store_credentials preserves any
    // existing values).
    if session.granted_scopes.iter().any(|s| s == "user:inference") {
        match openclaudia::claude_credentials::store_credentials(
            &session.credentials.access_token,
            session.credentials.refresh_token.as_deref(),
            session.credentials.expires_at.timestamp_millis(),
            session.granted_scopes.clone(),
            None,
            None,
        ) {
            Ok(()) => println!("Saved Claude credentials to ~/.claude/.credentials.json"),
            Err(e) => eprintln!("Warning: could not write ~/.claude/.credentials.json: {e}"),
        }
    } else {
        eprintln!(
            "Note: granted scopes lack 'user:inference'; \
             skipped writing ~/.claude/.credentials.json"
        );
    }

    let session_id = session.id.clone();
    let auth_mode = session.auth_mode.clone();
    store.store_session(session);

    println!("\nAuthentication successful!");
    println!("  Session ID: {}", safe_truncate(&session_id, 8));
    match auth_mode {
        openclaudia::oauth::AuthMode::ApiKey => {
            println!("  Auth mode: API key (organization account)");
        }
        openclaudia::oauth::AuthMode::BearerToken => {
            println!("  Auth mode: Bearer token (personal account)");
        }
        openclaudia::oauth::AuthMode::ProxyMode => {
            println!("  Auth mode: Proxy (via anthropic-proxy)");
        }
    }
    println!("\nYour session has been saved. OpenClaudia will now use your");
    println!("Claude Max subscription automatically when target is 'anthropic'.");

    Ok(())
}
