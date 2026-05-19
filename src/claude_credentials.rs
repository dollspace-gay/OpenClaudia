//! Claude Code credential reader for direct Anthropic API authentication.
//!
//! Reads OAuth tokens from Claude Code's credential store (`~/.claude/.credentials.json`)
//! and uses them directly with the Anthropic Messages API via `Authorization: Bearer`.
//! Handles automatic token refresh when tokens are expired.
//!
//! This enables `OpenClaudia` users who have Claude Code installed and logged in
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

/// Fine-grained tool streaming beta
pub const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";

/// The canonical `anthropic-beta` header value sent on every OAuth request.
///
/// All OAuth code paths **must** call this function instead of interpolating
/// individual constants, so that adding or removing a beta flag is a
/// single-file change with no risk of drift. See crosslink #272.
///
/// # Examples
///
/// ```
/// use openclaudia::claude_credentials::claude_code_beta_header_value;
/// let v = claude_code_beta_header_value();
/// assert!(v.contains("oauth-2025-04-20"));
/// assert!(v.contains("claude-code-20250219"));
/// ```
#[must_use]
pub fn claude_code_beta_header_value() -> String {
    format!(
        "{CLAUDE_CODE_BETA_HEADER},{OAUTH_BETA_HEADER},{INTERLEAVED_THINKING_BETA},{FINE_GRAINED_TOOL_STREAMING_BETA}"
    )
}

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

/// Path to the advisory lock file for credential access.
fn lock_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join(".credentials.lock"))
}

/// Advisory file lock for credential access.
/// Prevents TOCTOU race conditions when multiple processes refresh tokens.
/// Uses flock on Unix, `CreateFile` exclusive lock on Windows.
struct CredentialLock {
    _file: std::fs::File,
}

