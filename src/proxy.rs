//! HTTP Proxy Server - The core of `OpenClaudia`.
//!
//! Accepts OpenAI-compatible requests and forwards them to the configured provider
//! after running hooks and injecting context.

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::compaction::{CompactionConfig, ContextCompactor};
use crate::config::{AppConfig, ProviderConfig};
use crate::context::ContextInjector;
use crate::hooks::{
    load_claude_code_hooks, merge_hooks_config, HookEngine, HookError, HookEvent, HookInput,
    HookResult,
};
use crate::mcp::McpManager;
use crate::oauth::OAuthStore;
use crate::plugins::PluginManager;
use crate::providers::{get_adapter, ApiKey};
use crate::rules::{extract_extensions_from_tool_input, RulesEngine};
use crate::session::{get_session_context, SessionManager, TokenUsage};
use crate::vdd::{VddEngine, VddResult};

/// Normalize base URL by stripping trailing slash and /v1 suffix.
/// This prevents double /v1/v1 when endpoint paths include /v1 prefix.
#[must_use]
pub fn normalize_base_url(base_url: &str) -> String {
    base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_string()
}

/// Shared state for the proxy
#[derive(Clone)]
pub struct ProxyState {
    pub config: Arc<AppConfig>,
    pub client: Client,
    pub hook_engine: HookEngine,
    pub rules_engine: RulesEngine,
    pub compactor: ContextCompactor,
    pub session_manager: Arc<RwLock<SessionManager>>,
    pub plugin_manager: Arc<PluginManager>,
    pub mcp_manager: Arc<RwLock<McpManager>>,
    /// OAuth session store for Claude Max authentication
    pub oauth_store: Arc<OAuthStore>,
    /// VDD engine for adversarial review (if enabled)
    pub vdd_engine: Option<Arc<tokio::sync::Mutex<VddEngine>>>,
}

/// Errors that can occur in the proxy
#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("Provider not configured: {0}")]
    ProviderNotConfigured(String),

    #[error("No API key configured for provider: {0}")]
    NoApiKey(String),

    #[error("Request error: {0}")]
    RequestError(#[from] reqwest::Error),

    #[error("Invalid request body: {0}")]
    InvalidBody(String),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Hook blocked request: {0}")]
    HookBlocked(String),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::NoApiKey(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            Self::RequestError(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::HookBlocked(_) => (StatusCode::FORBIDDEN, self.to_string()),
            Self::ProviderNotConfigured(_) | Self::InvalidBody(_) | Self::JsonError(_) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
        };

        let body = serde_json::json!({
            "error": {
                "message": message,
                "type": "proxy_error"
            }
        });

        (status, Json(body)).into_response()
    }
}

/// OpenAI-compatible chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Message content can be string or array of content parts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// Content part for multimodal messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<Value>,
}

/// OpenAI-compatible chat completion request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

/// Create the proxy router
pub fn create_router(state: ProxyState) -> Router {
    Router::new()
        // Health check
        .route("/health", get(health_check))
        // Auth routes (device flow for Claude Max OAuth)
        .route("/auth/device", get(auth_device_page))
        .route("/auth/device/start", axum::routing::post(auth_device_start))
        .route(
            "/auth/device/submit",
            axum::routing::post(auth_device_submit),
        )
        .route("/auth/status", get(auth_status))
        // Stats endpoint for token usage
        .route("/stats", get(session_stats))
        // OpenAI-compatible endpoints
        .route("/v1/chat/completions", any(proxy_chat_completions))
        .route("/v1/completions", any(proxy_completions))
        .route("/v1/models", get(list_models))
        // Anthropic-compatible endpoints (for direct Anthropic clients)
        .route("/v1/messages", any(proxy_anthropic_messages))
        // Catch-all for other API routes
        .route("/v1/{*path}", any(proxy_passthrough))
        .with_state(state)
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "openclaudia",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Session stats endpoint - returns token usage and turn metrics
async fn session_stats(State(state): State<ProxyState>) -> impl IntoResponse {
    let sm = state.session_manager.read().await;
    Json(sm.get_session().map_or_else(
        || serde_json::json!({ "error": "No active session" }),
        |session| {
            let last_turn = session.turn_metrics.last();
            serde_json::json!({
                "session_id": session.id,
                "mode": session.mode,
                "request_count": session.request_count,
                "turns": session.turn_metrics.len(),
                "cumulative_usage": {
                    "input_tokens": session.cumulative_usage.input_tokens,
                    "output_tokens": session.cumulative_usage.output_tokens,
                    "cache_read_tokens": session.cumulative_usage.cache_read_tokens,
                    "cache_write_tokens": session.cumulative_usage.cache_write_tokens,
                    "total_tokens": session.cumulative_usage.total(),
                },
                "last_turn": last_turn.map(|t| serde_json::json!({
                    "turn_number": t.turn_number,
                    "estimated_input_tokens": t.estimated_input_tokens,
                    "injected_context_tokens": t.injected_context_tokens,
                    "system_prompt_tokens": t.system_prompt_tokens,
                    "tool_def_tokens": t.tool_def_tokens,
                    "actual_usage": t.actual_usage.as_ref().map(|u| serde_json::json!({
                        "input_tokens": u.input_tokens,
                        "output_tokens": u.output_tokens,
                        "cache_read_tokens": u.cache_read_tokens,
                        "cache_write_tokens": u.cache_write_tokens,
                    })),
                })),
            })
        },
    ))
}

/// Device flow page - HTML UI for OAuth authentication
async fn auth_device_page() -> impl IntoResponse {
    axum::response::Html(include_str!("../assets/device_flow.html"))
}

/// Start device authorization flow
async fn auth_device_start(
    State(state): State<ProxyState>,
) -> Result<impl IntoResponse, ProxyError> {
    use crate::oauth::{PkceParams, ANTHROPIC_CLIENT_ID, ANTHROPIC_REDIRECT_URI};

    let pkce = PkceParams::generate();
    let oauth_state = pkce.state.clone();

    // Store PKCE for later verification
    state.oauth_store.store_challenge(pkce.clone());

    // Build authorization URL
    let auth_url = format!(
        "https://claude.ai/oauth/authorize?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        ANTHROPIC_CLIENT_ID,
        urlencoding::encode(ANTHROPIC_REDIRECT_URI),
        urlencoding::encode("org:create_api_key user:profile user:inference"),
        pkce.challenge,
        oauth_state
    );

    info!("Device flow auth URL generated");

    Ok(Json(serde_json::json!({
        "auth_url": auth_url,
        "state": oauth_state
    })))
}

/// Submit authorization code from device flow
async fn auth_device_submit(
    State(state): State<ProxyState>,
    Json(payload): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ProxyError> {
    use crate::oauth::{parse_auth_code, OAuthClient, OAuthSession};

    let mut code = payload["code"].as_str().unwrap_or("").to_string();
    let mut oauth_state = payload["state"].as_str().unwrap_or("").to_string();

    // Handle combined code#state format
    if code.contains('#') {
        let (parsed_code, parsed_state) = parse_auth_code(&code);
        code = parsed_code;
        if let Some(s) = parsed_state {
            oauth_state = s;
        }
    }

    // Get PKCE challenge
    let pkce = state
        .oauth_store
        .take_challenge(&oauth_state)
        .ok_or_else(|| ProxyError::InvalidBody("Invalid state parameter".to_string()))?;

    // Exchange code for tokens
    let client = OAuthClient::new();
    let token_response = client
        .exchange_code(&code, &pkce)
        .await
        .map_err(|e| ProxyError::InvalidBody(format!("Token exchange failed: {e}")))?;

    // Create session
    let mut session = OAuthSession::from_token_response(token_response);

    // Try to create API key if we have the scope
    if session.can_create_api_key() {
        if let Ok(api_key) = client
            .create_api_key(&session.credentials.access_token)
            .await
        {
            session.api_key = Some(api_key);
        }
    }

    let session_id = session.id.clone();
    state.oauth_store.store_session(session);

    info!(
        "Device flow authentication successful, session: {}",
        session_id
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Authentication successful",
        "session_id": session_id
    })))
}

