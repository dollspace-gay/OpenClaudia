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

/// Resolve Claude Code's config directory.
///
/// Honors `CLAUDE_CONFIG_DIR` (matching Claude Code and forks such as
/// `@gitlawb/openclaude`) so multi-profile / container setups read and write
/// the same location; falls back to `~/.claude`.
fn claude_config_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::home_dir().map(|h| h.join(".claude")),
    }
}

/// Path to Claude Code's credentials file (`<config-dir>/.credentials.json`).
///
/// Public so callers (e.g. `openclaudia auth --status`) can show users where
/// credentials live.
#[must_use]
pub fn credentials_path() -> Option<PathBuf> {
    claude_config_dir().map(|d| d.join(".credentials.json"))
}

/// Path to the advisory lock file for credential access.
fn lock_path() -> Option<PathBuf> {
    claude_config_dir().map(|d| d.join(".credentials.lock"))
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

        // Note (crosslink #492 follow-up): this `OpenOptions::open` site is a
        // remaining candidate for `FileError`. The focused #492 pass left it on
        // `String` because converting `CredentialLock` to return `FileError`
        // would also require accommodating the libc::flock branch below
        // (an OS syscall, not file-content I/O). Tracked for a follow-up pass
        // so the public `acquire(...) -> Result<_, String>` contract stays
        // stable until that wider change is scoped.
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

        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let handle = file.as_raw_handle();
            // Lock the entire file exclusively (LOCKFILE_EXCLUSIVE_LOCK).
            // LOCKFILE_FAIL_IMMEDIATELY is NOT set — this blocks until the
            // lock is acquired, matching Unix `flock(LOCK_EX)` semantics.
            const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
            // Overlapped structure required by `LockFileEx` even for synchronous
            // calls. A zero-initialized `OVERLAPPED` is sufficient — Win32 docs
            // allow a null event handle / completion port for blocking locks.
            let overlapped = std::mem::MaybeUninit::<
                windows_sys::Win32::System::IO::OVERLAPPED,
            >::zeroed();
            // SAFETY: all-zero is a valid `OVERLAPPED` for sync `LockFileEx`.
            let mut overlapped = unsafe { overlapped.assume_init() };
            let ret = unsafe {
                windows_sys::Win32::Storage::FileSystem::LockFileEx(
                    handle as _,
                    LOCKFILE_EXCLUSIVE_LOCK,
                    0,
                    // NumberOfBytesToLockLow / High = 0 → lock entire file
                    0xFFFF_FFFF,
                    0xFFFF_FFFF,
                    &mut overlapped,
                )
            };
            if ret == 0 {
                return Err(format!(
                    "Failed to acquire credential lock: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        // Lock is released when the File is dropped:
        //   Unix: flock is released on close.
        //   Windows: CloseHandle (via Drop) releases the LockFileEx lock.
        Ok(Self { _file: file })
    }
}

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

    // File I/O and JSON parsing flow through the typed `FileError` enum so
    // the underlying io::ErrorKind / serde_json::Error are preserved on the
    // way out — see crosslink #492. We stringify here at the public boundary
    // because `load_credentials` still returns `Result<_, String>` for
    // backwards-compat with existing callers; the rendered message now
    // always names the file and the source chain.
    let content = crate::file_error::read_file(&path)
        .map_err(|e: crate::file_error::FileError| e.to_string())?;

    let creds: CredentialsFile = serde_json::from_str(&content)
        .map_err(crate::file_error::FileError::json_with_path(&path))
        .map_err(|e| e.to_string())?;

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

/// Resolve the `refresh_token` to persist after a successful refresh.
///
/// Returns the response's `refresh_token` field when present. When absent,
/// requires `OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE=1` to be set before
/// silently reusing the old one — see crosslink #825.
fn resolve_new_refresh_token(
    response_field: Option<&str>,
    previous_refresh_token: &str,
) -> Result<String, String> {
    if let Some(s) = response_field {
        return Ok(s.to_string());
    }
    let allow_reuse = std::env::var("OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE")
        .ok()
        .as_deref()
        == Some("1");
    if !allow_reuse {
        return Err(
            "Refresh response omitted 'refresh_token' field; refusing to reuse \
             the previous one (set OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE=1 to \
             opt in if your provider uses non-rotating refresh tokens)"
                .to_string(),
        );
    }
    tracing::warn!(
        "Refresh response omitted 'refresh_token' field; reusing previous \
         refresh token under OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE=1 — if your \
         provider rotates refresh tokens this will break on the next refresh"
    );
    Ok(previous_refresh_token.to_string())
}

async fn refresh_and_load(
    path: &std::path::Path,
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

    // Crosslink #825: when the refresh response omits the `refresh_token`
    // field, the OAuth server may either (a) be using non-rotating refresh
    // tokens — in which case reusing the old one is intentional — or (b) be
    // returning a partial / broken response. Silently reusing the old token
    // under (b) means we could lose the ability to refresh on the *next*
    // cycle without any operator-visible signal. Require an explicit
    // opt-in (`OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE=1`) before reusing,
    // and `warn!` so it shows up in logs either way.
    let new_refresh_token =
        resolve_new_refresh_token(refresh_response["refresh_token"].as_str(), refresh_token)?;

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

    // Persist via the shared writer (symlink guard + 0600 perms + typed
    // FileError on the way out). See crosslink #492.
    write_credentials_file(path, &updated)?;

    info!("Token refreshed successfully (expires in {}s)", expires_in);

    Ok(LoadedCredentials {
        access_token: new_access_token,
        subscription_type: oauth.subscription_type.clone(),
        rate_limit_tier: oauth.rate_limit_tier.clone(),
        scopes: new_scopes,
    })
}

/// Serialize and write a [`CredentialsFile`] to `path`.
///
/// Shared by [`refresh_and_load`] (token refresh) and [`store_credentials`]
/// (initial login). Refuses to follow a symlink, creates the parent directory
/// if needed, and replaces the target **atomically** (temp file + rename) with
/// `0600` perms on Unix — a crash or concurrent reader can never observe a
/// half-written credentials file. See review H1/H2.
fn write_credentials_file(
    path: &std::path::Path,
    creds: &CredentialsFile,
) -> Result<(), String> {
    // Reject symlinks before writing tokens (symlink-swap attack guard).
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "Credentials file {} is a symlink — refusing to write for security",
            path.display()
        ));
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("credentials path {} has no parent directory", path.display()))?;
    // First login may predate the config directory.
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;

    let json = serde_json::to_string_pretty(creds)
        .map_err(|e| format!("Failed to serialize credentials: {e}"))?;

    // Atomic replace: write a sibling temp file, then rename over the target.
    // `std::fs::write` truncates in place, so a crash or a concurrent reader
    // (Claude Code, openclaude, or our own chat refresh) mid-write could
    // corrupt this shared file. rename is atomic on a single volume on both
    // Unix and Windows, keeping every reader consistent even where the advisory
    // lock is weak (it is a no-op on Windows). The pid suffix avoids collisions
    // between concurrent writers. See review H1/H2.
    let tmp = parent.join(format!(".credentials.json.tmp.{}", std::process::id()));
    if let Err(e) = write_secret_tmp(&tmp, json.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Failed to write {}: {e}", tmp.display()));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Failed to replace {}: {e}", path.display())
    })
}

