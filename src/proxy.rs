//! HTTP Proxy Server - The core of OpenClaudia.
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
use crate::hooks::{HookEngine, HookError, HookEvent, HookInput, HookResult};
use crate::mcp::McpManager;
use crate::plugins::PluginManager;
use crate::providers::get_adapter;
use crate::rules::{extract_extensions_from_tool_input, RulesEngine};
use crate::session::{get_session_context, SessionManager};

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
            ProxyError::ProviderNotConfigured(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::NoApiKey(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            ProxyError::RequestError(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            ProxyError::InvalidBody(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::JsonError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::HookBlocked(_) => (StatusCode::FORBIDDEN, self.to_string()),
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
        // OpenAI-compatible endpoints
        .route("/v1/chat/completions", any(proxy_chat_completions))
        .route("/v1/completions", any(proxy_completions))
        .route("/v1/models", get(list_models))
        // Anthropic-compatible endpoints (for direct Anthropic clients)
        .route("/v1/messages", any(proxy_anthropic_messages))
        // Catch-all for other API routes
        .route("/v1/*path", any(proxy_passthrough))
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

/// List available models (returns configured provider's models)
async fn list_models(State(_state): State<ProxyState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "object": "list",
        "data": [
            {"id": "claude-3-5-sonnet-20241022", "object": "model", "owned_by": "anthropic"},
            {"id": "claude-3-5-haiku-20241022", "object": "model", "owned_by": "anthropic"},
            {"id": "claude-3-opus-20240229", "object": "model", "owned_by": "anthropic"},
            {"id": "gpt-4o", "object": "model", "owned_by": "openai"},
            {"id": "gpt-4o-mini", "object": "model", "owned_by": "openai"},
        ]
    }))
}

/// Run PreToolUse hooks for tool calls in the response
async fn run_pre_tool_use_hooks(
    hook_engine: &HookEngine,
    session_id: Option<&str>,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> HookResult {
    // Check for dangerous tool patterns and deny if needed
    let dangerous_patterns = ["rm -rf", "format c:", "drop table", "delete from"];
    if let Some(args_str) = tool_input.as_str() {
        for pattern in dangerous_patterns {
            if args_str.to_lowercase().contains(pattern) {
                return HookResult::denied(format!(
                    "Tool '{}' contains dangerous pattern: {}",
                    tool_name, pattern
                ));
            }
        }
    }

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

/// Extract file extensions from message content (looks for file paths)
fn extract_extensions_from_messages(messages: &[ChatMessage]) -> Vec<String> {
    use std::collections::HashSet;

    let mut extensions = HashSet::new();

    // Simple regex-like pattern to find file extensions in text
    // Matches patterns like: file.rs, /path/to/file.py, src/main.ts
    let extension_pattern = regex::Regex::new(r"[\w/\\.-]+\.([a-zA-Z0-9]{1,10})\b").unwrap();

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

/// Proxy chat completions (OpenAI format)
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

    // Inject context from hook results
    let mut request = request;
    ContextInjector::apply_prompt_modification(&mut request, &hook_result);
    ContextInjector::inject(&mut request, &hook_result);

    // Inject rules based on file extensions mentioned in messages
    let extensions = extract_extensions_from_messages(&request.messages);
    if !extensions.is_empty() {
        let rules_content = state
            .rules_engine
            .get_combined_rules(&extensions.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        if !rules_content.is_empty() {
            ContextInjector::inject_system_prefix(&mut request, &rules_content);
        }
    }

    // Add MCP tools to request if available
    {
        let mcp = state.mcp_manager.read().await;
        let mcp_tools = mcp.tools_as_openai_functions();
        if !mcp_tools.is_empty() {
            let mut tools = request.tools.unwrap_or_default();
            tools.extend(mcp_tools);
            request.tools = Some(tools);
        }
    }

    // Add plugin commands as available context
    let plugin_commands: Vec<String> = state
        .plugin_manager
        .all_commands()
        .iter()
        .map(|(plugin, cmd)| format!("/{} (from {})", cmd.name, plugin.name()))
        .collect();
    if !plugin_commands.is_empty() {
        let commands_context = format!("Available plugin commands: {}", plugin_commands.join(", "));
        ContextInjector::inject_system_suffix(&mut request, &commands_context);
    }

    // Inject session context
    let session_context = {
        let sm = state.session_manager.read().await;
        sm.get_session().map(get_session_context)
    };
    if let Some(context) = session_context {
        // Use inject_all for multiple context items
        ContextInjector::inject_all(&mut request, &[context]);
    }

    // Run PreToolUse hooks if there are tool calls in previous messages
    // This validates tool usage before the model responds
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

                    // Check hook extra data for any additional processing
                    for output in &hook_result.outputs {
                        if let Some(extra_data) = output.extra.get("metadata") {
                            debug!(metadata = %extra_data, "Hook provided extra metadata");
                        }
                    }

                    // Use check_blocked to validate and get proper error
                    if let Err(hook_err) = HookEngine::check_blocked(&hook_result) {
                        let reason = match hook_err {
                            HookError::Blocked(r) => r,
                            _ => "PreToolUse hook blocked".to_string(),
                        };
                        return Err(ProxyError::HookBlocked(format!(
                            "Tool '{}' blocked: {}",
                            name, reason
                        )));
                    }
                }
            }
        }
    }

    // Create model-specific compactor for accurate context limits
    let mut compactor = crate::compaction::ContextCompactor::for_model(&request.model);

    // Optionally update config from state compactor settings
    let base_config = state.compactor.config().clone();
    let mut model_config = compactor.config().clone();
    model_config.preserve_recent = base_config.preserve_recent;
    model_config.preserve_system = base_config.preserve_system;
    model_config.preserve_tool_calls = base_config.preserve_tool_calls;
    compactor.set_config(model_config);

    // Compact context if needed (for long conversations)
    let compaction_result = compactor
        .compact(&mut request, Some(&state.hook_engine), None)
        .await;

    match compaction_result {
        Ok(result) => {
            if result.compacted {
                info!(
                    original = result.original_tokens,
                    new = result.new_tokens,
                    summarized = result.messages_summarized,
                    summary_len = result.summary.as_ref().map(|s| s.len()).unwrap_or(0),
                    "Context compacted"
                );
                // Log the summary if available (for debugging)
                if let Some(summary) = &result.summary {
                    debug!(summary = %summary, "Compaction summary generated");
                }
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

    let is_stream = request.stream.unwrap_or(false);

    // Transform request to provider format
    let transformed_request = adapter
        .transform_request(&request)
        .map_err(|e| ProxyError::InvalidBody(e.to_string()))?;

    // Forward to provider with transformed request
    let response = forward_to_provider_raw(
        &state.client,
        provider,
        &api_key,
        adapter.chat_endpoint(),
        &transformed_request,
        is_stream,
        adapter.get_headers(&api_key),
    )
    .await?;

    Ok(response)
}

/// Handle MCP tool calls from the model response
pub async fn handle_mcp_tool_call(
    mcp_manager: &Arc<RwLock<McpManager>>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ProxyError> {
    let mcp = mcp_manager.read().await;

    // Check if the MCP server is connected
    let parts: Vec<&str> = tool_name.splitn(2, '_').collect();
    if parts.len() >= 2 {
        let server_name = parts[0];
        if !mcp.is_connected(server_name) {
            return Err(ProxyError::InvalidBody(format!(
                "MCP server '{}' is not connected",
                server_name
            )));
        }
    }

    // Call the tool
    match mcp.call_tool(tool_name, arguments).await {
        Ok(result) => Ok(result),
        Err(e) => Err(ProxyError::InvalidBody(format!(
            "MCP tool call failed: {}",
            e
        ))),
    }
}

/// Disconnect all MCP servers gracefully
pub async fn shutdown_mcp(mcp_manager: &Arc<RwLock<McpManager>>) {
    let mut mcp = mcp_manager.write().await;
    if let Err(e) = mcp.disconnect_all().await {
        warn!(error = %e, "Error disconnecting MCP servers");
    }
}

/// Proxy completions (legacy OpenAI format)
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
        &api_key,
        "/v1/completions",
        &request,
        is_stream,
    )
    .await?;

    Ok(response)
}

/// Proxy Anthropic messages endpoint
async fn proxy_anthropic_messages(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, ProxyError> {
    let request: Value =
        serde_json::from_str(&body).map_err(|e| ProxyError::InvalidBody(e.to_string()))?;

    let provider = state
        .config
        .get_provider("anthropic")
        .ok_or_else(|| ProxyError::ProviderNotConfigured("anthropic".to_string()))?;

    let api_key = extract_api_key(&headers)
        .or_else(|| provider.api_key.clone())
        .ok_or_else(|| ProxyError::NoApiKey("anthropic".to_string()))?;

    let is_stream = request["stream"].as_bool().unwrap_or(false);
    let response = forward_to_provider(
        &state.client,
        provider,
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

    let url = format!("{}{}", provider.base_url, path);
    debug!(url = %url, "Passthrough request");

    let mut req_builder = state.client.request(request.method().clone(), &url);

    // Copy relevant headers
    for (key, value) in headers.iter() {
        if key != header::HOST && key != header::CONTENT_LENGTH {
            if let Ok(v) = value.to_str() {
                req_builder = req_builder.header(key.as_str(), v);
            }
        }
    }

    // Set auth header based on provider
    req_builder = set_auth_header(req_builder, &state.config.proxy.target, &api_key);

    let response = req_builder.send().await?;
    convert_response(response).await
}

/// Determine which provider to use based on model name
fn determine_provider(model: &str, config: &AppConfig) -> String {
    let model_lower = model.to_lowercase();
    if model_lower.starts_with("claude") || model_lower.starts_with("anthropic") {
        "anthropic".to_string()
    } else if model_lower.starts_with("gpt")
        || model_lower.starts_with("o1")
        || model_lower.starts_with("o3")
    {
        "openai".to_string()
    } else if model_lower.starts_with("gemini") {
        "google".to_string()
    } else if model_lower.starts_with("glm") {
        // Z.AI/GLM models (OpenAI-compatible)
        "zai".to_string()
    } else if model_lower.starts_with("deepseek") {
        // DeepSeek models (OpenAI-compatible)
        "deepseek".to_string()
    } else if model_lower.starts_with("qwen") {
        // Alibaba Qwen models (OpenAI-compatible)
        "qwen".to_string()
    } else {
        // Fall back to configured target
        config.proxy.target.clone()
    }
}

/// Extract API key from Authorization header
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
        .or_else(|| {
            // Also check x-api-key header (Anthropic style)
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

/// Forward request to upstream provider
async fn forward_to_provider<T: Serialize>(
    client: &Client,
    provider: &ProviderConfig,
    api_key: &str,
    path: &str,
    body: &T,
    is_stream: bool,
) -> Result<Response, ProxyError> {
    let url = format!("{}{}", provider.base_url, path);
    debug!(url = %url, stream = is_stream, "Forwarding to provider");

    let mut req = client.post(&url).json(body);

    // Set provider-specific auth headers
    if provider.base_url.contains("anthropic") {
        req = req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2024-01-01");
    } else {
        req = req.header(header::AUTHORIZATION, format!("Bearer {}", api_key));
    }

    // Add any custom headers from config
    for (key, value) in &provider.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    let response = req.send().await?;
    convert_response(response).await
}

/// Set authentication header based on provider type
fn set_auth_header(
    mut req: reqwest::RequestBuilder,
    provider_name: &str,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if provider_name == "anthropic" {
        req = req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2024-01-01");
    } else {
        req = req.header(header::AUTHORIZATION, format!("Bearer {}", api_key));
    }
    req
}

/// Forward request to upstream provider with raw Value body and custom headers
async fn forward_to_provider_raw(
    client: &Client,
    provider: &ProviderConfig,
    _api_key: &str,
    path: &str,
    body: &Value,
    is_stream: bool,
    custom_headers: Vec<(String, String)>,
) -> Result<Response, ProxyError> {
    let url = format!("{}{}", provider.base_url, path);
    debug!(url = %url, stream = is_stream, "Forwarding to provider (raw)");

    let mut req = client.post(&url).json(body);

    // Apply custom headers from provider adapter
    for (key, value) in custom_headers {
        req = req.header(key.as_str(), value.as_str());
    }

    // Add any custom headers from config
    for (key, value) in &provider.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    let response = req.send().await?;
    convert_response(response).await
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
    Ok(builder.body(Body::from(body)).unwrap())
}

/// Start the proxy server
pub async fn start_server(config: AppConfig) -> anyhow::Result<()> {
    let addr = format!("{}:{}", config.proxy.host, config.proxy.port);

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let hook_engine = HookEngine::new(config.hooks.clone());
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
    {
        let mut mcp = mcp_manager.write().await;
        for (plugin, server) in plugin_manager.all_mcp_servers() {
            match server.transport.as_str() {
                "stdio" => {
                    if let Some(command) = &server.command {
                        let args: Vec<&str> = server.args.iter().map(|s| s.as_str()).collect();
                        match mcp.connect_stdio(&server.name, command, &args).await {
                            Ok(()) => {
                                info!(server = %server.name, plugin = %plugin.name(), "Connected MCP (stdio)")
                            }
                            Err(e) => {
                                warn!(server = %server.name, error = %e, "MCP connect failed")
                            }
                        }
                    }
                }
                "http" => {
                    if let Some(url) = &server.url {
                        match mcp.connect_http(&server.name, url).await {
                            Ok(()) => {
                                info!(server = %server.name, plugin = %plugin.name(), "Connected MCP (http)")
                            }
                            Err(e) => {
                                warn!(server = %server.name, error = %e, "MCP connect failed")
                            }
                        }
                    }
                }
                _ => {
                    warn!(server = %server.name, transport = %server.transport, "Unknown MCP transport")
                }
            }
        }
        if mcp.server_count() > 0 {
            info!(connected = mcp.server_count(), "MCP servers initialized");
        }
    }

    let state = ProxyState {
        config: Arc::new(config),
        client,
        hook_engine,
        rules_engine,
        compactor,
        session_manager,
        plugin_manager,
        mcp_manager,
    };

    // Fire SessionStart hook and inject session context
    let (session_id, session_context) = {
        let mut sm = state.session_manager.write().await;
        let session = sm.get_or_create_session();
        let context = get_session_context(session);
        (session.id.clone(), context)
    };

    let start_input = HookInput::new(HookEvent::SessionStart).with_session_id(&session_id);
    let start_result = state
        .hook_engine
        .run(HookEvent::SessionStart, &start_input)
        .await;

    // Log session context and hook results
    info!(
        session_id = %session_id,
        context_len = session_context.len(),
        hooks_allowed = start_result.allowed,
        "Session started"
    );

    let app = create_router(state);

    info!(address = %addr, "Starting OpenClaudia proxy server");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Start the proxy server with graceful shutdown support
pub async fn start_server_with_shutdown(
    config: AppConfig,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let addr = format!("{}:{}", config.proxy.host, config.proxy.port);

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let hook_engine = HookEngine::new(config.hooks.clone());
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

    // Initialize MCP manager
    let mcp_manager = Arc::new(RwLock::new(McpManager::new()));

    let state = ProxyState {
        config: Arc::new(config),
        client,
        hook_engine,
        rules_engine,
        compactor,
        session_manager,
        plugin_manager,
        mcp_manager,
    };

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