/// Check authentication status
async fn auth_status(State(state): State<ProxyState>, headers: HeaderMap) -> impl IntoResponse {
    // Check for session from cookie first
    let session = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let cookie = cookie.trim();
                cookie
                    .strip_prefix("anthropic_session=")
                    .map(std::string::ToString::to_string)
            })
        })
        .and_then(|session_id| state.oauth_store.get_session(&session_id));

    // No "any valid session" fallback — an absent cookie returns
    // `authenticated: false`. The previous fallback let any unauth
    // caller learn another user's session id. See crosslink #375.

    match session {
        Some(s) => Json(serde_json::json!({
            "authenticated": true,
            "session_id": s.id
        })),
        None => Json(serde_json::json!({
            "authenticated": false,
            "session_id": null
        })),
    }
}

/// List available models (returns configured provider's models)
async fn list_models(State(_state): State<ProxyState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "object": "list",
        "data": [
            {"id": "claude-opus-4-6", "object": "model", "owned_by": "anthropic"},
            {"id": "claude-sonnet-4-6", "object": "model", "owned_by": "anthropic"},
            {"id": "claude-haiku-4-5-20251001", "object": "model", "owned_by": "anthropic"},
            {"id": "gpt-5.2", "object": "model", "owned_by": "openai"},
            {"id": "gpt-4.1", "object": "model", "owned_by": "openai"},
        ]
    }))
}

/// Run `PreToolUse` hooks for tool calls in the response
async fn run_pre_tool_use_hooks(
    hook_engine: &HookEngine,
    session_id: Option<&str>,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> HookResult {
    // Security enforcement handled by the permissions system (src/permissions.rs)

    // Extract file extensions from tool input for context
    let extensions = extract_extensions_from_tool_input(tool_name, tool_input);

    let mut hook_input =
        HookInput::new(HookEvent::PreToolUse).with_tool(tool_name, tool_input.clone());

    if let Some(sid) = session_id {
        hook_input = hook_input.with_session_id(sid);
    }

    // Add extensions as extra context
    if !extensions.is_empty() {
        hook_input = hook_input.with_extra("extensions", serde_json::json!(extensions));
    }

    let result = hook_engine.run(HookEvent::PreToolUse, &hook_input).await;

    if !result.allowed {
        debug!(
            tool = %tool_name,
            "PreToolUse hook blocked tool execution"
        );
    }

    result
}

/// Lazily-compiled regex for extracting file extensions from message text.
static EXTENSION_PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"[\w/\\.-]+\.([a-zA-Z0-9]{1,10})\b")
        .expect("EXTENSION_PATTERN is a valid static regex")
});

/// Extract file extensions from message content (looks for file paths)
fn extract_extensions_from_messages(messages: &[ChatMessage]) -> Vec<String> {
    use std::collections::HashSet;

    let mut extensions = HashSet::new();
    let extension_pattern = &*EXTENSION_PATTERN;

    for msg in messages {
        let text = match &msg.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.clone())
                .collect::<Vec<_>>()
                .join(" "),
        };

        for cap in extension_pattern.captures_iter(&text) {
            if let Some(ext) = cap.get(1) {
                extensions.insert(ext.as_str().to_lowercase());
            }
        }
    }

    extensions.into_iter().collect()
}