/// Write `bytes` to `tmp`, tightening to `0600` before the secret lands on disk
/// (Unix) and best-effort fsyncing for durability. Atomicity is provided by the
/// caller's subsequent rename, so an fsync failure is logged, not fatal.
fn write_secret_tmp(tmp: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    if let Err(e) = f.sync_all() {
        tracing::warn!("fsync of credentials temp file failed (continuing): {e}");
    }
    Ok(())
}

/// Read and parse an existing credentials file's `claudeAiOauth` section, or
/// `None` if absent / unreadable / symlinked. Used to preserve fields the token
/// endpoint omits on a fresh login (refresh token, subscription, rate-limit).
fn read_existing_oauth(path: &std::path::Path) -> Option<ClaudeAiOauth> {
    if !path.exists()
        || path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<CredentialsFile>(&c).ok())
        .and_then(|c| c.claude_ai_oauth)
}

/// Merge a freshly obtained token with any pre-existing on-disk values.
///
/// Fields the Anthropic token endpoint does not return on a fresh login —
/// `refresh_token`, `subscriptionType`, `rateLimitTier` — fall back to the
/// existing file rather than being clobbered with `None`. Preserving the
/// refresh token in particular keeps automatic refresh working when a login
/// response omits it (review M1). Pure (no I/O) so it is unit-testable.
fn merge_oauth_fields(
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at_ms: i64,
    scopes: Vec<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    existing: Option<&ClaudeAiOauth>,
) -> ClaudeAiOauth {
    ClaudeAiOauth {
        access_token: access_token.to_string(),
        refresh_token: refresh_token
            .map(String::from)
            .or_else(|| existing.and_then(|o| o.refresh_token.clone())),
        expires_at: expires_at_ms,
        scopes,
        subscription_type: subscription_type
            .or_else(|| existing.and_then(|o| o.subscription_type.clone())),
        rate_limit_tier: rate_limit_tier
            .or_else(|| existing.and_then(|o| o.rate_limit_tier.clone())),
    }
}

