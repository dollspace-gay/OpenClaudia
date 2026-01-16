//! OAuth 2.0 Device Flow Authentication for Claude Max subscriptions
//!
//! Enables OpenClaudia to authenticate using Claude Pro/Max subscriptions
//! via OAuth 2.0 device authorization flow with PKCE.
//!
//! ## Flow Overview
//! 1. Generate PKCE challenge and authorization URL
//! 2. User visits URL, authenticates with Claude, receives code
//! 3. Exchange code for access/refresh tokens
//! 4. Use Bearer token with OAuth beta header for API requests
//!
//! ## Important Notes
//! - Requires Claude Pro or Max subscription
//! - Access tokens expire, auto-refresh supported
//! - System prompt injection required for OAuth tokens

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use tracing::{debug, error, info};

/// Anthropic's fixed OAuth client identifier
pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Fixed redirect URI for Anthropic OAuth
pub const ANTHROPIC_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

/// OAuth authorization endpoint for personal Claude Max accounts
/// Use claude.ai for personal Max subscribers, console.anthropic.com for org accounts
pub const OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// Token exchange endpoint
pub const TOKEN_ENDPOINT: &str = "https://console.anthropic.com/v1/oauth/token";

/// API key creation endpoint - creates ephemeral API key from OAuth token
pub const API_KEY_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/claude_cli/create_api_key";

/// OAuth scopes required for API access
/// Must include user:sessions:claude_code to get org:create_api_key permission
pub const OAUTH_SCOPES: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code";

// ============================================================================
// PKCE (Proof Key for Code Exchange) Implementation
// ============================================================================

/// PKCE parameters for secure OAuth flow
#[derive(Debug, Clone)]
pub struct PkceParams {
    /// Random verifier string (kept secret, sent during token exchange)
    pub verifier: String,
    /// SHA256 hash of verifier (sent during authorization)
    pub challenge: String,
    /// Random state for CSRF protection
    pub state: String,
}

impl PkceParams {
    /// Generate new PKCE parameters with cryptographically secure randomness
    pub fn generate() -> Self {
        let verifier = generate_random_string(64);
        let challenge = compute_s256_challenge(&verifier);
        let state = generate_random_string(64);

        Self {
            verifier,
            challenge,
            state,
        }
    }

    /// Build the full authorization URL with all required parameters
    pub fn build_auth_url(&self) -> String {
        let params = [
            ("code", "true"),
            ("client_id", ANTHROPIC_CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", ANTHROPIC_REDIRECT_URI),
            ("scope", OAUTH_SCOPES),
            ("code_challenge", &self.challenge),
            ("code_challenge_method", "S256"),
            ("state", &self.state),
        ];

        let query = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", OAUTH_AUTHORIZE_URL, query)
    }
}

/// Generate a cryptographically secure random string (base64url encoded)
fn generate_random_string(byte_length: usize) -> String {
    let mut bytes = vec![0u8; byte_length];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(&bytes)
}

/// Compute S256 challenge from verifier (SHA256 + base64url)
fn compute_s256_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

// ============================================================================
// OAuth Token Types
// ============================================================================

/// OAuth token pair with expiration tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    /// Bearer access token for API requests
    pub access_token: String,
    /// Refresh token for obtaining new access tokens
    pub refresh_token: Option<String>,
    /// When the access token expires
    pub expires_at: DateTime<Utc>,
}

impl OAuthCredentials {
    /// Check if token is completely expired
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }
}

/// Request body for token endpoint
#[derive(Debug, Serialize)]
pub struct TokenExchangeRequest {
    pub grant_type: String,
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Response from token endpoint
#[derive(Debug, Deserialize)]
pub struct TokenExchangeResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
}

// ============================================================================
// OAuth Session Management
// ============================================================================

/// Authentication mode for API calls
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AuthMode {
    /// Use ephemeral API key (x-api-key header) - for org accounts with org:create_api_key
    ApiKey,
    /// Use Bearer token directly (Authorization: Bearer) - for personal Max accounts
    BearerToken,
    /// Use anthropic-proxy with session cookie - simplest mode that actually works
    ProxyMode,
}