/// Prepare a chat completion request: run hooks, inject context, rules,
/// MCP tools, plugins, VDD.
///
/// The `#[allow(clippy::too_many_lines)]` below is deliberately retained
/// — this function is a long linear sequence of independent injection
/// phases (hook, prompt-mod, context inject, rules, MCP tools, plugin
/// tools, VDD context). Breaking it further without an enclosing
/// orchestrator would just move line count around. A follow-up PR can
/// formalize a `RequestContextPipeline` if it becomes worth the weight.
#[allow(clippy::too_many_lines)]
async fn prepare_request_context(
    request: &mut ChatCompletionRequest,
    state: &ProxyState,
) -> Result<(), ProxyError> {
    // Run UserPromptSubmit hooks
    let last_user_message = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        });

    let hook_input = HookInput::new(HookEvent::UserPromptSubmit)
        .with_prompt(last_user_message.unwrap_or_default());

    let hook_result = state
        .hook_engine
        .run(HookEvent::UserPromptSubmit, &hook_input)
        .await;

    if !hook_result.allowed {
        let reason = hook_result
            .outputs
            .first()
            .and_then(|o| o.reason.clone())
            .unwrap_or_else(|| "Request blocked by hook".to_string());
        return Err(ProxyError::HookBlocked(reason));
    }

    ContextInjector::apply_prompt_modification(request, &hook_result);
    ContextInjector::inject(request, &hook_result);

    // Inject rules based on file extensions
    let extensions = extract_extensions_from_messages(&request.messages);
    if !extensions.is_empty() {
        let rules_content = state.rules_engine.get_combined_rules(
            &extensions
                .iter()
                .map(std::string::String::as_str)
                .collect::<Vec<_>>(),
        );
        if !rules_content.is_empty() {
            ContextInjector::inject_system_prefix(request, &rules_content);
        }
    }

    // Add MCP tools
    let mcp_tools = state.mcp_manager.read().await.tools_as_openai_functions();
    if !mcp_tools.is_empty() {
        let mut tools = request.tools.take().unwrap_or_default();
        tools.extend(mcp_tools);
        request.tools = Some(tools);
    }

    // Add plugin commands as context
    let plugin_commands: Vec<String> = state
        .plugin_manager
        .all_commands()
        .iter()
        .map(|(plugin, cmd)| format!("/{}:{} (from {})", plugin.name(), cmd.name, plugin.name()))
        .collect();
    if !plugin_commands.is_empty() {
        let commands_context = format!("Available plugin commands: {}", plugin_commands.join(", "));
        ContextInjector::inject_system_suffix(request, &commands_context);
    }

    // Inject session context
    let session_context = {
        let sm = state.session_manager.read().await;
        sm.get_session().map(get_session_context)
    };
    if let Some(context) = session_context {
        ContextInjector::inject_all(request, &[context]);
    }

    // Inject VDD advisory from previous turn
    {
        let mut sm = state.session_manager.write().await;
        if let Some(vdd_context) = sm.take_vdd_context() {
            if !vdd_context.is_empty() {
                ContextInjector::inject_system_suffix(request, &vdd_context);
                debug!("Injected VDD advisory context from previous turn");
            }
        }
    }

    // Run PreToolUse hooks for tool calls in previous messages
    for msg in &request.messages {
        if let Some(tool_calls) = &msg.tool_calls {
            for tool_call in tool_calls {
                if let (Some(name), Some(args)) = (
                    tool_call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str()),
                    tool_call.get("function").and_then(|f| f.get("arguments")),
                ) {
                    let session_id = {
                        let sm = state.session_manager.read().await;
                        sm.get_session().map(|s| s.id.clone())
                    };
                    let hook_result = run_pre_tool_use_hooks(
                        &state.hook_engine,
                        session_id.as_deref(),
                        name,
                        args,
                    )
                    .await;

                    for output in &hook_result.outputs {
                        if let Some(extra_data) = output.extra.get("metadata") {
                            debug!(metadata = %extra_data, "Hook provided extra metadata");
                        }
                    }

                    if let Err(hook_err) = HookEngine::check_blocked(&hook_result) {
                        let reason = match hook_err {
                            HookError::Blocked(r) => r,
                            _ => "PreToolUse hook blocked".to_string(),
                        };
                        return Err(ProxyError::HookBlocked(format!(
                            "Tool '{name}' blocked: {reason}"
                        )));
                    }
                }
            }
        }
    }

    Ok(())
}

/// Estimate turn tokens (input / system / tool-definition), record them on
/// the active session, log the per-turn usage, and fire the
/// `token_warning` notification when estimated input exceeds the
/// configured warn threshold. Extracted from `proxy_chat_completions`
/// per crosslink #247 (SRP decomposition).
async fn record_turn_estimate(state: &ProxyState, request: &ChatCompletionRequest) {
    let estimated_input = crate::compaction::estimate_request_tokens(request);

    // Break down token components
    let system_prompt_tokens: usize = request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(crate::compaction::estimate_message_tokens)
        .sum();

    let tool_def_tokens: usize = request.tools.as_ref().map_or(0, |tools| {
        tools
            .iter()
            .map(|t| crate::compaction::estimate_tokens(&t.to_string()))
            .sum()
    });

    let injected_context_tokens = system_prompt_tokens + tool_def_tokens;

    let mut sm = state.session_manager.write().await;
    let Some(session) = sm.get_session_mut() else {
        return;
    };

    let turn = session.record_turn_estimate(
        estimated_input,
        injected_context_tokens,
        system_prompt_tokens,
        tool_def_tokens,
    );
    let context_window = crate::compaction::get_context_window(&request.model);
    // Integer-safe utilization computation.
    let utilization_pct_x10 = if context_window > 0 {
        estimated_input.saturating_mul(1000) / context_window
    } else {
        0
    };
    #[allow(clippy::cast_possible_truncation)]
    let usage_pct_f64 = f64::from(utilization_pct_x10 as u32) / 10.0;

    if state.config.session.token_tracking.log_usage {
        info!(
            turn = turn,
            estimated_input = estimated_input,
            system_prompt = system_prompt_tokens,
            tool_defs = tool_def_tokens,
            context_window = context_window,
            utilization_pct = format!("{usage_pct_f64:.1}%"),
            "Turn token estimate"
        );
    }

    let warn_threshold = state.config.session.token_tracking.warn_threshold;
    // Integer threshold avoids usize→f32 precision loss.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let threshold_tokens = (f64::from(context_window as u32) * f64::from(warn_threshold)) as usize;
    if estimated_input > threshold_tokens {
        warn!(
            estimated = estimated_input,
            threshold = format!("{:.0}%", warn_threshold * 100.0),
            context_window = context_window,
            "Token usage approaching context window limit"
        );
        // Fire token warning notification
        drop(sm); // release the write lock before the hook fires notifications
        state
            .hook_engine
            .fire_notification(
                "token_warning",
                serde_json::json!({ "usage_pct": usage_pct_f64 }),
            )
            .await;
    }
}

