//! Claude Code credential reader for direct Anthropic API authentication.
//!
//! Reads OAuth tokens from Claude Code's credential store (`~/.claude/.credentials.json`)
//! and uses them directly with the Anthropic Messages API via `Authorization: Bearer`.
//! Handles automatic token refresh when tokens are expired.
//!
//! This enables OpenClaudia users who have Claude Code installed and logged in
//! to use their existing subscription without an API key or proxy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

/// Claude Code's OAuth client ID (public, hardcoded in Claude Code source)
const CLAUDE_CODE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Token exchange/refresh endpoint
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// OAuth beta header required when using subscriber tokens
pub const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

/// Claude Code beta header for agentic queries
pub const CLAUDE_CODE_BETA_HEADER: &str = "claude-code-20250219";

/// Interleaved thinking beta
pub const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// 5 minute buffer before expiry to trigger refresh
const REFRESH_BUFFER_MS: i64 = 5 * 60 * 1000;

/// Credential structure matching Claude Code's `~/.claude/.credentials.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    pub claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAiOauth {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "refreshToken")]
    pub refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    pub expires_at: i64, // milliseconds since epoch
    pub scopes: Vec<String>,
    #[serde(rename = "subscriptionType")]
    pub subscription_type: Option<String>,
    #[serde(rename = "rateLimitTier")]
    pub rate_limit_tier: Option<String>,
}

/// Result of loading credentials
#[derive(Debug, Clone)]
pub struct LoadedCredentials {
    pub access_token: String,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub scopes: Vec<String>,
}

/// Get the path to Claude Code's credentials file
fn credentials_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join(".credentials.json"))
}

/// Check if Claude Code credentials exist
pub fn has_claude_code_credentials() -> bool {
    credentials_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Load Claude Code credentials, refreshing if expired.
///
/// Returns the access token ready for use as `Authorization: Bearer <token>`.
pub async fn load_credentials() -> Result<LoadedCredentials, String> {
    let path = credentials_path()
        .ok_or("Cannot determine home directory")?;

    if !path.exists() {
        return Err(format!(
            "Claude Code credentials not found at {}. Run `claude` and log in first.",
            path.display()
        ));
    }

    // Reject symlinks to prevent credential theft via symlink attacks
    if path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "Credentials file {} is a symlink — refusing to read for security",
            path.display()
        ));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let creds: CredentialsFile = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse credentials: {}", e))?;

    let oauth = creds.claude_ai_oauth
        .ok_or("No claudeAiOauth section in credentials file")?;

    // Check if user:inference scope is present
    if !oauth.scopes.iter().any(|s| s == "user:inference") {
        return Err("Claude Code credentials lack 'user:inference' scope. Re-login with Claude Code.".to_string());
    }

    // Check expiry (with 5 minute buffer)
    let now_ms = chrono::Utc::now().timestamp_millis();
    if now_ms + REFRESH_BUFFER_MS >= oauth.expires_at {
        info!("Claude Code token expired or expiring soon, refreshing...");
        return refresh_and_load(&path, &oauth).await;
    }

    debug!(
        "Claude Code credentials loaded (expires in {}s, type: {:?})",
        (oauth.expires_at - now_ms) / 1000,
        oauth.subscription_type
    );

    Ok(LoadedCredentials {
        access_token: oauth.access_token,
        subscription_type: oauth.subscription_type,
        rate_limit_tier: oauth.rate_limit_tier,
        scopes: oauth.scopes,
    })
}

