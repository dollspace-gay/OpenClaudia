use openclaudia::tools::safe_truncate;

/// Authenticate with Claude Max subscription via OAuth
pub async fn cmd_auth(status: bool, logout: bool) -> anyhow::Result<()> {
    use openclaudia::oauth::{parse_auth_code, OAuthClient, OAuthStore, PkceParams};
    use std::io::{self, Write};

    let store = OAuthStore::new();

    // Handle --status flag
    if status {
        let sessions: Vec<_> = {
            let _store = OAuthStore::new();
            let persist_path =
                dirs::data_local_dir().map(|d| d.join("openclaudia").join("oauth_sessions.json"));

            if let Some(path) = persist_path {
                if path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(sessions) = serde_json::from_str::<
                            std::collections::HashMap<String, serde_json::Value>,
                        >(&content)
                        {
                            sessions.into_iter().collect()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        };

        if sessions.is_empty() {
            println!("Not authenticated with Claude Max.");
            println!("Run 'openclaudia auth' to authenticate.");
        } else {
            println!("Authenticated with Claude Max.");
            println!("Sessions: {}", sessions.len());
            for (id, data) in &sessions {
                let expires = data
                    .get("credentials")
                    .and_then(|c| c.get("expires_at"))
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                println!("  {} (expires: {})", safe_truncate(id, 8), expires);
            }
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
    println!("  {}\n", auth_url);

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

    let client = OAuthClient::new();
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
                eprintln!("Warning: Failed to create API key: {}", e);
                eprintln!("Falling back to Bearer token authentication.");
                session.auth_mode = openclaudia::oauth::AuthMode::BearerToken;
            }
        }
    } else {
        println!("Using Bearer token authentication (personal Claude Max account)");
        println!("  Granted scopes: {}", session.granted_scopes.join(", "));
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