/// Run the VDD adversarial-review pipeline against a freshly-converted
/// builder response and return the (possibly-revised) response.
///
/// Consolidates the four `Response::from_parts` reassembly sites
/// previously inlined in `proxy_chat_completions` (one per VDD result
/// variant plus the JSON-parse-failure fallthrough) into a single
/// pattern-matched helper. See crosslink #247 point 5.
///
/// Bounded read closes crosslink #352: `max_response_bytes` (default
/// 50 MiB) caps the buffered body; over-limit and other read errors
/// log at `warn!` and return an empty passthrough rather than feeding
/// an empty buffer to the VDD engine.
async fn apply_vdd_review(
    response_value: Response,
    state: &ProxyState,
    request: &ChatCompletionRequest,
    provider_name: &str,
    api_key: &ApiKey,
) -> Result<Response, ProxyError> {
    let Some(vdd_engine) = &state.vdd_engine else {
        return Ok(response_value);
    };

    let (parts, body) = response_value.into_parts();
    let max_bytes = state.config.proxy.max_response_bytes;
    let response_bytes = match axum::body::to_bytes(body, max_bytes).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                error = %e,
                max_bytes = max_bytes,
                "Failed to read upstream response body for VDD review; \
                 returning empty passthrough to the client (crosslink #352)."
            );
            return Ok(Response::from_parts(parts, Body::empty()));
        }
    };

    // Parse the body as JSON. Non-JSON bodies are passed through verbatim.
    let Ok(response_json) = serde_json::from_slice::<Value>(&response_bytes) else {
        return Ok(Response::from_parts(parts, Body::from(response_bytes)));
    };

    let engine = vdd_engine.lock().await;
    let vdd_result = engine
        .process_response(&response_json, request, provider_name, Some(api_key))
        .await;

    // Decide which bytes to ship back. Only `Blocking` produces new bytes;
    // every other path reuses the original response body.
    let body_bytes: Vec<u8> = match vdd_result {
        Ok(VddResult::Advisory(advisory)) => {
            let genuine = advisory
                .findings
                .iter()
                .filter(|f| f.status == crate::vdd::FindingStatus::Genuine)
                .count();
            if !advisory.context_injection.is_empty() {
                let mut sm = state.session_manager.write().await;
                sm.store_vdd_context(advisory.context_injection);
            }
            info!(
                total = advisory.findings.len(),
                genuine = genuine,
                "VDD advisory review complete"
            );
            response_bytes.to_vec()
        }
        Ok(VddResult::Blocking(blocking)) => {
            info!(
                iterations = blocking.session.iterations.len(),
                genuine = blocking.session.total_genuine,
                converged = blocking.session.converged,
                chainlink_issues = blocking.chainlink_issues.len(),
                "VDD blocking loop complete"
            );
            serde_json::to_vec(&blocking.final_response)
                .unwrap_or_else(|_| response_bytes.to_vec())
        }
        Ok(VddResult::Skipped(reason)) => {
            debug!(reason = %reason, "VDD skipped");
            response_bytes.to_vec()
        }
        Err(e) => {
            warn!(error = %e, "VDD error (non-blocking, returning original response)");
            response_bytes.to_vec()
        }
    };

    Ok(Response::from_parts(parts, Body::from(body_bytes)))
}

async fn proxy_chat_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, ProxyError> {
    let request: ChatCompletionRequest =
        serde_json::from_str(&body).map_err(|e| ProxyError::InvalidBody(e.to_string()))?;

    info!(
        model = %request.model,
        messages = request.messages.len(),
        "Proxying chat completion request"
    );

    // Determine target provider from model or config
    let provider_name = determine_provider(&request.model, &state.config);
    let provider = state
        .config
        .get_provider(&provider_name)
        .ok_or_else(|| ProxyError::ProviderNotConfigured(provider_name.clone()))?;

    // Get API key from header or config
    let api_key = extract_api_key(&headers)
        .or_else(|| provider.api_key.clone())
        .ok_or_else(|| ProxyError::NoApiKey(provider_name.clone()))?;

    // Track request in session
    {
        let mut sm = state.session_manager.write().await;
        if let Some(session) = sm.get_session_mut() {
            session.increment_requests();
        }
    }

    // Prepare request: run hooks, inject context, rules, MCP tools, VDD
    let mut request = request;
    prepare_request_context(&mut request, &state).await?;

    // Create model-specific compactor for accurate context limits
    let mut compactor = crate::compaction::ContextCompactor::for_model(&request.model);

    // Optionally update config from state compactor settings
    let base_config = state.compactor.config().clone();
    let mut model_config = compactor.config().clone();
    model_config.preserve_recent = base_config.preserve_recent;
    model_config.preserve_system = base_config.preserve_system;
    model_config.preserve_tool_calls = base_config.preserve_tool_calls;
    compactor.set_config(model_config);

    // Get last actual input token count from session for more accurate compaction
    let actual_token_hint: Option<usize> = {
        let sm = state.session_manager.read().await;
        sm.get_session().and_then(|session| {
            session
                .turn_metrics
                .last()
                .and_then(|tm| tm.actual_usage.as_ref())
                .map(|u| usize::try_from(u.input_tokens).unwrap_or(usize::MAX))
        })
    };

    // Compact context if needed (for long conversations)
    let compaction_result = compactor
        .compact_with_hint(
            &mut request,
            Some(&state.hook_engine),
            None,
            actual_token_hint,
        )
        .await;

    match compaction_result {
        Ok(result) => {
            if result.compacted {
                let summary_len = result.summary.as_ref().map_or(0, std::string::String::len);
                info!(
                    original = result.original_tokens,
                    new = result.new_tokens,
                    summarized = result.messages_summarized,
                    summary_len = summary_len,
                    "Context compacted"
                );
                // Log the summary if available (for debugging)
                if let Some(summary) = &result.summary {
                    debug!(summary = %summary, "Compaction summary generated");
                }
                // Fire compaction notification
                state
                    .hook_engine
                    .fire_notification(
                        "compaction",
                        serde_json::json!({ "summary_length": summary_len }),
                    )
                    .await;
            }
        }
        Err(crate::compaction::CompactionError::HookBlocked(reason)) => {
            warn!(reason = %reason, "Compaction blocked by hook");
        }
        Err(crate::compaction::CompactionError::Failed(reason)) => {
            warn!(reason = %reason, "Compaction failed");
        }
    }

    // Get the provider adapter for request transformation
    let adapter = get_adapter(&provider_name);
    debug!(provider = adapter.name(), "Using provider adapter");

    // Pre-request token estimation and tracking
    let token_tracking_enabled = state.config.session.token_tracking.enabled;
    if token_tracking_enabled {
        record_turn_estimate(&state, &request).await;
    }

    let is_stream = request.stream.unwrap_or(false);

    // Transform request to provider format with thinking config
    let mut transformed_request = adapter
        .transform_request_with_thinking(&request, &provider.thinking)
        .map_err(|e| ProxyError::InvalidBody(e.to_string()))?;

    // For OpenAI-compatible streaming requests, inject stream_options to
    // include usage in the final SSE event so we can track token costs.
    if is_stream && !provider_name.contains("anthropic") {
        if let Some(obj) = transformed_request.as_object_mut() {
            if !obj.contains_key("stream_options") {
                obj.insert(
                    "stream_options".to_string(),
                    serde_json::json!({"include_usage": true}),
                );
            }
        }
    }

    // Forward to provider with transformed request
    let raw_response = forward_to_provider_raw_reqwest(
        &state.client,
        provider,
        &api_key,
        &adapter.chat_endpoint(&request.model),
        &transformed_request,
        is_stream,
        adapter.get_headers(&api_key),
    )
    .await?;

    // Post-response: extract usage from non-streaming responses and convert
    if token_tracking_enabled && !is_stream {
        let (response_value, usage) = convert_response_with_usage(raw_response).await?;
        if let Some(usage) = usage {
            let mut sm = state.session_manager.write().await;
            if let Some(session) = sm.get_session_mut() {
                if state.config.session.token_tracking.log_usage {
                    info!(
                        input = usage.input_tokens,
                        output = usage.output_tokens,
                        cache_read = usage.cache_read_tokens,
                        cache_write = usage.cache_write_tokens,
                        "Actual token usage from provider"
                    );
                }
                session.record_actual_usage(usage);
            }
        }

        let response_value =
            apply_vdd_review(response_value, &state, &request, &provider_name, &api_key).await?;

        Ok(response_value)
    } else {
        let response = convert_response(raw_response).await?;
        Ok(response)
    }
}