/// Active OAuth session with credentials and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthSession {
    /// Session identifier (used as pseudo API key)
    pub id: String,
    /// OAuth credentials
    pub credentials: OAuthCredentials,
    /// Ephemeral API key created from OAuth token (used for actual API calls)
    pub api_key: Option<String>,
    /// Authentication mode for API calls
    pub auth_mode: AuthMode,
    /// Scopes that were actually granted by OAuth server
    pub granted_scopes: Vec<String>,
    /// When session was created
    pub created_at: DateTime<Utc>,
    /// Optional user identifier
    pub user_id: Option<String>,
}

impl OAuthSession {
    /// Create new session from token response
    pub fn from_token_response(response: TokenExchangeResponse) -> Self {
        // Parse granted scopes from response
        let granted_scopes: Vec<String> = response
            .scope
            .as_ref()
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        // Determine initial auth mode based on granted scopes
        // If we have org:create_api_key, we'll try API key mode
        // Otherwise, fall back to Bearer token mode
        let has_api_key_scope = granted_scopes.iter().any(|s| s == "org:create_api_key");
        let auth_mode = if has_api_key_scope {
            AuthMode::ApiKey
        } else {
            AuthMode::BearerToken
        };

        if auth_mode == AuthMode::BearerToken {
            info!(
                "Personal account detected (no org:create_api_key scope) - using Bearer token auth"
            );
        }

        Self {
            id: uuid::Uuid::new_v4().to_string(),
            credentials: OAuthCredentials {
                access_token: response.access_token,
                refresh_token: response.refresh_token,
                expires_at: Utc::now() + Duration::seconds(response.expires_in as i64),
            },
            api_key: None, // Set after calling create_api_key if auth_mode is ApiKey
            auth_mode,
            granted_scopes,
            created_at: Utc::now(),
            user_id: None,
        }
    }

    /// Check if this session can create API keys
    pub fn can_create_api_key(&self) -> bool {
        self.granted_scopes
            .iter()
            .any(|s| s == "org:create_api_key")
    }
}

/// Thread-safe storage for OAuth sessions and pending PKCE challenges
pub struct OAuthStore {
    /// Active sessions keyed by session ID
    sessions: RwLock<HashMap<String, OAuthSession>>,
    /// Pending PKCE challenges keyed by state parameter
    pending_challenges: RwLock<HashMap<String, PkceParams>>,
    /// Path for persistent session storage
    persist_path: Option<PathBuf>,
}

impl Default for OAuthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthStore {
    /// Create new OAuth store with optional persistence
    pub fn new() -> Self {
        let persist_path =
            dirs::data_local_dir().map(|d| d.join("openclaudia").join("oauth_sessions.json"));

        let store = Self {
            sessions: RwLock::new(HashMap::new()),
            pending_challenges: RwLock::new(HashMap::new()),
            persist_path: persist_path.clone(),
        };

        // Load persisted sessions
        if persist_path.is_some() {
            store.load_from_disk();
        }

        store
    }

    /// Store PKCE challenge for pending authorization
    pub fn store_challenge(&self, pkce: PkceParams) {
        let state = pkce.state.clone();
        let mut challenges = self.pending_challenges.write().unwrap();
        challenges.insert(state, pkce);
    }

    /// Retrieve and remove PKCE challenge by state
    pub fn take_challenge(&self, state: &str) -> Option<PkceParams> {
        let mut challenges = self.pending_challenges.write().unwrap();
        challenges.remove(state)
    }

    /// Store new OAuth session
    pub fn store_session(&self, session: OAuthSession) {
        let id = session.id.clone();
        {
            let mut sessions = self.sessions.write().unwrap();
            sessions.insert(id.clone(), session);
        }
        self.persist_to_disk();
        info!("OAuth session stored: {}", id);
    }

    /// Retrieve session by ID
    pub fn get_session(&self, id: &str) -> Option<OAuthSession> {
        let sessions = self.sessions.read().unwrap();
        sessions.get(id).cloned()
    }

    /// Get any valid (non-expired) session - used when no specific session ID is provided
    pub fn get_any_valid_session(&self) -> Option<OAuthSession> {
        let sessions = self.sessions.read().unwrap();
        for (id, session) in sessions.iter() {
            let expired = session.credentials.is_expired();
            tracing::debug!(
                "Session {}: expired={}, expires_at={:?}",
                id,
                expired,
                session.credentials.expires_at
            );
        }
        sessions
            .values()
            .find(|s| !s.credentials.is_expired())
            .cloned()
    }

    /// Load sessions from disk, filtering out expired ones
    fn load_from_disk(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };

        match fs::read_to_string(path) {
            Ok(data) => {
                if let Ok(loaded) = serde_json::from_str::<HashMap<String, OAuthSession>>(&data) {
                    // Filter out expired sessions during load
                    let valid_sessions: HashMap<String, OAuthSession> = loaded
                        .into_iter()
                        .filter(|(id, session)| {
                            if session.credentials.is_expired() {
                                info!("Removing expired OAuth session: {}", id);
                                false
                            } else {
                                true
                            }
                        })
                        .collect();

                    let mut sessions = self.sessions.write().unwrap();
                    *sessions = valid_sessions;
                    info!("Loaded {} OAuth sessions from disk", sessions.len());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("No persisted OAuth sessions found");
            }
            Err(e) => {
                error!("Failed to load OAuth sessions: {}", e);
            }
        }
    }

    /// Persist sessions to disk
    fn persist_to_disk(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let sessions = self.sessions.read().unwrap();
        match serde_json::to_string_pretty(&*sessions) {
            Ok(json) => {
                if let Err(e) = fs::write(path, json) {
                    error!("Failed to persist OAuth sessions: {}", e);
                }
            }
            Err(e) => {
                error!("Failed to serialize OAuth sessions: {}", e);
            }
        }
    }
}

// ============================================================================
// OAuth Client for Token Operations
// ============================================================================

/// Client for OAuth token operations
pub struct OAuthClient {
    http: reqwest::Client,
}

impl Default for OAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthClient {
    pub fn new() -> Self {
        // Build client with Claude Code User-Agent (critical for OAuth)
        let http = reqwest::Client::builder()
            .user_agent("Claude Code/1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { http }
    }

    /// Exchange authorization code for tokens
    ///
    /// NOTE: This performs an immediate token refresh after initial exchange,
    /// which is required for the tokens to work with the API. The initial tokens
    /// from the authorization code exchange may not be valid for API use.
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce: &PkceParams,
    ) -> Result<TokenExchangeResponse> {
        let request = TokenExchangeRequest {
            grant_type: "authorization_code".to_string(),
            client_id: ANTHROPIC_CLIENT_ID.to_string(),
            code: Some(code.to_string()),
            redirect_uri: Some(ANTHROPIC_REDIRECT_URI.to_string()),
            code_verifier: Some(pkce.verifier.clone()),
            refresh_token: None,
            state: Some(pkce.state.clone()),
        };

        let initial_response = self.send_token_request(request).await?;

        // CRITICAL: Immediate token refresh after initial exchange
        // The anthropic-proxy discovered that initial tokens may not be valid for API use
        // Refreshing immediately gives us tokens that work
        info!("Initial token obtained, attempting immediate refresh...");

        if let Some(ref refresh_token) = initial_response.refresh_token {
            match self.refresh_token(refresh_token).await {
                Ok(refreshed) => {
                    info!("✅ Immediate token refresh successful!");
                    // Return refreshed tokens, keeping original refresh_token if not returned
                    Ok(TokenExchangeResponse {
                        access_token: refreshed.access_token,
                        token_type: refreshed.token_type,
                        expires_in: refreshed.expires_in,
                        refresh_token: refreshed.refresh_token.or(initial_response.refresh_token),
                        scope: refreshed.scope.or(initial_response.scope),
                    })
                }
                Err(e) => {
                    tracing::warn!(
                        "Immediate token refresh failed: {:?}, using original tokens",
                        e
                    );
                    Ok(initial_response)
                }
            }
        } else {
            tracing::warn!("No refresh token in initial response, using original tokens");
            Ok(initial_response)
        }
    }

    /// Refresh access token using refresh token
    pub async fn refresh_token(&self, refresh_token: &str) -> Result<TokenExchangeResponse> {
        let request = TokenExchangeRequest {
            grant_type: "refresh_token".to_string(),
            client_id: ANTHROPIC_CLIENT_ID.to_string(),
            code: None,
            redirect_uri: None,
            code_verifier: None,
            refresh_token: Some(refresh_token.to_string()),
            state: None,
        };

        self.send_token_request(request).await
    }

    /// Send token request to Anthropic
    async fn send_token_request(
        &self,
        request: TokenExchangeRequest,
    ) -> Result<TokenExchangeResponse> {
        debug!("Sending token request to {}", TOKEN_ENDPOINT);

        // CRITICAL: Anthropic's OAuth endpoint requires form-urlencoded, NOT JSON
        // This is the key difference that makes anthropic-proxy work
        let response = self
            .http
            .post(TOKEN_ENDPOINT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&request)
            .send()
            .await
            .context("Failed to send token request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed ({}): {}", status, body);
        }

