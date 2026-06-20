//! Codex credential discovery for OpenAI auth.
//!
//! Codex stores login material in `$CODEX_HOME/auth.json` (or
//! `~/.codex/auth.json`) with the same logical shape as its open-source Rust
//! client. We read only enough of that shape to reuse supported credentials:
//! OpenAI API keys can feed the existing Chat Completions path, while ChatGPT
//! and Codex personal access tokens must use the Responses backend.

use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";
pub const CODEX_ACCESS_TOKEN_ENV_VAR: &str = "CODEX_ACCESS_TOKEN";
pub const CODEX_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthMode {
    ApiKey,
    Chatgpt,
    ChatgptAuthTokens,
    AgentIdentity,
    PersonalAccessToken,
    BedrockApiKey,
}

impl CodexAuthMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "api_key" | "apiKey" | "apikey" => Some(Self::ApiKey),
            "chatgpt" => Some(Self::Chatgpt),
            "chatgpt_auth_tokens" | "chatgptAuthTokens" => Some(Self::ChatgptAuthTokens),
            "agent_identity" | "agentIdentity" => Some(Self::AgentIdentity),
            "personal_access_token" | "personalAccessToken" => Some(Self::PersonalAccessToken),
            "bedrock_api_key" | "bedrockApiKey" => Some(Self::BedrockApiKey),
            _ => None,
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ApiKey => "OpenAI API key",
            Self::Chatgpt => "Codex ChatGPT login",
            Self::ChatgptAuthTokens => "external Codex ChatGPT tokens",
            Self::AgentIdentity => "Codex agent identity",
            Self::PersonalAccessToken => "Codex personal access token",
            Self::BedrockApiKey => "Codex Bedrock API key",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthSource {
    EnvAccessToken,
    AuthJson,
}

impl CodexAuthSource {
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::EnvAccessToken => CODEX_ACCESS_TOKEN_ENV_VAR,
            Self::AuthJson => "Codex auth.json",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CodexResponsesAuth {
    pub access_token: String,
    pub account_id: Option<String>,
    pub is_fedramp_account: bool,
    pub source: CodexAuthSource,
    pub mode: CodexAuthMode,
}

impl std::fmt::Debug for CodexResponsesAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexResponsesAuth")
            .field("access_token", &"<redacted>")
            .field("account_id", &self.account_id)
            .field("is_fedramp_account", &self.is_fedramp_account)
            .field("source", &self.source)
            .field("mode", &self.mode)
            .finish()
    }
}

impl CodexResponsesAuth {
    #[must_use]
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.access_token),
            ),
            ("Accept".to_string(), "text/event-stream".to_string()),
        ];
        if let Some(account_id) = &self.account_id {
            headers.push(("ChatGPT-Account-ID".to_string(), account_id.clone()));
        }
        if self.is_fedramp_account {
            headers.push(("X-OpenAI-Fedramp".to_string(), "true".to_string()));
        }
        headers
    }

    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{} via {}",
            self.mode.display_name(),
            self.source.display_name()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAuthMaterial {
    ApiKey {
        api_key: String,
        source: CodexAuthSource,
    },
    Responses(CodexResponsesAuth),
    Unsupported {
        mode: CodexAuthMode,
        source: CodexAuthSource,
    },
}