/// Handle MCP tool calls from the model response.
///
/// # Errors
///
/// Returns `ProxyError::InvalidBody` if the MCP server is not connected or
/// the tool call fails.
pub async fn handle_mcp_tool_call(
    mcp_manager: &Arc<RwLock<McpManager>>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ProxyError> {
    let mcp = mcp_manager.read().await;

    // Check if the MCP server is connected (format: mcp__servername__toolname)
    let parts: Vec<&str> = tool_name.splitn(3, "__").collect();
    if parts.len() == 3 && parts[0] == "mcp" {
        let server_name = parts[1];
        if !mcp.is_connected(server_name) {
            return Err(ProxyError::InvalidBody(format!(
                "MCP server '{server_name}' is not connected"
            )));
        }
    }

    // Call the tool
    match mcp.call_tool(tool_name, arguments).await {
        Ok(result) => Ok(result),
        Err(e) => Err(ProxyError::InvalidBody(format!(
            "MCP tool call failed: {e}"
        ))),
    }
}

/// Fire a `tool_error` notification when a tool execution fails.
/// This should be called by any code path that executes tools and gets an error.
pub async fn fire_tool_error_notification(
    hook_engine: &HookEngine,
    tool_name: &str,
    error_msg: &str,
) {
    hook_engine
        .fire_notification(
            "tool_error",
            serde_json::json!({
                "tool": tool_name,
                "error": error_msg,
            }),
        )
        .await;
}

/// Disconnect all MCP servers gracefully
pub async fn shutdown_mcp(mcp_manager: &Arc<RwLock<McpManager>>) {
    let mut mcp = mcp_manager.write().await;
    if let Err(e) = mcp.disconnect_all().await {
        warn!(error = %e, "Error disconnecting MCP servers");
    }
}

/// Proxy completions (legacy `OpenAI` format)
async fn proxy_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, ProxyError> {
    let request: Value =
        serde_json::from_str(&body).map_err(|e| ProxyError::InvalidBody(e.to_string()))?;

    let model = request["model"]
        .as_str()
        .unwrap_or("gpt-3.5-turbo-instruct");
    let provider_name = determine_provider(model, &state.config);
    let provider = state
        .config
        .get_provider(&provider_name)
        .ok_or_else(|| ProxyError::ProviderNotConfigured(provider_name.clone()))?;

    let api_key = extract_api_key(&headers)
        .or_else(|| provider.api_key.clone())
        .ok_or_else(|| ProxyError::NoApiKey(provider_name.clone()))?;

    let is_stream = request["stream"].as_bool().unwrap_or(false);
    let response = forward_to_provider(
        &state.client,
        provider,
        &provider_name,
        &api_key,
        "/v1/completions",
        &request,
        is_stream,
    )
    .await?;

    Ok(response)
}

/// Proxy Anthropic messages endpoint
/// Handles OAuth Bearer token auth with Claude Code system prompt injection (like anthropic-proxy)
async fn proxy_anthropic_messages(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, ProxyError> {
    let mut request: Value =
        serde_json::from_str(&body).map_err(|e| ProxyError::InvalidBody(e.to_string()))?;

    let provider = state
        .config
        .get_provider("anthropic")
        .ok_or_else(|| ProxyError::ProviderNotConfigured("anthropic".to_string()))?;

    // Check for OAuth session from cookie first
    let session = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let cookie = cookie.trim();
                cookie
                    .strip_prefix("anthropic_session=")
                    .map(std::string::ToString::to_string)
            })
        })
        .and_then(|session_id| {
            debug!(
                "[/v1/messages] Looking up session from cookie: {}",
                session_id
            );
            state.oauth_store.get_session(&session_id)
        });

    // No "any valid session" fallback here. An absent cookie falls through
    // to API-key auth below (extract_api_key / provider.api_key), rather
    // than silently impersonating another client's OAuth session. See
    // crosslink #375 (critical) — the prior fallback let any unauth
    // caller (local malicious process, compromised plugin, network-adjacent
    // attacker when bound to 0.0.0.0) charge requests to the first valid
    // session in the store.

    // If we have an OAuth session, use Bearer token auth with Claude Code prompt injection
    if let Some(session) = session {
        info!("[/v1/messages] Using OAuth session: {}", session.id);

        // CRITICAL: Inject Claude Code system prompt (this is what makes OAuth work!)
        // The API validates that requests contain this identifier
        let claude_code_obj = serde_json::json!({
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude."
        });

        match request.get_mut("system") {
            Some(Value::Array(system_array)) => {
                system_array.insert(0, claude_code_obj);
            }
            Some(Value::String(existing_str)) => {
                let existing_obj = serde_json::json!({
                    "type": "text",
                    "text": existing_str.clone()
                });
                request["system"] = serde_json::json!([claude_code_obj, existing_obj]);
            }
            _ => {
                request["system"] = serde_json::json!([claude_code_obj]);
            }
        }

        // Strip TTL from cache_control objects (Anthropic API rejects TTL with OAuth)
        strip_cache_control_ttl(&mut request);

        let url = format!("{}/v1/messages", normalize_base_url(&provider.base_url));

        let mut builder = state.client.post(&url).json(&request);
        // Centralized OAuth header construction — every Anthropic-specific
        // header literal now lives on the adapter. See crosslink #338.
        for (name, value) in crate::providers::AnthropicAdapter::oauth_headers(
            &session.credentials.access_token,
        ) {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder.send().await?;

        return convert_response(response).await;
    }

    // Fallback to API key auth (no system prompt injection needed)
    let api_key = extract_api_key(&headers)
        .or_else(|| provider.api_key.clone())
        .ok_or_else(|| ProxyError::NoApiKey("anthropic".to_string()))?;

    let is_stream = request["stream"].as_bool().unwrap_or(false);
    let response = forward_to_provider(
        &state.client,
        provider,
        "anthropic",
        &api_key,
        "/v1/messages",
        &request,
        is_stream,
    )
    .await?;

    Ok(response)
}