/// Persist OAuth credentials to Claude Code's `~/.claude/.credentials.json`
/// store in the `claudeAiOauth` format.
///
/// This makes `OpenClaudia` a first-class *producer* of the shared credential
/// file (previously it only read it): a token obtained via `openclaudia auth`
/// is now usable by the chat/proxy paths through [`load_credentials`] without
/// Claude Code or another fork being installed.
///
/// The Anthropic token endpoint does not return `subscriptionType` /
/// `rateLimitTier`, so when the caller passes `None` for those, any values
/// already present in an existing credentials file (e.g. from a prior Claude
/// Code login) are preserved rather than clobbered.
///
/// `expires_at_ms` is milliseconds since the Unix epoch (matching
/// [`ClaudeAiOauth::expires_at`]).
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved, the target is a
/// symlink, or the file cannot be written.
pub fn store_credentials(
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at_ms: i64,
    scopes: Vec<String>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
) -> Result<(), String> {
    // Hold the advisory lock across read-merge-write so a concurrent refresh
    // (our chat path, or another OpenClaudia process) cannot interleave.
    let _lock = CredentialLock::acquire()?;
    let path = credentials_path().ok_or("Cannot determine credentials directory")?;

    // Preserve refresh token / subscription / rate-limit metadata the token
    // endpoint omits on a fresh login, rather than clobbering them with None.
    let need_existing =
        refresh_token.is_none() || subscription_type.is_none() || rate_limit_tier.is_none();
    let existing = if need_existing {
        read_existing_oauth(&path)
    } else {
        None
    };

    let merged = merge_oauth_fields(
        access_token,
        refresh_token,
        expires_at_ms,
        scopes,
        subscription_type,
        rate_limit_tier,
        existing.as_ref(),
    );
    if merged.refresh_token.is_none() {
        tracing::warn!(
            "store_credentials: login returned no refresh token and none on disk; \
             automatic token refresh will be unavailable until the next login"
        );
    }

    write_credentials_file(
        &path,
        &CredentialsFile {
            claude_ai_oauth: Some(merged),
        },
    )
}

/// A read-only snapshot of the stored Claude Code credentials, for status
/// display. Unlike [`load_credentials`], reading this does **not** trigger a
/// token refresh or any network call.
#[derive(Debug, Clone)]
pub struct CredentialStatus {
    /// Token expiry, milliseconds since the Unix epoch.
    pub expires_at_ms: i64,
    /// Whether the token is already past `expires_at_ms`.
    pub expired: bool,
    /// Whether the token is within the refresh buffer of expiry (not yet
    /// expired, but the next chat call will transparently refresh it).
    pub expires_soon: bool,
    /// Whether the token carries the `user:inference` scope required for chat.
    pub has_inference_scope: bool,
    /// Subscription tier (`pro`, `max`, …) if recorded.
    pub subscription_type: Option<String>,
}

/// Peek at the stored Claude Code credentials without refreshing them.
///
/// Returns `Ok(None)` when no credentials file (or no `claudeAiOauth` section)
/// is present. Used by `openclaudia auth --status` to report the real,
/// chat-usable credential store (the file [`load_credentials`] reads).
///
/// # Errors
///
/// Returns an error if the file exists but is a symlink, or cannot be read or
/// parsed.
pub fn peek_credentials() -> Result<Option<CredentialStatus>, String> {
    let Some(path) = credentials_path().filter(|p| p.exists()) else {
        return Ok(None);
    };
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "Credentials file {} is a symlink — refusing to read for security",
            path.display()
        ));
    }
    let content = crate::file_error::read_file(&path).map_err(|e| e.to_string())?;
    let creds: CredentialsFile = serde_json::from_str(&content)
        .map_err(crate::file_error::FileError::json_with_path(&path))
        .map_err(|e| e.to_string())?;
    let Some(oauth) = creds.claude_ai_oauth else {
        return Ok(None);
    };
    Ok(Some(status_from_oauth(
        &oauth,
        chrono::Utc::now().timestamp_millis(),
    )))
}