        let body = response
            .text()
            .await
            .context("Failed to read token response")?;

        debug!("Token response received");

        let token_response: TokenExchangeResponse =
            serde_json::from_str(&body).context("Failed to parse token response")?;

        // Validate token type is Bearer
        if token_response.token_type.to_lowercase() != "bearer" {
            anyhow::bail!(
                "Unexpected token type '{}', expected 'Bearer'",
                token_response.token_type
            );
        }

        // Log granted scopes (important for debugging permission issues)
        if let Some(ref scope) = token_response.scope {
            info!("OAuth granted scopes: {}", scope);
        } else {
            info!("OAuth response did not include scope field");
        }

        Ok(token_response)
    }

    /// Create an ephemeral API key from OAuth access token
    ///
    /// Claude Code uses this to convert OAuth tokens into API keys for actual
    /// API calls, since the /v1/messages endpoint doesn't support OAuth directly.
    pub async fn create_api_key(&self, access_token: &str) -> Result<String> {
        debug!("Creating API key from OAuth token at {}", API_KEY_ENDPOINT);

        // Claude Code sends null body with just Authorization header
        let response = self
            .http
            .post(API_KEY_ENDPOINT)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .context("Failed to send API key creation request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API key creation failed ({}): {}", status, body);
        }

        let body = response
            .text()
            .await
            .context("Failed to read API key response")?;

        // Parse the response to get the raw_key
        #[derive(Deserialize)]
        struct ApiKeyResponse {
            raw_key: String,
        }

        let key_response: ApiKeyResponse =
            serde_json::from_str(&body).context("Failed to parse API key response")?;

        info!("Successfully created API key from OAuth token");
        Ok(key_response.raw_key)
    }
}

// ============================================================================
// Authorization Code Parsing
// ============================================================================

/// Parse authorization code from Claude's combined format
///
/// Claude returns the code as: `{authorization_code}#{state}`
pub fn parse_auth_code(input: &str) -> (String, Option<String>) {
    if let Some(idx) = input.find('#') {
        let code = input[..idx].to_string();
        let state = input[idx + 1..].to_string();
        (code, Some(state))
    } else {
        (input.to_string(), None)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_generation() {
        let pkce = PkceParams::generate();

        // Verifier should be base64url encoded 64 bytes
        assert!(!pkce.verifier.is_empty());
        assert!(!pkce.challenge.is_empty());
        assert!(!pkce.state.is_empty());

        // Challenge should be different from verifier
        assert_ne!(pkce.verifier, pkce.challenge);
    }

    #[test]
    fn test_s256_challenge() {
        // Known test vector
        let verifier = "test_verifier";
        let challenge = compute_s256_challenge(verifier);

        // Should be consistent
        assert_eq!(challenge, compute_s256_challenge(verifier));
    }

    #[test]
    fn test_auth_url_construction() {
        let pkce = PkceParams::generate();
        let url = pkce.build_auth_url();

        assert!(url.starts_with(OAUTH_AUTHORIZE_URL));
        assert!(url.contains("client_id="));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("state="));
    }

    #[test]
    fn test_parse_auth_code_combined() {
        let input = "auth_code_123#state_abc";
        let (code, state) = parse_auth_code(input);

        assert_eq!(code, "auth_code_123");
        assert_eq!(state, Some("state_abc".to_string()));
    }

    #[test]
    fn test_parse_auth_code_simple() {
        let input = "just_a_code";
        let (code, state) = parse_auth_code(input);

        assert_eq!(code, "just_a_code");
        assert_eq!(state, None);
    }

    #[test]
    fn test_token_expiry_check() {
        let creds = OAuthCredentials {
            access_token: "test".to_string(),
            refresh_token: None,
            expires_at: Utc::now() + Duration::seconds(100),
        };

        // 100 seconds remaining - not expired
        assert!(!creds.is_expired());

        let expired_creds = OAuthCredentials {
            access_token: "test".to_string(),
            refresh_token: None,
            expires_at: Utc::now() - Duration::seconds(10),
        };

        // Already past expiry
        assert!(expired_creds.is_expired());
    }
}