/// Passthrough for unhandled routes
async fn proxy_passthrough(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, ProxyError> {
    let path = request.uri().path();
    let provider = state
        .config
        .active_provider()
        .ok_or_else(|| ProxyError::ProviderNotConfigured(state.config.proxy.target.clone()))?;

    let api_key = extract_api_key(&headers)
        .or_else(|| provider.api_key.clone())
        .ok_or_else(|| ProxyError::NoApiKey(state.config.proxy.target.clone()))?;

    let url = format!("{}{}", normalize_base_url(&provider.base_url), path);
    debug!(url = %url, "Passthrough request");

    let mut req_builder = state.client.request(request.method().clone(), &url);

    // Whitelist safe headers to forward — prevents credential leaks from
    // custom X-* headers or Authorization headers meant for other services.
    const SAFE_PASSTHROUGH_HEADERS: &[&str] = &[
        "accept",
        "accept-encoding",
        "accept-language",
        "user-agent",
        "content-type",
    ];
    for (key, value) in &headers {
        let key_lower = key.as_str().to_lowercase();
        if SAFE_PASSTHROUGH_HEADERS.contains(&key_lower.as_str()) {
            if let Ok(v) = value.to_str() {
                req_builder = req_builder.header(key.as_str(), v);
            }
        }
    }

    // Set auth header based on provider
    // Provider-owned auth headers via the adapter's get_headers method.
    // Previously this called a local `set_auth_header` helper that branched
    // on provider-name equality — the adapter trait is the correct
    // abstraction (crosslink #338).
    let adapter = crate::providers::get_adapter(&state.config.proxy.target);
    for (k, v) in adapter.get_headers(&api_key) {
        req_builder = req_builder.header(k.as_str(), v.as_str());
    }

    let response = req_builder.send().await?;
    convert_response(response).await
}

/// Determine which provider to use based on model name.
/// Returns a static string — no allocation needed.
#[must_use]
pub fn determine_provider(model: &str, config: &AppConfig) -> String {
    let model_lower = model.to_lowercase();
    let provider = if model_lower.starts_with("claude") || model_lower.starts_with("anthropic") {
        "anthropic"
    } else if model_lower.starts_with("gpt")
        || model_lower.starts_with("o1")
        || model_lower.starts_with("o3")
        || model_lower.starts_with("o4")
    {
        "openai"
    } else if model_lower.starts_with("gemini") {
        "google"
    } else if model_lower.starts_with("glm") {
        "zai"
    } else if model_lower.starts_with("deepseek") {
        "deepseek"
    } else if model_lower.starts_with("qwen")
        || model_lower.starts_with("qwq")
        || model_lower.starts_with("qvq")
    {
        "qwen"
    } else {
        // Fall back to configured target
        return config.proxy.target.clone();
    };
    provider.to_string()
}

/// Recursively strip `ttl` from any `cache_control` objects in a JSON value.
/// Anthropic's API rejects TTL in `cache_control` when using OAuth credentials.
fn strip_cache_control_ttl(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(cc_map)) = map.get_mut("cache_control") {
                cc_map.remove("ttl");
            }
            for v in map.values_mut() {
                strip_cache_control_ttl(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_cache_control_ttl(v);
            }
        }
        _ => {}
    }
}

/// Extract API key from `Authorization` or `x-api-key` header.
///
/// Returns `Some(ApiKey)` if the header value parses AND passes
/// [`ApiKey::try_from_string`] validation (non-empty, ASCII, no control
/// chars). A header that fails validation is silently dropped to `None`
/// rather than returning an error — the header may be someone else's
/// garbage (malformed client, stale cookie) and the caller's fallback to
/// `provider.api_key` is the correct recovery. See crosslink #256.
fn extract_api_key(headers: &HeaderMap) -> Option<ApiKey> {
    // Authorization header — must use `Bearer <key>` form. A raw key
    // without the prefix is rejected with a structured warn! so the
    // operator can diagnose mis-configured clients (previously this
    // silently returned None — crosslink #452 mandated point 3).
    let authz = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let from_authz = authz.and_then(|v| {
        v.strip_prefix("Bearer ")
            .map(std::string::ToString::to_string)
            .or_else(|| {
                warn!(
                    "Authorization header present but lacks 'Bearer ' prefix; \
                     refusing to use it as an API key"
                );
                None
            })
    });

    let raw = from_authz.or_else(|| {
        // Also check x-api-key header (Anthropic style)
        headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string)
    })?;

    match ApiKey::try_from_string(raw) {
        Ok(key) => Some(key),
        Err(e) => {
            // Structured log — never the raw value.
            warn!(
                error = %e,
                "Rejected malformed api_key supplied via request header"
            );
            None
        }
    }
}

/// Convert reqwest response to axum response, also extracting token usage if present
async fn convert_response_with_usage(
    response: reqwest::Response,
) -> Result<(Response, Option<TokenUsage>), ProxyError> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = Response::builder().status(status);

    for (key, value) in response.headers() {
        if key != header::TRANSFER_ENCODING && key != header::CONTENT_LENGTH {
            if let Ok(v) = HeaderValue::from_bytes(value.as_bytes()) {
                builder = builder.header(key.as_str(), v);
            }
        }
    }

    let body = response.bytes().await?;

    // Try to extract usage from the response body
    let usage = serde_json::from_slice::<Value>(&body)
        .ok()
        .map(|json| extract_usage_from_response(&json))
        .filter(|u| u.total() > 0);

    let response = builder
        .body(Body::from(body))
        .map_err(|e| ProxyError::InvalidBody(format!("Failed to build response body: {e}")))?;
    Ok((response, usage))
}