impl CodexAuthMaterial {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::ApiKey { source, .. } => {
                format!("OpenAI API key via {}", source.display_name())
            }
            Self::Responses(auth) => auth.label(),
            Self::Unsupported { mode, source } => {
                format!("{} via {}", mode.display_name(), source.display_name())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct AuthDotJson {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default, rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: Option<TokenData>,
    #[serde(default)]
    agent_identity: Option<Value>,
    #[serde(default)]
    personal_access_token: Option<String>,
    #[serde(default)]
    bedrock_api_key: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    #[serde(default)]
    id_token: Option<IdToken>,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum IdToken {
    Raw(String),
    Object {
        #[serde(default)]
        raw_jwt: Option<String>,
        #[serde(default)]
        chatgpt_account_id: Option<String>,
        #[serde(default)]
        is_fedramp_account: Option<bool>,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct JwtClaims {
    account_id: Option<String>,
    is_fedramp_account: bool,
}

fn trimmed_nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn raw_id_token(id_token: &Option<IdToken>) -> Option<&str> {
    match id_token {
        Some(IdToken::Raw(raw)) => Some(raw.as_str()),
        Some(IdToken::Object { raw_jwt, .. }) => raw_jwt.as_deref(),
        None => None,
    }
}

fn id_token_claims(id_token: &Option<IdToken>) -> JwtClaims {
    let mut claims = raw_id_token(id_token)
        .and_then(parse_jwt_claims)
        .unwrap_or_default();
    if let Some(IdToken::Object {
        chatgpt_account_id,
        is_fedramp_account,
        ..
    }) = id_token
    {
        if claims.account_id.is_none() {
            claims.account_id.clone_from(chatgpt_account_id);
        }
        if !claims.is_fedramp_account {
            claims.is_fedramp_account = is_fedramp_account.unwrap_or(false);
        }
    }
    claims
}

fn parse_jwt_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    let account_id = value
        .get("https://api.openai.com/auth.chatgpt_account_id")
        .or_else(|| value.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_fedramp_account = value
        .get("https://api.openai.com/auth.chatgpt_account_is_fedramp")
        .or_else(|| value.get("is_fedramp_account"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(JwtClaims {
        account_id,
        is_fedramp_account,
    })
}

fn resolved_mode(auth: &AuthDotJson) -> Option<CodexAuthMode> {
    if let Some(mode) = auth.auth_mode.as_deref().and_then(CodexAuthMode::parse) {
        return Some(mode);
    }
    if auth
        .personal_access_token
        .as_ref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Some(CodexAuthMode::PersonalAccessToken);
    }
    if auth.bedrock_api_key.is_some() {
        return Some(CodexAuthMode::BedrockApiKey);
    }
    if auth
        .openai_api_key
        .as_ref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Some(CodexAuthMode::ApiKey);
    }
    if auth.agent_identity.is_some() {
        return Some(CodexAuthMode::AgentIdentity);
    }
    if auth
        .tokens
        .as_ref()
        .is_some_and(|t| !t.access_token.trim().is_empty())
    {
        return Some(CodexAuthMode::Chatgpt);
    }
    None
}

fn load_from_auth_json(auth: AuthDotJson) -> Result<Option<CodexAuthMaterial>, String> {
    let Some(mode) = resolved_mode(&auth) else {
        return Ok(None);
    };
    match mode {
        CodexAuthMode::ApiKey => {
            let Some(api_key) = trimmed_nonempty(auth.openai_api_key) else {
                return Ok(Some(CodexAuthMaterial::Unsupported {
                    mode,
                    source: CodexAuthSource::AuthJson,
                }));
            };
            Ok(Some(CodexAuthMaterial::ApiKey {
                api_key,
                source: CodexAuthSource::AuthJson,
            }))
        }
        CodexAuthMode::Chatgpt | CodexAuthMode::ChatgptAuthTokens => {
            let Some(tokens) = auth.tokens else {
                return Ok(Some(CodexAuthMaterial::Unsupported {
                    mode,
                    source: CodexAuthSource::AuthJson,
                }));
            };
            let access_token = tokens.access_token.trim().to_string();
            if access_token.is_empty() {
                return Ok(Some(CodexAuthMaterial::Unsupported {
                    mode,
                    source: CodexAuthSource::AuthJson,
                }));
            }
            let claims = id_token_claims(&tokens.id_token);
            let token_claims = parse_jwt_claims(&access_token).unwrap_or_default();
            Ok(Some(CodexAuthMaterial::Responses(CodexResponsesAuth {
                access_token,
                account_id: tokens
                    .account_id
                    .or(claims.account_id)
                    .or(token_claims.account_id),
                is_fedramp_account: claims.is_fedramp_account || token_claims.is_fedramp_account,
                source: CodexAuthSource::AuthJson,
                mode,
            })))
        }
        CodexAuthMode::PersonalAccessToken => {
            let Some(access_token) = trimmed_nonempty(auth.personal_access_token) else {
                return Ok(Some(CodexAuthMaterial::Unsupported {
                    mode,
                    source: CodexAuthSource::AuthJson,
                }));
            };
            let claims = parse_jwt_claims(&access_token).unwrap_or_default();
            Ok(Some(CodexAuthMaterial::Responses(CodexResponsesAuth {
                access_token,
                account_id: claims.account_id,
                is_fedramp_account: claims.is_fedramp_account,
                source: CodexAuthSource::AuthJson,
                mode,
            })))
        }
        CodexAuthMode::AgentIdentity | CodexAuthMode::BedrockApiKey => {
            Ok(Some(CodexAuthMaterial::Unsupported {
                mode,
                source: CodexAuthSource::AuthJson,
            }))
        }
    }
}

#[must_use]
pub fn codex_home() -> Option<PathBuf> {
    std::env::var_os(CODEX_HOME_ENV_VAR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

#[must_use]
pub fn auth_json_path() -> Option<PathBuf> {
    codex_home().map(|home| home.join("auth.json"))
}

#[must_use]
pub fn has_codex_auth_json() -> bool {
    auth_json_path().is_some_and(|path| path.is_file())
}

/// Load current Codex auth material from `CODEX_ACCESS_TOKEN` or auth.json.
///
/// This intentionally does not inspect OS keyrings or refresh tokens; those are
/// owned by Codex proper. Stale cached tokens surface as upstream auth errors.
pub fn load_codex_auth() -> Result<Option<CodexAuthMaterial>, String> {
    if let Ok(token) = std::env::var(CODEX_ACCESS_TOKEN_ENV_VAR) {
        let token = token.trim();
        if !token.is_empty() {
            let claims = parse_jwt_claims(token).unwrap_or_default();
            return Ok(Some(CodexAuthMaterial::Responses(CodexResponsesAuth {
                access_token: token.to_string(),
                account_id: claims.account_id,
                is_fedramp_account: claims.is_fedramp_account,
                source: CodexAuthSource::EnvAccessToken,
                mode: CodexAuthMode::ChatgptAuthTokens,
            })));
        }
    }

    let Some(path) = auth_json_path() else {
        return Ok(None);
    };
    load_codex_auth_from_path(&path)
}

pub fn load_codex_auth_from_path(path: &Path) -> Result<Option<CodexAuthMaterial>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("failed to stat {}: {err}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing to read symlinked {}", path.display()));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    let auth: AuthDotJson = serde_json::from_str(&raw)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;
    load_from_auth_json(auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_auth_json(value: Value) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(&path, serde_json::to_vec(&value).expect("json")).expect("write auth");
        (dir, path)
    }

    #[test]
    fn loads_api_key_from_auth_json() {
        let (_dir, path) = write_auth_json(json!({
            "auth_mode": "api_key",
            "OPENAI_API_KEY": "sk-test"
        }));

        let auth = load_codex_auth_from_path(&path)
            .expect("load")
            .expect("auth");

        assert_eq!(
            auth,
            CodexAuthMaterial::ApiKey {
                api_key: "sk-test".to_string(),
                source: CodexAuthSource::AuthJson
            }
        );
    }

    #[test]
    fn loads_chatgpt_tokens_for_responses_backend() {
        let (_dir, path) = write_auth_json(json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "access-token",
                "account_id": "account-123",
                "id_token": {
                    "raw_jwt": null,
                    "is_fedramp_account": true
                }
            }
        }));

        let auth = load_codex_auth_from_path(&path)
            .expect("load")
            .expect("auth");

        let CodexAuthMaterial::Responses(auth) = auth else {
            panic!("expected responses auth");
        };
        assert_eq!(auth.access_token, "access-token");
        assert_eq!(auth.account_id.as_deref(), Some("account-123"));
        assert!(auth.is_fedramp_account);
        assert_eq!(auth.mode, CodexAuthMode::Chatgpt);
    }

    #[test]
    fn refuses_symlinked_auth_json() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("target.json");
            let link = dir.path().join("auth.json");
            std::fs::write(&target, "{}").expect("write target");
            std::os::unix::fs::symlink(&target, &link).expect("symlink");

            let err = load_codex_auth_from_path(&link).unwrap_err();
            assert!(err.contains("refusing to read symlinked"));
        }
    }
}