/// Compute a [`CredentialStatus`] from a parsed `claudeAiOauth` blob at a given
/// clock value. Pure (no I/O) so the expiry/scope logic is unit-testable.
fn status_from_oauth(oauth: &ClaudeAiOauth, now_ms: i64) -> CredentialStatus {
    CredentialStatus {
        expires_at_ms: oauth.expires_at,
        expired: now_ms >= oauth.expires_at,
        // Mirrors load_credentials' refresh trigger: within the buffer, the
        // next chat call will transparently refresh.
        expires_soon: now_ms < oauth.expires_at && now_ms + REFRESH_BUFFER_MS >= oauth.expires_at,
        has_inference_scope: oauth.scopes.iter().any(|s| s == "user:inference"),
        subscription_type: oauth.subscription_type.clone(),
    }
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
///
/// # Crosslink #923 — why this constant lives here (deliberate coupling)
///
/// The QA review flagged this constant as a decoupling violation: a
/// `claude_credentials` module injects content into the system prompt the
/// prompt-builder is unaware of, and the literal string couples
/// `OpenClaudia`'s identity attestation to a specific Anthropic policy.
///
/// We have accepted the feedback but kept the current shape, for the
/// following reasons:
///
/// 1. **The string IS an OAuth credential.** The Anthropic OAuth endpoint
///    refuses requests whose first system block does not contain exactly
///    this literal. The string is therefore part of the OAuth contract
///    (alongside the bearer token and `anthropic-beta` header), not a
///    free-form prompt fragment, and so belongs in the credentials module
///    that owns the rest of that contract.
/// 2. **Single source of truth.** Both `inject_system_prompt` (full chat
///    mode) and `inject_oauth_prefix_only` (proxy mode) reference the same
///    constant; moving the literal into `prompt.rs` would split the OAuth
///    contract across two crates with no compile-time link between them.
/// 3. **Operational risk is bounded.** If Anthropic changes the literal,
///    the failure mode is a 401 from `/v1/messages` with a clear server
///    message ("invalid system prefix") — not a silent degradation.
///    Updating the constant is a one-line fix in one file.
///
/// The follow-up work to move OAuth prefix-block construction into
/// `build_system_prompt_blocks(..., oauth_prefix: Option<&str>)` is
/// tracked in the same issue thread but is deferred because it would
/// require threading the credential state through every prompt-builder
/// callsite without changing what the wire actually carries.
pub const CLAUDE_CODE_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Additional system prompt content sent as a separate block after the prefix.
/// This is where behavioral instructions and persona go.
pub const CLAUDIA_SYSTEM_PROMPT: &str = include_str!("claude_code_prompt.txt");

/// Inject only the Claude Code prefix block required for OAuth tokens.
///
/// Block 0: The exact one-liner prefix (API validates this string for OAuth)
/// Block 1+: Whatever was already in the system field (preserved as-is)
///
/// Unlike [`inject_system_prompt`], this does NOT prepend the Claudia
/// behavioral persona — it is the minimum mutation required for the
/// Anthropic API to accept an OAuth Bearer request, and is used by the
/// `/v1/messages` proxy endpoint where the caller (an arbitrary
/// Anthropic SDK client) owns its own system prompt content.
///
/// Centralized here so that the magic-string prefix and the three-way
/// match on the existing `system` shape live in one place. Previously
/// inlined into `proxy::proxy_anthropic_messages` — see crosslink #386.
pub fn inject_oauth_prefix_only(request: &mut serde_json::Value) {
    let prefix_block = serde_json::json!({
        "type": "text",
        "text": CLAUDE_CODE_SYSTEM_PROMPT,
    });

    match request.get_mut("system") {
        Some(serde_json::Value::Array(arr)) => {
            arr.insert(0, prefix_block);
        }
        Some(serde_json::Value::String(existing)) => {
            let existing_obj = serde_json::json!({
                "type": "text",
                "text": existing.clone(),
            });
            request["system"] = serde_json::json!([prefix_block, existing_obj]);
        }
        _ => {
            request["system"] = serde_json::json!([prefix_block]);
        }
    }
}

/// Maximum recursion depth for [`strip_cache_control_ttl`].
///
/// Matches the cap used by `hooks::merge::deep_merge` (crosslink #333).
/// Realistic Anthropic Messages API request bodies bottom out at <10
/// levels of nesting (system / messages / content blocks / tool inputs);
/// 32 leaves ample headroom while preventing a hostile request body
/// from blowing the stack via unbounded JSON nesting (crosslink #805).
pub(crate) const MAX_STRIP_DEPTH: usize = 32;

/// Recursively strip `ttl` from any `cache_control` objects in a JSON
/// value.
///
/// The Anthropic Messages API rejects `cache_control.ttl` when the
/// request is authenticated with an OAuth Bearer token (the field is
/// only legal under `x-api-key` auth on accounts with the appropriate
/// entitlement). Co-located with [`inject_oauth_prefix_only`] because
/// the two are co-requisites of every OAuth-authenticated request —
/// see crosslink #386.
///
/// Recursion is capped at [`MAX_STRIP_DEPTH`] levels. A hostile request
/// body containing thousands of nested arrays or objects would
/// otherwise overflow the stack before `serde_json` itself bailed
/// (crosslink #805). On reaching the cap we emit a `warn!` with the
/// JSON path that triggered the cutoff and stop recursing into that
/// subtree; any `cache_control.ttl` deeper than the cap is left in
/// place, which the upstream API will reject with a 400 — strictly
/// safer than crashing the proxy.
pub fn strip_cache_control_ttl(value: &mut serde_json::Value) {
    strip_cache_control_ttl_inner(value, 0, "$");
}

fn strip_cache_control_ttl_inner(value: &mut serde_json::Value, depth: usize, path: &str) {
    if depth >= MAX_STRIP_DEPTH {
        tracing::warn!(
            path = %path,
            limit = MAX_STRIP_DEPTH,
            "strip_cache_control_ttl depth cap reached; refusing to recurse further (crosslink #805)",
        );
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(cc_map)) = map.get_mut("cache_control") {
                cc_map.remove("ttl");
            }
            for (k, v) in map.iter_mut() {
                let child_path = format!("{path}.{k}");
                strip_cache_control_ttl_inner(v, depth + 1, &child_path);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter_mut().enumerate() {
                let child_path = format!("{path}[{i}]");
                strip_cache_control_ttl_inner(v, depth + 1, &child_path);
            }
        }
        _ => {}
    }
}

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

    // --- Crosslink #825: refresh_token rotation policy ---

    /// Spec — when the OAuth response carries a `refresh_token`, the helper
    /// returns it verbatim (this is the rotating-token happy path). This
    /// path doesn't touch env vars so it can run in parallel with anything.
    #[test]
    fn resolve_new_refresh_token_uses_response_field_when_present() {
        let result = resolve_new_refresh_token(Some("new-rotated-token"), "old-token");
        assert_eq!(result.as_deref(), Ok("new-rotated-token"));
    }

    /// Spec — both env-var-sensitive branches of [`resolve_new_refresh_token`]
    /// folded into one test so they cannot race each other or other tests in
    /// this binary on the shared `OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE` slot.
    /// Saves and restores the ambient value on the way in and out so a parent
    /// process that legitimately set the var observes no side effect.
    #[test]
    fn resolve_new_refresh_token_optin_policy() {
        const VAR: &str = "OPENCLAUDIA_ALLOW_REFRESH_TOKEN_REUSE";
        let prev = std::env::var(VAR).ok();
        // SAFETY (both `unsafe` calls below): this single test owns the env
        // var for its duration — the only other site that reads it is the
        // production code under test, called synchronously from here. No
        // background thread in this binary writes to this var.
        unsafe {
            std::env::remove_var(VAR);
        }
        let err_result = resolve_new_refresh_token(None, "old-token");
        // SAFETY: see comment above.
        unsafe {
            std::env::set_var(VAR, "1");
        }
        let reuse_result = resolve_new_refresh_token(None, "old-token");
        // Restore before any assertion that might unwind.
        // SAFETY: see comment above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var(VAR, v),
                None => std::env::remove_var(VAR),
            }
        }

        let err = err_result.expect_err("must refuse silent reuse without explicit opt-in");
        assert!(
            err.contains(VAR),
            "error must name the opt-in env var; got: {err}"
        );
        assert_eq!(
            reuse_result.as_deref(),
            Ok("old-token"),
            "with opt-in set, helper must return the previous token"
        );
    }

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

    #[test]
    fn write_credentials_file_round_trips_with_claude_code_keys() {
        // `store_credentials` and `refresh_and_load` both serialize through
        // `write_credentials_file`; this pins the on-disk shape to Claude
        // Code's camelCase keys so the file stays readable by `load_credentials`,
        // by Claude Code itself, and by forks like @gitlawb/openclaude. Uses a
        // temp path — never the real ~/.claude.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".credentials.json");
        let creds = CredentialsFile {
            claude_ai_oauth: Some(ClaudeAiOauth {
                access_token: "sk-ant-oat01-TESTTOKEN".to_string(),
                refresh_token: Some("sk-ant-ort01-REFRESH".to_string()),
                expires_at: 1_999_999_999_999,
                scopes: vec!["user:inference".to_string(), "user:profile".to_string()],
                subscription_type: Some("pro".to_string()),
                rate_limit_tier: Some("default_claude_ai".to_string()),
            }),
        };

        write_credentials_file(&path, &creds).expect("write should succeed");

        let content = std::fs::read_to_string(&path).expect("file should exist");
        // Canonical Claude Code key names — drift here breaks interop.
        assert!(
            content.contains("\"claudeAiOauth\""),
            "missing claudeAiOauth: {content}"
        );
        assert!(
            content.contains("\"accessToken\""),
            "missing accessToken: {content}"
        );
        assert!(
            content.contains("\"expiresAt\""),
            "missing expiresAt: {content}"
        );

        let parsed: CredentialsFile = serde_json::from_str(&content).expect("parse");
        let oauth = parsed.claude_ai_oauth.expect("oauth section round-trips");
        assert_eq!(oauth.access_token, "sk-ant-oat01-TESTTOKEN");
        assert_eq!(oauth.expires_at, 1_999_999_999_999);
        assert_eq!(oauth.subscription_type.as_deref(), Some("pro"));
        assert!(oauth.scopes.iter().any(|s| s == "user:inference"));
    }

    #[test]
    fn merge_oauth_fields_preserves_refresh_token_when_login_omits_it() {
        // review M1: a login response without a refresh_token must NOT wipe the
        // existing one, or automatic refresh silently breaks on next expiry.
        let existing = ClaudeAiOauth {
            access_token: "old".into(),
            refresh_token: Some("keep-me".into()),
            expires_at: 1,
            scopes: vec![],
            subscription_type: Some("max".into()),
            rate_limit_tier: Some("tier-x".into()),
        };
        let merged = merge_oauth_fields(
            "new-access",
            None, // login returned no refresh token
            42,
            vec!["user:inference".into()],
            None, // no subscription from token endpoint
            None, // no rate limit from token endpoint
            Some(&existing),
        );
        assert_eq!(merged.access_token, "new-access");
        assert_eq!(merged.expires_at, 42);
        assert_eq!(merged.refresh_token.as_deref(), Some("keep-me"));
        assert_eq!(merged.subscription_type.as_deref(), Some("max"));
        assert_eq!(merged.rate_limit_tier.as_deref(), Some("tier-x"));
    }

    #[test]
    fn merge_oauth_fields_prefers_fresh_values_over_existing() {
        let existing = ClaudeAiOauth {
            access_token: "old".into(),
            refresh_token: Some("old-refresh".into()),
            expires_at: 1,
            scopes: vec![],
            subscription_type: Some("pro".into()),
            rate_limit_tier: None,
        };
        let merged = merge_oauth_fields(
            "new-access",
            Some("new-refresh"),
            99,
            vec!["user:inference".into()],
            Some("max".into()),
            Some("tier-new".into()),
            Some(&existing),
        );
        assert_eq!(merged.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(merged.subscription_type.as_deref(), Some("max"));
        assert_eq!(merged.rate_limit_tier.as_deref(), Some("tier-new"));
    }

    #[test]
    fn merge_oauth_fields_without_existing_keeps_none() {
        let merged = merge_oauth_fields("a", None, 7, vec![], None, None, None);
        assert!(merged.refresh_token.is_none());
        assert!(merged.subscription_type.is_none());
        assert!(merged.rate_limit_tier.is_none());
    }

    #[test]
    fn status_from_oauth_flags_expiry_and_scope() {
        let base = ClaudeAiOauth {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: 0,
            scopes: vec!["user:inference".into(), "user:profile".into()],
            subscription_type: Some("pro".into()),
            rate_limit_tier: None,
        };

        // Valid, far from expiry.
        let far = ClaudeAiOauth {
            expires_at: REFRESH_BUFFER_MS * 100,
            ..base.clone()
        };
        let s = status_from_oauth(&far, 0);
        assert!(!s.expired && !s.expires_soon && s.has_inference_scope);
        assert_eq!(s.subscription_type.as_deref(), Some("pro"));

        // Already expired.
        let expired = ClaudeAiOauth {
            expires_at: 100,
            ..base.clone()
        };
        let s = status_from_oauth(&expired, 200);
        assert!(s.expired && !s.expires_soon);

        // Within the refresh buffer (not yet expired).
        let soon = ClaudeAiOauth {
            expires_at: REFRESH_BUFFER_MS,
            ..base.clone()
        };
        let s = status_from_oauth(&soon, 1);
        assert!(!s.expired && s.expires_soon);

        // Missing inference scope.
        let no_inf = ClaudeAiOauth {
            scopes: vec!["user:profile".into()],
            expires_at: REFRESH_BUFFER_MS * 100,
            ..base
        };
        let s = status_from_oauth(&no_inf, 0);
        assert!(!s.has_inference_scope);
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

    // --- Regression guards for crosslink #386: decomposition of
    // proxy_anthropic_messages. These tests pin the wire-level behavior
    // that was previously inlined into the proxy handler, so any future
    // edit to the helpers preserves what subscriber clients observe.

    /// Spec — `inject_oauth_prefix_only` prepends the exact prefix block
    /// when `system` is already an array (preserves existing blocks).
    #[test]
    fn inject_oauth_prefix_only_prepends_to_array() {
        let mut req = serde_json::json!({
            "system": [{"type": "text", "text": "user-provided"}]
        });
        inject_oauth_prefix_only(&mut req);
        let arr = req["system"].as_array().expect("system must be array");
        assert_eq!(arr.len(), 2, "must prepend exactly one block");
        assert_eq!(arr[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(arr[1]["text"], "user-provided");
    }

    /// Spec — `inject_oauth_prefix_only` upgrades a string `system` to a
    /// two-block array (prefix, then the original string).
    #[test]
    fn inject_oauth_prefix_only_upgrades_string() {
        let mut req = serde_json::json!({"system": "you are helpful"});
        inject_oauth_prefix_only(&mut req);
        let arr = req["system"].as_array().expect("system must be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(arr[1]["text"], "you are helpful");
    }

    /// Spec — `inject_oauth_prefix_only` creates a one-block array when
    /// `system` is missing entirely.
    #[test]
    fn inject_oauth_prefix_only_creates_when_absent() {
        let mut req = serde_json::json!({});
        inject_oauth_prefix_only(&mut req);
        let arr = req["system"].as_array().expect("system must be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
    }

    /// Spec — `inject_oauth_prefix_only` does NOT inject the Claudia
    /// behavioral persona block. That belongs to `inject_system_prompt`
    /// for the CLI client, not to the proxy's pass-through behavior.
    #[test]
    fn inject_oauth_prefix_only_does_not_add_behavioral_block() {
        let mut req = serde_json::json!({});
        inject_oauth_prefix_only(&mut req);
        let arr = req["system"].as_array().expect("system must be array");
        assert_eq!(arr.len(), 1, "must be prefix-only, not prefix+behavioral");
    }

    /// Spec — `strip_cache_control_ttl` removes `ttl` from nested
    /// `cache_control` objects (the OAuth API rejects TTL).
    #[test]
    fn strip_cache_control_ttl_removes_nested_ttl() {
        let mut value = serde_json::json!({
            "system": [
                {
                    "type": "text",
                    "text": "hello",
                    "cache_control": { "type": "ephemeral", "ttl": 3600 }
                }
            ]
        });
        strip_cache_control_ttl(&mut value);
        let cc = &value["system"][0]["cache_control"];
        assert_eq!(cc["type"], "ephemeral", "type must be preserved");
        assert!(
            cc.get("ttl").is_none(),
            "ttl must be stripped from cache_control"
        );
    }

    /// Spec — `strip_cache_control_ttl` is a no-op when no `ttl` is
    /// present.
    #[test]
    fn strip_cache_control_ttl_noop_when_no_ttl() {
        let mut value = serde_json::json!({
            "cache_control": { "type": "ephemeral" }
        });
        strip_cache_control_ttl(&mut value);
        assert_eq!(value["cache_control"]["type"], "ephemeral");
    }

    // ────────────────────────────────────────────────────────────────
    // Regression tests for crosslink #805: unbounded recursion in
    // `strip_cache_control_ttl` would let a hostile request body
    // (deeply nested objects or arrays) blow the stack. The fix caps
    // recursion at MAX_STRIP_DEPTH levels.
    // ────────────────────────────────────────────────────────────────

    /// A 1000-level nested array would previously recurse 1000 frames
    /// deep and could overflow the stack on smaller-stack platforms.
    /// With the cap, the call returns cleanly without panicking.
    #[test]
    fn strip_cache_control_ttl_rejects_1000_level_nesting_without_stack_overflow() {
        // Build [[[…[]…]]] 1000 levels deep.
        let mut value = serde_json::Value::Array(Vec::new());
        for _ in 0..1000u16 {
            value = serde_json::Value::Array(vec![value]);
        }
        // Must not panic / stack-overflow.
        strip_cache_control_ttl(&mut value);
    }

    /// At the depth cap, anything beyond is intentionally not visited
    /// — so a `cache_control.ttl` planted at depth > cap survives
    /// (and the API will 400, which is strictly safer than crashing).
    #[test]
    fn strip_cache_control_ttl_does_not_visit_past_depth_cap() {
        // Wrap a cache_control object inside MAX_STRIP_DEPTH + 5 arrays.
        let payload = serde_json::json!({
            "cache_control": { "type": "ephemeral", "ttl": 3600 }
        });
        let mut value = payload;
        for _ in 0..(MAX_STRIP_DEPTH + 5) {
            value = serde_json::Value::Array(vec![value]);
        }

        strip_cache_control_ttl(&mut value);

        // Unwrap back down to find the inner cache_control.
        let mut cursor = &value;
        while let Some(arr) = cursor.as_array() {
            if arr.is_empty() {
                break;
            }
            cursor = &arr[0];
        }
        // The ttl beyond the cap MUST still be present — proving the
        // cap actually stopped recursion (and that the function did
        // not silently rewrite arbitrary depth without bound). The
        // ttl lives inside `cache_control`, not at the top-level
        // cursor — we are testing that the cap prevented the
        // descent into the object that contains it.
        let cc = cursor
            .get("cache_control")
            .expect("cache_control object survives wrapping");
        let ttl = cc.get("ttl");
        assert!(
            ttl.is_some(),
            "ttl beyond depth cap should be left intact (cap stopped recursion), got cc={cc:?}",
        );
    }

    /// Just *under* the cap, the strip still happens — proving the
    /// cap is permissive enough for realistic request shapes. A real
    /// Anthropic Messages API request bottoms out at ~10 levels
    /// (system / messages / content blocks / tool inputs), so a 16-
    /// level test is comfortably realistic and well under the 32 cap.
    #[test]
    fn strip_cache_control_ttl_strips_within_depth_cap() {
        let mut inner = serde_json::json!({
            "cache_control": { "type": "ephemeral", "ttl": 3600 }
        });
        // Wrap in 16 layers of arrays — well under MAX_STRIP_DEPTH = 32.
        for _ in 0..16 {
            inner = serde_json::Value::Array(vec![inner]);
        }

        strip_cache_control_ttl(&mut inner);

        // Unwrap down to the cache_control object.
        let mut cursor = &inner;
        while let Some(arr) = cursor.as_array() {
            cursor = &arr[0];
        }
        let cc = cursor.get("cache_control").expect("cache_control survives");
        assert_eq!(cc["type"], "ephemeral");
        assert!(
            cc.get("ttl").is_none(),
            "ttl within depth cap MUST be stripped, got cc={cc:?}",
        );
    }

    /// Depth cap is exactly `MAX_STRIP_DEPTH` (boundary pin). At depth
    /// `MAX_STRIP_DEPTH - 1` we still descend; at `MAX_STRIP_DEPTH`
    /// we don't. A `cache_control` at *exactly* the cap depth survives
    /// (because depth incremented before the descend).
    #[test]
    fn strip_cache_control_ttl_depth_cap_boundary() {
        // 31 wraps means the inner `cache_control` object is visited
        // at depth = 31 (the loop increments once per array
        // descent), which is < MAX_STRIP_DEPTH (32) — so it strips.
        let mut value = serde_json::json!({
            "cache_control": { "type": "ephemeral", "ttl": 1 }
        });
        for _ in 0..(MAX_STRIP_DEPTH - 1) {
            value = serde_json::Value::Array(vec![value]);
        }
        strip_cache_control_ttl(&mut value);
        let mut cursor = &value;
        while let Some(arr) = cursor.as_array() {
            cursor = &arr[0];
        }
        assert!(
            cursor["cache_control"].get("ttl").is_none(),
            "ttl just under the cap must be stripped"
        );
    }
}