/// Extract token usage from a provider's JSON response
/// Handles `OpenAI` format (`usage.prompt_tokens/completion_tokens`)
/// and Anthropic format (`usage.input_tokens/output_tokens`)
fn extract_usage_from_response(response: &Value) -> TokenUsage {
    let Some(usage) = response.get("usage") else {
        return TokenUsage::default();
    };

    // OpenAI format
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(serde_json::Value::as_u64)
        // Anthropic format
        .or_else(|| {
            usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0);

    let output_tokens = usage
        .get("completion_tokens")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0);

    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(serde_json::Value::as_u64)
        // OpenAI format uses prompt_tokens_details.cached_tokens
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0);

    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
    }
}

/// Extract token usage from an SSE data line (JSON).
///
/// For Anthropic: look for `message_delta` with `usage` in the top-level.
/// For `OpenAI`: look for the final chunk with a `usage` field (when
/// `stream_options.include_usage` is set).
///
/// Returns `Some(TokenUsage)` if usage was found, `None` otherwise.
#[must_use]
pub fn extract_usage_from_sse_event(json: &Value) -> Option<TokenUsage> {
    // Anthropic: message_delta event carries cumulative usage at the top level
    if json.get("type").and_then(|t| t.as_str()) == Some("message_delta") {
        if let Some(usage) = json.get("usage") {
            let output_tokens = usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            // message_delta usually only has output_tokens; input is on message_start
            if output_tokens > 0 {
                return Some(TokenUsage {
                    input_tokens: 0,
                    output_tokens,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                });
            }
        }
    }

    // Anthropic: message_start carries input usage
    if json.get("type").and_then(|t| t.as_str()) == Some("message_start") {
        if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
            let input_tokens = usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let cache_write = usage
                .get("cache_creation_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if input_tokens > 0 || cache_read > 0 || cache_write > 0 {
                return Some(TokenUsage {
                    input_tokens,
                    output_tokens: 0,
                    cache_read_tokens: cache_read,
                    cache_write_tokens: cache_write,
                });
            }
        }
    }

    // OpenAI: final chunk with usage field (when stream_options.include_usage is true)
    if let Some(usage) = json.get("usage") {
        if usage.is_object() {
            let u = extract_usage_from_response(json);
            if u.total() > 0 {
                return Some(u);
            }
        }
    }

    None
}

/// SSE stream timeout duration: if no data arrives within this window,
/// the stream is considered stalled.
pub const SSE_STREAM_TIMEOUT_SECS: u64 = 30;

/// Forward request to upstream provider.
///
/// `api_key` is an [`ApiKey`] newtype — the raw secret only leaves it at
/// the adapter's `.get_headers(api_key)` call, which is the single audited
/// boundary where headers are constructed. See crosslink #256 and #338.
///
/// Auth headers are produced by the provider's
/// [`ProviderAdapter::get_headers`] implementation, not by a local
/// substring test on `base_url`. Previously three separate locations
/// (`forward_to_provider`, `set_auth_header`, and
/// `proxy_anthropic_messages`) each branched on a different discriminator
/// (URL substring vs. provider-name equality vs. hardcoded literal); now
/// only the adapter matters. Adding a new provider with unusual auth is
/// a one-file change instead of four.
async fn forward_to_provider<T: Serialize + Sync>(
    client: &Client,
    provider: &ProviderConfig,
    provider_name: &str,
    api_key: &ApiKey,
    path: &str,
    body: &T,
    is_stream: bool,
) -> Result<Response, ProxyError> {
    let url = format!("{}{}", normalize_base_url(&provider.base_url), path);
    debug!(url = %url, stream = is_stream, "Forwarding to provider");

    let mut req = client.post(&url).json(body);

    // Provider-owned auth and protocol headers.
    let adapter = crate::providers::get_adapter(provider_name);
    for (key, value) in adapter.get_headers(api_key) {
        req = req.header(key.as_str(), value.as_str());
    }

    // Operator-supplied passthrough headers from config (these override
    // the adapter's defaults — reqwest uses last-write-wins semantics).
    for (key, value) in &provider.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    let response = req.send().await?;
    convert_response(response).await
}

/// Forward request to upstream provider with raw Value body and custom headers.
/// Returns the raw `reqwest::Response` for inspection before conversion.
async fn forward_to_provider_raw_reqwest(
    client: &Client,
    provider: &ProviderConfig,
    _api_key: &ApiKey,
    path: &str,
    body: &Value,
    is_stream: bool,
    custom_headers: Vec<(String, String)>,
) -> Result<reqwest::Response, ProxyError> {
    let url = format!("{}{}", normalize_base_url(&provider.base_url), path);
    debug!(url = %url, stream = is_stream, "Forwarding to provider (raw/reqwest)");

    let mut req = client.post(&url).json(body);

    for (key, value) in custom_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    for (key, value) in &provider.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    Ok(req.send().await?)
}

/// Convert reqwest response to axum response
async fn convert_response(response: reqwest::Response) -> Result<Response, ProxyError> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = Response::builder().status(status);

    // Copy response headers
    for (key, value) in response.headers() {
        if key != header::TRANSFER_ENCODING && key != header::CONTENT_LENGTH {
            if let Ok(v) = HeaderValue::from_bytes(value.as_bytes()) {
                builder = builder.header(key.as_str(), v);
            }
        }
    }

    let body = response.bytes().await?;

    // If the response is HTML (error page from CDN/proxy), convert to a
    // clean JSON error instead of dumping raw HTML to the terminal.
    if !status.is_success() {
        let body_str = String::from_utf8_lossy(&body);
        if body_str.trim_start().starts_with('<') || body_str.contains("<!DOCTYPE") {
            let clean_error = serde_json::json!({
                "error": {
                    "type": "upstream_error",
                    "message": format!("Provider returned HTTP {status} with HTML error page"),
                    "status": status.as_u16()
                }
            });
            let json_body = serde_json::to_string(&clean_error).unwrap_or_default();
            return builder
                .header("content-type", "application/json")
                .body(Body::from(json_body))
                .map_err(|e| ProxyError::InvalidBody(format!("Failed to build error body: {e}")));
        }
    }

    builder
        .body(Body::from(body))
        .map_err(|e| ProxyError::InvalidBody(format!("Failed to build response body: {e}")))
}