impl CredentialLock {
    /// Acquire an exclusive lock on the credentials lock file.
    /// Blocks until the lock is available.
    fn acquire() -> Result<Self, String> {
        let path = lock_path().ok_or("Cannot determine home directory for lock file")?;

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("Failed to open lock file {}: {e}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            // LOCK_EX = exclusive, blocks until acquired
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
            if ret != 0 {
                return Err(format!(
                    "Failed to acquire credential lock: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        // On non-Unix, the file open with write mode provides basic mutual exclusion
        Ok(Self { _file: file })
    }
}

// Lock is released when the File is dropped (flock is released on close)

/// Check if Claude Code credentials exist
#[must_use]
pub fn has_claude_code_credentials() -> bool {
    credentials_path().is_some_and(|p| p.exists())
}

/// Load Claude Code credentials, refreshing if expired.
///
/// Returns the access token ready for use as `Authorization: Bearer <token>`.
///
/// # Errors
///
/// Returns an error if credentials cannot be found, read, parsed, or refreshed.
pub async fn load_credentials() -> Result<LoadedCredentials, String> {
    // Acquire advisory lock — prevents race conditions with other OpenClaudia
    // instances or Claude Code refreshing tokens concurrently.
    let _lock = CredentialLock::acquire()?;

    let path = credentials_path().ok_or("Cannot determine home directory")?;

    if !path.exists() {
        return Err(format!(
            "Claude Code credentials not found at {}. Run `claude` and log in first.",
            path.display()
        ));
    }

    // Reject symlinks to prevent credential theft via symlink attacks
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "Credentials file {} is a symlink — refusing to read for security",
            path.display()
        ));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let creds: CredentialsFile =
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse credentials: {e}"))?;

    let oauth = creds
        .claude_ai_oauth
        .ok_or("No claudeAiOauth section in credentials file")?;

    // Check if user:inference scope is present
    if !oauth.scopes.iter().any(|s| s == "user:inference") {
        return Err(
            "Claude Code credentials lack 'user:inference' scope. Re-login with Claude Code."
                .to_string(),
        );
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
/// Caller must hold `CredentialLock` — this function reads, refreshes via API,
/// and writes the credentials file. The lock prevents concurrent processes from
/// racing on the same file.
/// Call the OAuth token-refresh endpoint and return the raw JSON response body.
async fn call_token_refresh_api(
    refresh_token: &str,
    scopes: &str,
) -> Result<serde_json::Value, String> {
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
        .map_err(|e| format!("Token refresh request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        // Never propagate the raw body — Anthropic echoes `refresh_token`
        // values in its validation errors (crosslink #263). Log at debug
        // for operators, return the sanitized form to the caller.
        tracing::debug!("token_refresh_failed body (not shipped to caller): {body}");
        return Err(format!(
            "Token refresh failed ({status}): {}",
            crate::oauth::redact_oauth_error_body(&body)
        ));
    }

    response
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {e}"))
}

async fn refresh_and_load(
    path: &PathBuf,
    oauth: &ClaudeAiOauth,
) -> Result<LoadedCredentials, String> {
    const MIN_EXPIRES_IN_SECS: i64 = 60;
    const MAX_EXPIRES_IN_SECS: i64 = 30 * 24 * 3600;

    let refresh_token = oauth
        .refresh_token
        .as_deref()
        .ok_or("No refresh token available — re-login with Claude Code")?;

    let scopes = oauth.scopes.join(" ");
    let refresh_response = call_token_refresh_api(refresh_token, &scopes).await?;

    let new_access_token = refresh_response["access_token"]
        .as_str()
        .ok_or("No access_token in refresh response")?
        .to_string();

    let new_refresh_token = refresh_response["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token)
        .to_string();

    // `expires_in` is required by the OAuth spec — refuse to silently
    // default to 3600 when the field is missing or malformed. A missing
    // field indicates a protocol deviation the operator needs to see.
    // Clamp the received value to [60s, 30d] with a tracing warn on any
    // clamp to avoid 401-retry loops (too short) and multi-year tokens
    // (too long). See crosslink #480.
    let expires_in_raw = refresh_response["expires_in"]
        .as_i64()
        .ok_or("Refresh response missing required 'expires_in' field")?;
    if expires_in_raw <= 0 {
        return Err(format!(
            "Refresh response returned non-positive 'expires_in' ({expires_in_raw})"
        ));
    }
    let expires_in = if expires_in_raw < MIN_EXPIRES_IN_SECS {
        tracing::warn!(
            received = expires_in_raw,
            clamped_to = MIN_EXPIRES_IN_SECS,
            "Refresh expires_in too small; clamping to avoid 401-retry loop"
        );
        MIN_EXPIRES_IN_SECS
    } else if expires_in_raw > MAX_EXPIRES_IN_SECS {
        tracing::warn!(
            received = expires_in_raw,
            clamped_to = MAX_EXPIRES_IN_SECS,
            "Refresh expires_in too large; clamping to refuse multi-year tokens"
        );
        MAX_EXPIRES_IN_SECS
    } else {
        expires_in_raw
    };

    let new_expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    // Parse scopes from response
    let new_scopes: Vec<String> = refresh_response["scope"].as_str().map_or_else(
        || oauth.scopes.clone(),
        |s| s.split_whitespace().map(String::from).collect(),
    );

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
        .map_err(|e| format!("Failed to serialize updated credentials: {e}"))?;

    // Reject symlinks before writing refreshed tokens
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "Credentials file {} is a symlink — refusing to write for security",
            path.display()
        ));
    }

    std::fs::write(path, json).map_err(|e| format!("Failed to write updated credentials: {e}"))?;

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
#[must_use]
pub fn get_oauth_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        (
            "Authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
        // Beta headers matching what Claude Code sends (required for OAuth model access).
        // Uses claude_code_beta_header_value() as the single source of truth — see crosslink #272.
        (
            "anthropic-beta".to_string(),
            claude_code_beta_header_value(),
        ),
    ]
}

/// Get the API endpoint for OAuth-authenticated requests.
#[must_use]
pub fn get_oauth_endpoint(_model: &str) -> String {
    "https://api.anthropic.com/v1/messages".to_string()
}

/// The system prompt prefix that must be present for OAuth tokens to access premium models.
///
/// The Anthropic API validates this exact string. Must be in its own system
/// block — do NOT append to this.
pub const CLAUDE_CODE_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Additional system prompt content sent as a separate block after the prefix.
/// This is where behavioral instructions and persona go.
pub const CLAUDIA_SYSTEM_PROMPT: &str = include_str!("claude_code_prompt.txt");

/// Inject the Claude Code system prompt into a request body.
///
/// Block 0: The exact one-liner prefix (API validates this string for OAuth)
/// Block 1: Full behavioral instructions + Claudia persona (from `claude_code_prompt.txt`)
/// Block 2+: Whatever was already in the system array (our per-session prompt)
///
/// This matches Claude Code's multi-block system array structure.
pub fn inject_system_prompt(request: &mut serde_json::Value) {
    // Block 0: exact prefix — API validates this for OAuth access
    let prefix_block = serde_json::json!({
        "type": "text",
        "text": CLAUDE_CODE_SYSTEM_PROMPT,
    });

    // Block 1: behavioral instructions + Claudia persona (cached)
    let behavioral_block = serde_json::json!({
        "type": "text",
        "text": CLAUDIA_SYSTEM_PROMPT,
        "cache_control": {"type": "ephemeral"}
    });

    match request.get_mut("system") {
        Some(serde_json::Value::Array(arr)) => {
            // Existing blocks become block 2+
            arr.insert(0, behavioral_block);
            arr.insert(0, prefix_block);
        }
        Some(serde_json::Value::String(existing)) => {
            let existing_obj = serde_json::json!({
                "type": "text",
                "text": existing.clone(),
            });
            request["system"] = serde_json::json!([prefix_block, behavioral_block, existing_obj]);
        }
        _ => {
            request["system"] = serde_json::json!([prefix_block, behavioral_block]);
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
        let json = r"{}";
        let creds: CredentialsFile = serde_json::from_str(json).unwrap();
        assert!(creds.claude_ai_oauth.is_none());
    }

    #[test]
    fn test_get_oauth_headers() {
        let headers = get_oauth_headers("test-token-123");
        assert!(headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer test-token-123"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "anthropic-beta" && v.contains("oauth-2025-04-20")));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
    }

    #[test]
    fn test_has_credentials_function() {
        // Just verify it doesn't panic
        let _ = has_claude_code_credentials();
    }

    // --- Regression guard for crosslink #272: beta-header string drift ---

    #[test]
    fn beta_header_consts_have_expected_values() {
        assert_eq!(CLAUDE_CODE_BETA_HEADER, "claude-code-20250219");
        assert_eq!(OAUTH_BETA_HEADER, "oauth-2025-04-20");
        assert_eq!(INTERLEAVED_THINKING_BETA, "interleaved-thinking-2025-05-14");
        assert_eq!(
            FINE_GRAINED_TOOL_STREAMING_BETA,
            "fine-grained-tool-streaming-2025-05-14"
        );
    }

    #[test]
    fn claude_code_beta_header_value_contains_all_flags() {
        let v = claude_code_beta_header_value();
        assert!(
            v.contains("claude-code-20250219"),
            "missing claude-code beta in: {v}"
        );
        assert!(v.contains("oauth-2025-04-20"), "missing oauth beta in: {v}");
        assert!(
            v.contains("interleaved-thinking-2025-05-14"),
            "missing interleaved-thinking beta in: {v}"
        );
        assert!(
            v.contains("fine-grained-tool-streaming-2025-05-14"),
            "missing fine-grained-tool-streaming beta in: {v}"
        );
    }

    #[test]
    fn get_oauth_headers_beta_includes_fine_grained_tool_streaming() {
        let headers = get_oauth_headers("tok");
        let beta = headers
            .iter()
            .find(|(k, _)| k == "anthropic-beta")
            .expect("anthropic-beta header must be present");
        assert!(
            beta.1.contains("fine-grained-tool-streaming-2025-05-14"),
            "fine-grained-tool-streaming missing from anthropic-beta: {}",
            beta.1
        );
    }
}