/// Refresh the token and update the credentials file.
///
/// Known limitation: this function has no locking mechanism, so concurrent
/// processes calling `refresh_and_load` simultaneously could race. This is
/// acceptable for single-process use (OpenClaudia only runs one instance).
/// Claude Code itself uses a file lock (`~/.claude/.refresh_lock`) for
/// multi-process safety, but we skip that complexity here.
async fn refresh_and_load(path: &PathBuf, oauth: &ClaudeAiOauth) -> Result<LoadedCredentials, String> {
    let refresh_token = oauth.refresh_token.as_deref()
        .ok_or("No refresh token available — re-login with Claude Code")?;

    let scopes = oauth.scopes.join(" ");

    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLAUDE_CODE_CLIENT_ID,
            "scope": scopes,
        }))
        .send()
        .await
        .map_err(|e| format!("Token refresh request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Token refresh failed ({}): {}", status, body));
    }

    let refresh_response: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse refresh response: {}", e))?;

    let new_access_token = refresh_response["access_token"]
        .as_str()
        .ok_or("No access_token in refresh response")?
        .to_string();

    let new_refresh_token = refresh_response["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token)
        .to_string();

    let expires_in = refresh_response["expires_in"]
        .as_i64()
        .unwrap_or(3600);

    let new_expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    // Parse scopes from response
    let new_scopes: Vec<String> = refresh_response["scope"]
        .as_str()
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_else(|| oauth.scopes.clone());

    // Update credentials file
    let updated = CredentialsFile {
        claude_ai_oauth: Some(ClaudeAiOauth {
            access_token: new_access_token.clone(),
            refresh_token: Some(new_refresh_token),
            expires_at: new_expires_at,
            scopes: new_scopes.clone(),
            subscription_type: oauth.subscription_type.clone(),
            rate_limit_tier: oauth.rate_limit_tier.clone(),
        }),
    };

    let json = serde_json::to_string_pretty(&updated)
        .map_err(|e| format!("Failed to serialize updated credentials: {}", e))?;

    // Reject symlinks before writing refreshed tokens
    if path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "Credentials file {} is a symlink — refusing to write for security",
            path.display()
        ));
    }

    std::fs::write(path, json)
        .map_err(|e| format!("Failed to write updated credentials: {}", e))?;

    // Preserve original file permissions (0600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    info!("Token refreshed successfully (expires in {}s)", expires_in);

    Ok(LoadedCredentials {
        access_token: new_access_token,
        subscription_type: oauth.subscription_type.clone(),
        rate_limit_tier: oauth.rate_limit_tier.clone(),
        scopes: new_scopes,
    })
}

/// Build the HTTP headers for Anthropic API with OAuth Bearer auth.
///
/// These headers replace the `x-api-key` header used with API keys.
pub fn get_oauth_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("Authorization".to_string(), format!("Bearer {}", access_token)),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
        // Beta headers matching what Claude Code sends (required for OAuth model access)
        ("anthropic-beta".to_string(), format!(
            "{},{},{}",
            CLAUDE_CODE_BETA_HEADER,
            OAUTH_BETA_HEADER,
            INTERLEAVED_THINKING_BETA,
        )),
    ]
}

/// Get the API endpoint for OAuth-authenticated requests.
pub fn get_oauth_endpoint(_model: &str) -> String {
    "https://api.anthropic.com/v1/messages".to_string()
}

/// The system prompt prefix that must be present for OAuth tokens to access
/// premium models (Sonnet, Opus). The Anthropic API validates this string.
pub const CLAUDE_CODE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Inject the Claude Code system prompt into a request body.
/// This must be the first element in the system array for OAuth model access.
pub fn inject_system_prompt(request: &mut serde_json::Value) {
    let claude_code_obj = serde_json::json!({
        "type": "text",
        "text": CLAUDE_CODE_SYSTEM_PROMPT,
    });

    match request.get_mut("system") {
        Some(serde_json::Value::Array(arr)) => {
            arr.insert(0, claude_code_obj);
        }
        Some(serde_json::Value::String(existing)) => {
            let existing_obj = serde_json::json!({
                "type": "text",
                "text": existing.clone(),
            });
            request["system"] = serde_json::json!([claude_code_obj, existing_obj]);
        }
        _ => {
            request["system"] = serde_json::json!([claude_code_obj]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_path() {
        let path = credentials_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.to_str().unwrap().contains(".claude"));
        assert!(p.to_str().unwrap().ends_with(".credentials.json"));
    }

    #[test]
    fn test_parse_credentials() {
        let json = r#"{
            "claudeAiOauth": {
                "accessToken": "test-token",
                "refreshToken": "test-refresh",
                "expiresAt": 9999999999999,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_20x"
            }
        }"#;

        let creds: CredentialsFile = serde_json::from_str(json).unwrap();
        let oauth = creds.claude_ai_oauth.unwrap();
        assert_eq!(oauth.access_token, "test-token");
        assert_eq!(oauth.refresh_token, Some("test-refresh".to_string()));
        assert_eq!(oauth.subscription_type, Some("max".to_string()));
        assert!(oauth.scopes.contains(&"user:inference".to_string()));
    }

    #[test]
    fn test_parse_credentials_no_oauth() {
        let json = r#"{}"#;
        let creds: CredentialsFile = serde_json::from_str(json).unwrap();
        assert!(creds.claude_ai_oauth.is_none());
    }

    #[test]
    fn test_get_oauth_headers() {
        let headers = get_oauth_headers("test-token-123");
        assert!(headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer test-token-123"));
        assert!(headers.iter().any(|(k, v)| k == "anthropic-beta" && v.contains("oauth-2025-04-20")));
        assert!(headers.iter().any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
    }

    #[test]
    fn test_has_credentials_function() {
        // Just verify it doesn't panic
        let _ = has_claude_code_credentials();
    }
}