/// Build a `ProxyState` from the given config, initializing all subsystems.
async fn build_proxy_state(config: AppConfig) -> anyhow::Result<ProxyState> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    // Load hooks from both OpenClaudia config and Claude Code settings.json
    let claude_hooks = load_claude_code_hooks();
    let merged_hooks = merge_hooks_config(config.hooks.clone(), claude_hooks);
    let hook_engine = HookEngine::new(merged_hooks);

    let rules_engine = RulesEngine::new(".openclaudia/rules");

    // Initialize compactor with default model context
    let compactor = ContextCompactor::new(CompactionConfig::default());

    // Initialize session manager
    let session_manager = Arc::new(RwLock::new(SessionManager::new(
        &config.session.persist_path,
    )));

    // Initialize plugin manager and discover plugins
    let mut plugin_manager = PluginManager::new();
    let plugin_errors = plugin_manager.discover();
    for err in plugin_errors {
        warn!(error = %err, "Plugin discovery error");
    }
    let plugin_manager = Arc::new(plugin_manager);

    // Initialize MCP manager and connect to configured servers
    let mcp_manager = Arc::new(RwLock::new(McpManager::new()));
    connect_mcp_servers(&mcp_manager, &plugin_manager).await;

    // Initialize OAuth store for Claude Max authentication
    let oauth_store = Arc::new(OAuthStore::new());

    // Initialize VDD engine if enabled
    let vdd_engine = if config.vdd.enabled {
        if let Err(e) = config.vdd.validate(&config.proxy.target) {
            anyhow::bail!("VDD configuration error: {e}");
        }
        info!(
            mode = %config.vdd.mode,
            adversary = %config.vdd.adversary.provider,
            "VDD engine enabled"
        );
        Some(Arc::new(tokio::sync::Mutex::new(VddEngine::new(
            &config.vdd,
            &config,
            client.clone(),
        ))))
    } else {
        debug!(
            "VDD is disabled. To enable adversarial review, add vdd.enabled=true to config.yaml"
        );
        None
    };

    Ok(ProxyState {
        config: Arc::new(config),
        client,
        hook_engine,
        rules_engine,
        compactor,
        session_manager,
        plugin_manager,
        mcp_manager,
        oauth_store,
        vdd_engine,
    })
}

/// Connect to all MCP servers discovered through plugins.
async fn connect_mcp_servers(
    mcp_manager: &Arc<RwLock<McpManager>>,
    plugin_manager: &Arc<PluginManager>,
) {
    let mut mcp = mcp_manager.write().await;
    for (plugin, server) in plugin_manager.all_mcp_servers() {
        match server.transport.as_str() {
            "stdio" => {
                if let Some(command) = &server.command {
                    let args: Vec<&str> = server
                        .args
                        .iter()
                        .map(std::string::String::as_str)
                        .collect();
                    match mcp.connect_stdio(&server.name, command, &args).await {
                        Ok(()) => {
                            info!(server = %server.name, plugin = %plugin.name(), "Connected MCP (stdio)");
                        }
                        Err(e) => {
                            warn!(server = %server.name, error = %e, "MCP connect failed");
                        }
                    }
                }
            }
            "http" => {
                if let Some(url) = &server.url {
                    match mcp.connect_http(&server.name, url).await {
                        Ok(()) => {
                            info!(server = %server.name, plugin = %plugin.name(), "Connected MCP (http)");
                        }
                        Err(e) => {
                            warn!(server = %server.name, error = %e, "MCP connect failed");
                        }
                    }
                }
            }
            _ => {
                warn!(server = %server.name, transport = %server.transport, "Unknown MCP transport");
            }
        }
    }
    if mcp.server_count() > 0 {
        info!(connected = mcp.server_count(), "MCP servers initialized");
    }
}

/// Fire the `SessionStart` hook and return the session ID.
async fn fire_session_start(state: &ProxyState) -> String {
    let session_id = {
        let mut sm = state.session_manager.write().await;
        let id = sm.get_or_create_session().id.clone();
        drop(sm);
        id
    };

    let start_input = HookInput::new(HookEvent::SessionStart).with_session_id(&session_id);
    let start_result = state
        .hook_engine
        .run(HookEvent::SessionStart, &start_input)
        .await;

    info!(
        session_id = %session_id,
        hooks_allowed = start_result.allowed,
        "Session started"
    );

    session_id
}

/// Start the proxy server.
///
/// # Errors
///
/// Returns an error if binding the TCP listener or serving fails.
pub async fn start_server(config: AppConfig) -> anyhow::Result<()> {
    let addr = format!("{}:{}", config.proxy.host, config.proxy.port);
    let state = build_proxy_state(config).await?;
    fire_session_start(&state).await;

    let app = create_router(state);

    info!(address = %addr, "Starting OpenClaudia proxy server");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Start the proxy server with graceful shutdown support.
///
/// # Errors
///
/// Returns an error if binding the TCP listener, serving, or VDD
/// configuration validation fails.
pub async fn start_server_with_shutdown(
    config: AppConfig,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let addr = format!("{}:{}", config.proxy.host, config.proxy.port);

    // Build the proxy state + fire SessionStart hook via the SAME
    // helpers that `start_server` uses. The previous implementation of
    // this function duplicated ~150 lines of initialization (Client,
    // hook merging, rules engine, compactor, session manager, plugin
    // discovery, MCP connect loop, OAuth store, VDD engine setup,
    // SessionStart hook). Any change to provisioning had to land in
    // two places — classic stovepipe. See crosslink #246.
    let state = build_proxy_state(config).await?;
    fire_session_start(&state).await;

    let app = create_router(state);

    info!(address = %addr, "Starting OpenClaudia proxy server (with shutdown support)");

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Use axum's graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for shutdown signal
            loop {
                if shutdown_rx.changed().await.is_err() || *shutdown_rx.borrow() {
                    info!("Shutdown signal received, stopping server...");
                    break;
                }
            }
        })
        .await?;

    Ok(())
}
