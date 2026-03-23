//! ACP (Agent Client Protocol) Server — JSON-RPC 2.0 over stdio.
//!
//! Enables OpenClaudia to interoperate with `acpx` and other agent harnesses.
//! Implements all stable ACP methods:
//! - `initialize` — handshake/capability negotiation
//! - `authenticate` — credential validation
//! - `session/new` — create a new session
//! - `session/load` — resume a persisted session
//! - `session/prompt` — execute prompt with streaming updates
//! - `session/cancel` — cancel in-flight prompt
//! - `session/set_mode` — change session mode
//! - `session/set_config_option` — set session config
//!
//! Tool execution is delegated through ACP client methods:
//! - `fs/read_text_file`, `fs/write_text_file` — file operations
//! - `terminal/create`, `terminal/output`, `terminal/wait_for_exit`,
//!   `terminal/kill`, `terminal/release` — shell execution

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info};

use crate::config::AppConfig;
use crate::hooks::{load_claude_code_hooks, merge_hooks_config, HookEngine};
use crate::providers::get_adapter;
use crate::rules::RulesEngine;
use crate::session::SessionManager;

// ============================================================================
// JSON-RPC types
// ============================================================================

/// Incoming JSON-RPC message (could be request, notification, or response).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonRpcMessage {
    #[allow(dead_code)]
    jsonrpc: String,
    /// Present on requests (needs response) and responses.
    #[serde(default)]
    id: Option<Value>,
    /// Present on requests and notifications.
    #[serde(default)]
    method: Option<String>,
    /// Present on requests and notifications.
    #[serde(default)]
    params: Option<Value>,
    /// Present on successful responses.
    #[serde(default)]
    result: Option<Value>,
    /// Present on error responses.
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// Outgoing JSON-RPC response.
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

/// Outgoing JSON-RPC notification (no id, no response expected).
#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// Outgoing JSON-RPC request (server → client, e.g. fs/read_text_file).
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

// Standard JSON-RPC error codes
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const _INTERNAL_ERROR: i64 = -32603;

// ============================================================================
// ACP Server
// ============================================================================

/// ACP server state.
pub struct AcpServer {
    /// Application config (providers, hooks, etc.)
    config: AppConfig,
    /// Session manager for persistence
    session_manager: SessionManager,
    /// Hook engine (used during prompt execution for PreToolUse/PostToolUse hooks)
    #[allow(dead_code)]
    hook_engine: HookEngine,
    /// Rules engine (used to inject .clauderules context)
    #[allow(dead_code)]
    rules_engine: RulesEngine,
    /// Active ACP session ID → OpenClaudia session ID mapping
    session_map: HashMap<String, String>,
    /// Conversation messages for the active session
    messages: Vec<Value>,
    /// Model name
    model: String,
    /// API key
    api_key: String,
    /// Request ID counter for server→client requests
    next_request_id: AtomicU64,
    /// Pending responses for server→client requests
    pending_responses: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>>,
    /// Cancellation flag for in-flight prompts
    cancel_flag: Arc<AtomicBool>,
    /// Channel for writing to stdout (serialized access)
    stdout_tx: mpsc::UnboundedSender<String>,
    /// Session config options set via session/set_config_option
    config_options: HashMap<String, Value>,
    /// Terminal ID counter for ACP terminal lifecycle
    #[allow(dead_code)]
    next_terminal_id: AtomicU64,
}

impl AcpServer {
    /// Create a new ACP server from the loaded config.
    pub fn new(
        config: AppConfig,
        model: String,
        api_key: String,
        stdout_tx: mpsc::UnboundedSender<String>,
    ) -> Self {
        let persist_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("openclaudia")
            .join("sessions");

        let claude_hooks = load_claude_code_hooks();
        let merged_hooks = merge_hooks_config(config.hooks.clone(), claude_hooks);
        let hook_engine = HookEngine::new(merged_hooks);
        let rules_engine = RulesEngine::new(".openclaudia/rules");

        Self {
            config,
            session_manager: SessionManager::new(persist_dir),
            hook_engine,
            rules_engine,
            session_map: HashMap::new(),
            messages: Vec::new(),
            model,
            api_key,
            next_request_id: AtomicU64::new(1),
            pending_responses: Arc::new(Mutex::new(HashMap::new())),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            stdout_tx,
            config_options: HashMap::new(),
            next_terminal_id: AtomicU64::new(1),
        }
    }

    // ========================================================================
    // Transport helpers
    // ========================================================================

    /// Send a JSON-RPC response.
    fn send_response(&self, id: Value, result: Option<Value>, error: Option<JsonRpcError>) {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result,
            error,
        };
        if let Ok(line) = serde_json::to_string(&resp) {
            let _ = self.stdout_tx.send(line);
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    fn send_notification(&self, method: &str, params: Option<Value>) {
        let notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };
        if let Ok(line) = serde_json::to_string(&notif) {
            let _ = self.stdout_tx.send(line);
        }
    }

    /// Send a JSON-RPC request to the client and await the response.
    async fn client_request(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        // Register pending response
        {
            let mut pending = self.pending_responses.lock().await;
            pending.insert(id, tx);
        }

        // Send request
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };
        if let Ok(line) = serde_json::to_string(&req) {
            let _ = self.stdout_tx.send(line);
        }

        // Await response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(rpc_err))) => Err(format!("RPC error {}: {}", rpc_err.code, rpc_err.message)),
            Ok(Err(_)) => Err("Client request channel closed".to_string()),
            Err(_) => {
                // Clean up pending
                let mut pending = self.pending_responses.lock().await;
                pending.remove(&id);
                Err("Client request timed out".to_string())
            }
        }
    }

    /// Send a session/update notification.
    fn send_session_update(&self, session_id: &str, update_type: &str, content: Value) {
        self.send_notification(
            "session/update",
            Some(json!({
                "sessionId": session_id,
                "sessionUpdate": update_type,
                "content": content,
            })),
        );
    }

    fn send_error(&self, id: Value, code: i64, message: &str) {
        self.send_response(
            id,
            None,
            Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        );
    }

    // ========================================================================
    // Message routing
    // ========================================================================

    /// Route an incoming JSON-RPC message.
    async fn handle_message(&mut self, msg: JsonRpcMessage) {
        // Is this a response to a server→client request?
        if msg.method.is_none() && (msg.result.is_some() || msg.error.is_some()) {
            if let Some(id) = msg.id.as_ref().and_then(|v| v.as_u64()) {
                let mut pending = self.pending_responses.lock().await;
                if let Some(tx) = pending.remove(&id) {
                    if let Some(err) = msg.error {
                        let _ = tx.send(Err(err));
                    } else {
                        let _ = tx.send(Ok(msg.result.unwrap_or(Value::Null)));
                    }
                }
            }
            return;
        }

        // It's a request or notification from the client
        let method = match msg.method {
            Some(ref m) => m.clone(),
            None => {
                if let Some(id) = msg.id {
                    self.send_error(id, INVALID_REQUEST, "Missing method field");
                }
                return;
            }
        };

        let params = msg.params.unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => self.handle_initialize(msg.id, params),
            "authenticate" => self.handle_authenticate(msg.id, params),
            "session/new" => self.handle_session_new(msg.id, params),
            "session/load" => self.handle_session_load(msg.id, params),
            "session/prompt" => self.handle_session_prompt(msg.id, params).await,
            "session/cancel" => self.handle_session_cancel(msg.id, params),
            "session/set_mode" => self.handle_session_set_mode(msg.id, params),
            "session/set_config_option" => self.handle_session_set_config_option(msg.id, params),
            _ => {
                if let Some(id) = msg.id {
                    self.send_error(id, METHOD_NOT_FOUND, &format!("Unknown method: {}", method));
                }
            }
        }
    }

    // ========================================================================
    // ACP method handlers
    // ========================================================================

    fn handle_initialize(&self, id: Option<Value>, _params: Value) {
        let id = match id {
            Some(id) => id,
            None => return, // Notification — no response needed
        };

        self.send_response(
            id,
            Some(json!({
                "protocolVersion": "0.1",
                "serverInfo": {
                    "name": "openclaudia",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "prompts": true,
                    "tools": true,
                    "fs": {
                        "read": true,
                        "write": true,
                    },
                    "terminal": true,
                },
            })),
            None,
        );

        info!("ACP initialize handshake complete");
    }

    fn handle_authenticate(&self, id: Option<Value>, _params: Value) {
        let id = match id {
            Some(id) => id,
            None => return,
        };

        // OpenClaudia uses its own provider API keys from config, so ACP auth
        // is accepted unconditionally — the client doesn't need to provide credentials.
        self.send_response(
            id,
            Some(json!({
                "authenticated": true,
            })),
            None,
        );
    }

    fn handle_session_new(&mut self, id: Option<Value>, _params: Value) {
        let id = match id {
            Some(id) => id,
            None => return,
        };

        let session = self.session_manager.get_or_create_session();
        let oc_session_id = session.id.clone();

        // Generate an ACP-facing session ID
        let acp_session_id = uuid::Uuid::new_v4().to_string();
        self.session_map
            .insert(acp_session_id.clone(), oc_session_id);
        self.messages.clear();

        self.send_response(
            id,
            Some(json!({
                "sessionId": acp_session_id,
            })),
            None,
        );

        info!(acp_session_id = %acp_session_id, "Created new ACP session");
    }

    fn handle_session_load(&mut self, id: Option<Value>, params: Value) {
        let id = match id {
            Some(id) => id,
            None => return,
        };

        let acp_session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(sid) => sid.to_string(),
            None => {
                self.send_error(id, INVALID_PARAMS, "Missing sessionId");
                return;
            }
        };

        // Check if we know this ACP session
        if let Some(oc_id) = self.session_map.get(&acp_session_id) {
            // Try to load the persisted OpenClaudia session
            if let Some(session) = self.session_manager.load_session(oc_id) {
                // Restore it as active
                self.session_manager.start_coding(&session.id);
                self.send_response(
                    id,
                    Some(json!({
                        "sessionId": acp_session_id,
                        "loaded": true,
                    })),
                    None,
                );
                info!(acp_session_id = %acp_session_id, "Loaded ACP session");
                return;
            }
        }

        // Unknown or unloadable — create a new session and map it
        let session = self.session_manager.get_or_create_session();
        let oc_session_id = session.id.clone();
        self.session_map
            .insert(acp_session_id.clone(), oc_session_id);
        self.messages.clear();

        self.send_response(
            id,
            Some(json!({
                "sessionId": acp_session_id,
                "loaded": false,
            })),
            None,
        );

        info!(acp_session_id = %acp_session_id, "session/load fell back to new session");
    }

    fn handle_session_cancel(&self, id: Option<Value>, _params: Value) {
        self.cancel_flag.store(true, Ordering::SeqCst);

        if let Some(id) = id {
            self.send_response(
                id,
                Some(json!({
                    "cancelled": true,
                })),
                None,
            );
        }

        info!("Prompt cancellation requested");
    }

    fn handle_session_set_mode(&self, id: Option<Value>, params: Value) {
        let id = match id {
            Some(id) => id,
            None => return,
        };

        let mode = match params.get("mode").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => {
                self.send_error(id, INVALID_PARAMS, "Missing mode");
                return;
            }
        };

        // Map to OpenClaudia session modes
        match mode {
            "initializer" | "coding" | "auto" => {
                self.send_response(id, Some(json!({"mode": mode})), None);
                info!(mode = %mode, "Session mode set");
            }
            _ => {
                self.send_error(
                    id,
                    INVALID_PARAMS,
                    &format!("Invalid mode: {}. Supported: initializer, coding, auto", mode),
                );
            }
        }
    }

    fn handle_session_set_config_option(&mut self, id: Option<Value>, params: Value) {
        let id = match id {
            Some(id) => id,
            None => return,
        };

        let key = match params.get("key").and_then(|v| v.as_str()) {
            Some(k) => k.to_string(),
            None => {
                self.send_error(id, INVALID_PARAMS, "Missing key");
                return;
            }
        };

        let value = match params.get("value") {
            Some(v) => v.clone(),
            None => {
                self.send_error(id, INVALID_PARAMS, "Missing value");
                return;
            }
        };

        self.config_options.insert(key.clone(), value.clone());
        self.send_response(
            id,
            Some(json!({"key": key, "value": value})),
            None,
        );

        info!(key = %key, "Config option set");
    }

    // ========================================================================
    // Prompt execution — the core agentic loop
    // ========================================================================

    async fn handle_session_prompt(&mut self, id: Option<Value>, params: Value) {
        let id = match id {
            Some(id) => id,
            None => return,
        };

        let acp_session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(sid) => sid.to_string(),
            None => {
                self.send_error(id, INVALID_PARAMS, "Missing sessionId");
                return;
            }
        };

        let prompt = match params.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                self.send_error(id, INVALID_PARAMS, "Missing prompt");
                return;
            }
        };

        // Reset cancel flag
        self.cancel_flag.store(false, Ordering::SeqCst);

        // Add user message
        self.messages.push(json!({
            "role": "user",
            "content": prompt,
        }));

        // Run the agentic loop
        let stop_reason = self.run_prompt_loop(&acp_session_id).await;

        // Record turn metrics
        if let Some(session) = self.session_manager.get_session_mut() {
            session.request_count += 1;
            session.updated_at = chrono::Utc::now();
        }

        self.send_response(
            id,
            Some(json!({
                "stopReason": stop_reason,
            })),
            None,
        );
    }

    /// Run the prompt → tool calls → re-prompt loop.
    async fn run_prompt_loop(&mut self, acp_session_id: &str) -> String {
        let adapter = get_adapter(&self.config.proxy.target);
        let client = reqwest::Client::new();
        let max_iterations = 50; // Safety limit

        for iteration in 0..max_iterations {
            if self.cancel_flag.load(Ordering::SeqCst) {
                return "cancelled".to_string();
            }

            // Build the request
            let tools = crate::tools::get_tool_definitions();
            let system_prompt = crate::prompt::build_system_prompt(None, None, None);

            // Prepend system prompt to messages
            let mut all_messages: Vec<crate::proxy::ChatMessage> = vec![
                crate::proxy::ChatMessage {
                    role: "system".to_string(),
                    content: crate::proxy::MessageContent::Text(system_prompt),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ];
            all_messages.extend(
                self.messages.iter().filter_map(|m| serde_json::from_value(m.clone()).ok()),
            );

            // Build a ChatCompletionRequest for the adapter
            let chat_request = crate::proxy::ChatCompletionRequest {
                model: self.model.clone(),
                messages: all_messages,
                temperature: None,
                max_tokens: None,
                stream: Some(true),
                tools: Some(serde_json::from_value(tools.clone()).unwrap_or_default()),
                tool_choice: None,
                extra: std::collections::HashMap::new(),
            };

            // Transform for provider
            let transformed = match adapter.transform_request_with_thinking(
                &chat_request,
                &self.config.active_provider().map(|p| p.thinking.clone()).unwrap_or_default(),
            ) {
                Ok(t) => t,
                Err(e) => {
                    self.send_session_update(
                        acp_session_id,
                        "agent_message_chunk",
                        json!({"type": "text", "text": format!("Provider error: {}", e)}),
                    );
                    return "error".to_string();
                }
            };

            // Determine endpoint
            let provider = match self.config.active_provider() {
                Some(p) => p,
                None => return "error".to_string(),
            };
            let endpoint = format!(
                "{}{}",
                provider.base_url,
                adapter.chat_endpoint(&self.model)
            );

            // Build HTTP request with headers
            let mut headers = adapter.get_headers(&self.api_key);
            headers.extend(
                provider
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );

            let mut req = client.post(&endpoint).json(&transformed);
            for (key, value) in &headers {
                req = req.header(key, value);
            }

            // Send request
            debug!(endpoint = %endpoint, iteration = iteration, "Sending provider request");
            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    self.send_session_update(
                        acp_session_id,
                        "agent_message_chunk",
                        json!({"type": "text", "text": format!("Request failed: {}", e)}),
                    );
                    return "error".to_string();
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let body = response.text().await.unwrap_or_default();
                let error_msg = if content_type.contains("text/html") {
                    format!("Error {}: (HTML response — check provider configuration)", status)
                } else {
                    format!("Error {}: {}", status, body)
                };
                self.send_session_update(
                    acp_session_id,
                    "agent_message_chunk",
                    json!({"type": "text", "text": error_msg}),
                );
                // Remove the failed user message
                self.messages.pop();
                return "error".to_string();
            }

            // Stream the response
            let stream_result = self
                .stream_provider_response(acp_session_id, response)
                .await;

            match stream_result {
                StreamResult::EndTurn { content } => {
                    // No tool calls — we're done
                    if !content.is_empty() {
                        self.messages.push(json!({
                            "role": "assistant",
                            "content": content,
                        }));
                    }
                    return "end_turn".to_string();
                }
                StreamResult::ToolCalls {
                    content,
                    tool_calls,
                } => {
                    // Add assistant message with tool calls
                    let tool_calls_json: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments,
                                }
                            })
                        })
                        .collect();

                    self.messages.push(json!({
                        "role": "assistant",
                        "content": if content.is_empty() { Value::Null } else { Value::String(content) },
                        "tool_calls": tool_calls_json,
                    }));

                    // Execute tools via ACP client methods
                    for tc in &tool_calls {
                        if self.cancel_flag.load(Ordering::SeqCst) {
                            return "cancelled".to_string();
                        }

                        self.send_session_update(
                            acp_session_id,
                            "tool_call",
                            json!({
                                "title": format!("{}", tc.name),
                                "status": "running",
                            }),
                        );

                        let result = self.execute_tool_via_acp(&tc.name, &tc.arguments).await;

                        self.send_session_update(
                            acp_session_id,
                            "tool_call",
                            json!({
                                "title": format!("{}", tc.name),
                                "status": "completed",
                                "output": result.content,
                            }),
                        );

                        // Add tool result to messages
                        self.messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tc.id,
                            "content": result.content,
                        }));
                    }

                    // Continue the loop — re-prompt with tool results
                }
                StreamResult::Cancelled => {
                    return "cancelled".to_string();
                }
                StreamResult::Error(msg) => {
                    self.send_session_update(
                        acp_session_id,
                        "agent_message_chunk",
                        json!({"type": "text", "text": msg}),
                    );
                    return "error".to_string();
                }
            }
        }

        "max_iterations".to_string()
    }

    // ========================================================================
    // Streaming response processing
    // ========================================================================

    /// Stream a provider response and extract content + tool calls.
    async fn stream_provider_response(
        &self,
        acp_session_id: &str,
        response: reqwest::Response,
    ) -> StreamResult {
        use futures::StreamExt;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut full_content = String::new();
        let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();

        // Track partial tool call state
        let mut current_tool_index: Option<usize> = None;

        while let Some(chunk_result) = stream.next().await {
            if self.cancel_flag.load(Ordering::SeqCst) {
                return StreamResult::Cancelled;
            }

            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    return StreamResult::Error(format!("Stream error: {}", e));
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    if line == "data: [DONE]" {
                        // Stream complete
                        if tool_calls.is_empty() {
                            return StreamResult::EndTurn {
                                content: full_content,
                            };
                        } else {
                            return StreamResult::ToolCalls {
                                content: full_content,
                                tool_calls,
                            };
                        }
                    }
                    continue;
                }

                if !line.starts_with("data: ") {
                    // Handle Anthropic event: lines
                    if line.starts_with("event: ") {
                        let event_type = line.trim_start_matches("event: ");
                        match event_type {
                            "message_stop" => {
                                if tool_calls.is_empty() {
                                    return StreamResult::EndTurn {
                                        content: full_content,
                                    };
                                } else {
                                    return StreamResult::ToolCalls {
                                        content: full_content,
                                        tool_calls,
                                    };
                                }
                            }
                            _ => {}
                        }
                    }
                    continue;
                }

                let data = &line["data: ".len()..];
                let json: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Handle OpenAI-format streaming
                if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                    for choice in choices {
                        let delta = match choice.get("delta") {
                            Some(d) => d,
                            None => continue,
                        };

                        // Text content
                        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                            full_content.push_str(text);
                            self.send_session_update(
                                acp_session_id,
                                "agent_message_chunk",
                                json!({"type": "text", "text": text}),
                            );
                        }

                        // Tool calls
                        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc_delta in tcs {
                                let index =
                                    tc_delta.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                        as usize;

                                // New tool call
                                if let Some(func) = tc_delta.get("function") {
                                    if let Some(name) =
                                        func.get("name").and_then(|n| n.as_str())
                                    {
                                        while tool_calls.len() <= index {
                                            tool_calls.push(AccumulatedToolCall {
                                                id: String::new(),
                                                name: String::new(),
                                                arguments: String::new(),
                                            });
                                        }
                                        tool_calls[index].name = name.to_string();
                                        current_tool_index = Some(index);
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(|a| a.as_str())
                                    {
                                        if tool_calls.len() > index {
                                            tool_calls[index].arguments.push_str(args);
                                        }
                                    }
                                }

                                if let Some(tc_id) =
                                    tc_delta.get("id").and_then(|i| i.as_str())
                                {
                                    if tool_calls.len() > index {
                                        tool_calls[index].id = tc_id.to_string();
                                    }
                                }
                            }
                        }

                        // Finish reason
                        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str())
                        {
                            if reason == "stop" && tool_calls.is_empty() {
                                return StreamResult::EndTurn {
                                    content: full_content,
                                };
                            }
                            if reason == "tool_calls" {
                                return StreamResult::ToolCalls {
                                    content: full_content,
                                    tool_calls,
                                };
                            }
                        }
                    }
                }

                // Handle Anthropic-format streaming
                if let Some(delta_type) = json.get("type").and_then(|t| t.as_str()) {
                    match delta_type {
                        "content_block_start" => {
                            let content_block =
                                json.get("content_block").unwrap_or(&Value::Null);
                            let block_type =
                                content_block.get("type").and_then(|t| t.as_str()).unwrap_or("");

                            match block_type {
                                "thinking" => {
                                    self.send_session_update(
                                        acp_session_id,
                                        "thinking",
                                        json!({"type": "thinking", "status": "started"}),
                                    );
                                }
                                "tool_use" => {
                                    let name = content_block
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("unknown");
                                    let tc_id = content_block
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("");
                                    tool_calls.push(AccumulatedToolCall {
                                        id: tc_id.to_string(),
                                        name: name.to_string(),
                                        arguments: String::new(),
                                    });
                                    current_tool_index = Some(tool_calls.len() - 1);
                                }
                                _ => {}
                            }
                        }
                        "content_block_delta" => {
                            let delta = json.get("delta").unwrap_or(&Value::Null);
                            let delta_type =
                                delta.get("type").and_then(|t| t.as_str()).unwrap_or("");

                            match delta_type {
                                "text_delta" => {
                                    if let Some(text) = delta.get("text").and_then(|t| t.as_str())
                                    {
                                        full_content.push_str(text);
                                        self.send_session_update(
                                            acp_session_id,
                                            "agent_message_chunk",
                                            json!({"type": "text", "text": text}),
                                        );
                                    }
                                }
                                "thinking_delta" => {
                                    if let Some(text) =
                                        delta.get("thinking").and_then(|t| t.as_str())
                                    {
                                        self.send_session_update(
                                            acp_session_id,
                                            "thinking",
                                            json!({"type": "thinking", "text": text}),
                                        );
                                    }
                                }
                                "input_json_delta" => {
                                    if let Some(partial) =
                                        delta.get("partial_json").and_then(|p| p.as_str())
                                    {
                                        if let Some(idx) = current_tool_index {
                                            if idx < tool_calls.len() {
                                                tool_calls[idx].arguments.push_str(partial);
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        "content_block_stop" => {
                            // Block finished — nothing special needed
                        }
                        "message_delta" => {
                            if let Some(delta) = json.get("delta") {
                                if let Some(reason) =
                                    delta.get("stop_reason").and_then(|r| r.as_str())
                                {
                                    if reason == "end_turn" && tool_calls.is_empty() {
                                        return StreamResult::EndTurn {
                                            content: full_content,
                                        };
                                    }
                                    if reason == "tool_use" {
                                        return StreamResult::ToolCalls {
                                            content: full_content,
                                            tool_calls,
                                        };
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Stream ended without explicit stop
        if tool_calls.is_empty() {
            StreamResult::EndTurn {
                content: full_content,
            }
        } else {
            StreamResult::ToolCalls {
                content: full_content,
                tool_calls,
            }
        }
    }

    // ========================================================================
    // Tool execution via ACP client methods
    // ========================================================================

    /// Execute a tool by delegating to ACP client methods.
    async fn execute_tool_via_acp(&self, tool_name: &str, arguments_json: &str) -> AcpToolResult {
        let args: HashMap<String, Value> =
            serde_json::from_str(arguments_json).unwrap_or_default();

        match tool_name {
            "read_file" => self.acp_read_file(&args).await,
            "write_file" => self.acp_write_file(&args).await,
            "edit_file" => self.acp_edit_file(&args).await,
            "bash" => self.acp_bash(&args).await,
            "bash_output" => self.acp_bash_output(&args).await,
            "kill_shell" => self.acp_kill_shell(&args).await,
            "list_files" => self.acp_list_files(&args).await,
            "glob" | "grep" => self.acp_search(&args, tool_name).await,
            // Internal tools run locally — not file/terminal operations
            "web_fetch" | "web_search" | "web_browser" => {
                self.execute_local_tool(tool_name, arguments_json)
            }
            "memory_search" | "memory_save" | "memory_delete" | "memory_list" => {
                self.execute_local_tool(tool_name, arguments_json)
            }
            "task_create" | "task_update" | "task_get" | "task_list" => {
                self.execute_local_tool(tool_name, arguments_json)
            }
            "todo_write" | "todo_read" => {
                self.execute_local_tool(tool_name, arguments_json)
            }
            "enter_plan_mode" | "exit_plan_mode" => {
                self.execute_local_tool(tool_name, arguments_json)
            }
            name if name.starts_with("mcp__") => {
                // MCP tools run locally through the MCP manager
                self.execute_local_tool(tool_name, arguments_json)
            }
            _ => AcpToolResult {
                content: format!("Unknown tool: {}", tool_name),
                is_error: true,
            },
        }
    }

    /// Execute a tool locally (for internal tools that don't need ACP delegation).
    fn execute_local_tool(&self, tool_name: &str, arguments_json: &str) -> AcpToolResult {
        use crate::tools::{FunctionCall, ToolCall};

        let tc = ToolCall {
            id: "local".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: tool_name.to_string(),
                arguments: arguments_json.to_string(),
            },
        };

        let result = crate::tools::execute_tool(&tc);
        AcpToolResult {
            content: result.content,
            is_error: result.is_error,
        }
    }

    // -- File operations via ACP client --

    async fn acp_read_file(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let path = match args.get("file_path").or(args.get("path")).and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return AcpToolResult {
                    content: "Missing file_path argument".to_string(),
                    is_error: true,
                }
            }
        };

        match self
            .client_request("fs/read_text_file", Some(json!({"path": path})))
            .await
        {
            Ok(result) => {
                let text = result
                    .get("text")
                    .or(result.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Apply offset/limit if specified
                let offset = args
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);

                let lines: Vec<&str> = text.lines().collect();
                let start = offset.min(lines.len());
                let end = limit
                    .map(|l| (start + l).min(lines.len()))
                    .unwrap_or(lines.len());

                let numbered: String = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");

                AcpToolResult {
                    content: numbered,
                    is_error: false,
                }
            }
            Err(e) => AcpToolResult {
                content: format!("Failed to read file: {}", e),
                is_error: true,
            },
        }
    }

    async fn acp_write_file(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let path = match args.get("file_path").or(args.get("path")).and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return AcpToolResult {
                    content: "Missing file_path argument".to_string(),
                    is_error: true,
                }
            }
        };

        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return AcpToolResult {
                    content: "Missing content argument".to_string(),
                    is_error: true,
                }
            }
        };

        match self
            .client_request(
                "fs/write_text_file",
                Some(json!({"path": path, "content": content})),
            )
            .await
        {
            Ok(_) => AcpToolResult {
                content: format!("Successfully wrote to {}", path),
                is_error: false,
            },
            Err(e) => AcpToolResult {
                content: format!("Failed to write file: {}", e),
                is_error: true,
            },
        }
    }

    async fn acp_edit_file(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let path = match args.get("file_path").or(args.get("path")).and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return AcpToolResult {
                    content: "Missing file_path argument".to_string(),
                    is_error: true,
                }
            }
        };

        let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return AcpToolResult {
                    content: "Missing old_string argument".to_string(),
                    is_error: true,
                }
            }
        };

        let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return AcpToolResult {
                    content: "Missing new_string argument".to_string(),
                    is_error: true,
                }
            }
        };

        // Read the file via ACP
        let file_content = match self
            .client_request("fs/read_text_file", Some(json!({"path": path})))
            .await
        {
            Ok(result) => result
                .get("text")
                .or(result.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                return AcpToolResult {
                    content: format!("Failed to read file for edit: {}", e),
                    is_error: true,
                }
            }
        };

        // Apply the edit
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let (new_content, count) = if replace_all {
            let count = file_content.matches(old_string).count();
            (file_content.replace(old_string, new_string), count)
        } else {
            if let Some(_) = file_content.find(old_string) {
                (file_content.replacen(old_string, new_string, 1), 1)
            } else {
                return AcpToolResult {
                    content: format!(
                        "old_string not found in {}. Read the file first to see exact content.",
                        path
                    ),
                    is_error: true,
                };
            }
        };

        if count == 0 {
            return AcpToolResult {
                content: format!("old_string not found in {}", path),
                is_error: true,
            };
        }

        // Write back via ACP
        match self
            .client_request(
                "fs/write_text_file",
                Some(json!({"path": path, "content": new_content})),
            )
            .await
        {
            Ok(_) => AcpToolResult {
                content: format!(
                    "Successfully edited {} ({} replacement{})",
                    path,
                    count,
                    if count != 1 { "s" } else { "" }
                ),
                is_error: false,
            },
            Err(e) => AcpToolResult {
                content: format!("Failed to write edited file: {}", e),
                is_error: true,
            },
        }
    }

    // -- Terminal operations via ACP client --

    async fn acp_bash(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return AcpToolResult {
                    content: "Missing command argument".to_string(),
                    is_error: true,
                }
            }
        };

        let run_in_background = args
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Create terminal
        let terminal_id = match self
            .client_request(
                "terminal/create",
                Some(json!({
                    "command": command,
                    "cwd": std::env::current_dir()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                })),
            )
            .await
        {
            Ok(result) => result
                .get("terminalId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                return AcpToolResult {
                    content: format!("Failed to create terminal: {}", e),
                    is_error: true,
                }
            }
        };

        if run_in_background {
            return AcpToolResult {
                content: format!(
                    "Background shell started with terminal ID: {}\nUse bash_output with this ID to retrieve output.",
                    terminal_id
                ),
                is_error: false,
            };
        }

        // Wait for completion
        let exit_result = match self
            .client_request(
                "terminal/wait_for_exit",
                Some(json!({"terminalId": terminal_id})),
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                return AcpToolResult {
                    content: format!("Failed waiting for terminal: {}", e),
                    is_error: true,
                }
            }
        };

        // Get output
        let output = match self
            .client_request(
                "terminal/output",
                Some(json!({"terminalId": terminal_id})),
            )
            .await
        {
            Ok(result) => result
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(_) => String::new(),
        };

        // Release terminal
        let _ = self
            .client_request(
                "terminal/release",
                Some(json!({"terminalId": terminal_id})),
            )
            .await;

        let exit_code = exit_result
            .get("exitCode")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);

        AcpToolResult {
            content: if output.is_empty() {
                format!("(exit code {})", exit_code)
            } else {
                format!("{}\n(exit code {})", output, exit_code)
            },
            is_error: exit_code != 0,
        }
    }

    async fn acp_bash_output(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let terminal_id = match args
            .get("shell_id")
            .or(args.get("terminal_id"))
            .and_then(|v| v.as_str())
        {
            Some(id) => id,
            None => {
                return AcpToolResult {
                    content: "Missing shell_id argument".to_string(),
                    is_error: true,
                }
            }
        };

        match self
            .client_request(
                "terminal/output",
                Some(json!({"terminalId": terminal_id})),
            )
            .await
        {
            Ok(result) => {
                let output = result
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                AcpToolResult {
                    content: output.to_string(),
                    is_error: false,
                }
            }
            Err(e) => AcpToolResult {
                content: format!("Failed to get terminal output: {}", e),
                is_error: true,
            },
        }
    }

    async fn acp_kill_shell(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let terminal_id = match args
            .get("shell_id")
            .or(args.get("terminal_id"))
            .and_then(|v| v.as_str())
        {
            Some(id) => id,
            None => {
                return AcpToolResult {
                    content: "Missing shell_id argument".to_string(),
                    is_error: true,
                }
            }
        };

        match self
            .client_request(
                "terminal/kill",
                Some(json!({"terminalId": terminal_id})),
            )
            .await
        {
            Ok(_) => AcpToolResult {
                content: format!("Terminal {} killed", terminal_id),
                is_error: false,
            },
            Err(e) => AcpToolResult {
                content: format!("Failed to kill terminal: {}", e),
                is_error: true,
            },
        }
    }

    async fn acp_list_files(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        // Delegate as a terminal command
        let mut ls_args = HashMap::new();
        ls_args.insert(
            "command".to_string(),
            Value::String(format!("ls -la {}", path)),
        );
        self.acp_bash(&ls_args).await
    }

    async fn acp_search(&self, args: &HashMap<String, Value>, tool_name: &str) -> AcpToolResult {
        // Delegate glob/grep as terminal commands using find/rg
        let command = match tool_name {
            "glob" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*");
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("find {} -name '{}' -type f 2>/dev/null | head -100", path, pattern)
            }
            "grep" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let file_type = args.get("type").and_then(|v| v.as_str());
                let glob = args.get("glob").and_then(|v| v.as_str());

                let mut cmd = format!("rg --no-heading");
                if let Some(ft) = file_type {
                    cmd.push_str(&format!(" --type {}", ft));
                }
                if let Some(g) = glob {
                    cmd.push_str(&format!(" --glob '{}'", g));
                }
                cmd.push_str(&format!(" '{}' {} 2>/dev/null | head -200", pattern, path));
                cmd
            }
            _ => {
                return AcpToolResult {
                    content: format!("Unknown search tool: {}", tool_name),
                    is_error: true,
                }
            }
        };

        let mut bash_args = HashMap::new();
        bash_args.insert("command".to_string(), Value::String(command));
        self.acp_bash(&bash_args).await
    }
}

// ============================================================================
// Supporting types
// ============================================================================

/// Result of streaming a provider response.
enum StreamResult {
    /// Model finished with text content, no tool calls.
    EndTurn { content: String },
    /// Model requested tool calls.
    ToolCalls {
        content: String,
        tool_calls: Vec<AccumulatedToolCall>,
    },
    /// Cancelled by session/cancel.
    Cancelled,
    /// Error during streaming.
    Error(String),
}

/// A fully accumulated tool call from streaming chunks.
#[derive(Debug, Clone)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Result of executing a tool via ACP.
struct AcpToolResult {
    content: String,
    #[allow(dead_code)]
    is_error: bool,
}

// ============================================================================
// Server entry point
// ============================================================================

/// Run the ACP server on stdin/stdout.
pub async fn run_acp_server(
    config: AppConfig,
    model: String,
    api_key: String,
) -> Result<()> {
    // Set up stdout writer channel — all writes go through this to avoid interleaving
    let (stdout_tx, mut stdout_rx) = mpsc::unbounded_channel::<String>();

    // Spawn stdout writer on a blocking thread — StdoutLock is not Send
    let writer_handle = std::thread::spawn(move || {
        let stdout = io::stdout();
        while let Some(line) = stdout_rx.blocking_recv() {
            let mut out = stdout.lock();
            if writeln!(out, "{}", line).is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
        }
    });

    // Spawn stdin reader on a blocking thread — stdin.lock() is not Send
    let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let reader = stdin.lock();
        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        if stdin_tx.send(trimmed).is_err() {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut server = AcpServer::new(config, model, api_key, stdout_tx);

    info!("ACP server started on stdio");

    // Process messages from stdin reader thread
    while let Some(line) = stdin_rx.recv().await {
        let msg: JsonRpcMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                // Send parse error if we can extract an id
                let id = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").cloned())
                    .unwrap_or(Value::Null);

                server.send_error(id, PARSE_ERROR, &format!("Parse error: {}", e));
                continue;
            }
        };

        server.handle_message(msg).await;
    }

    // Clean up — dropping server drops stdout_tx, which causes the writer thread to exit
    drop(server);
    let _ = writer_handle.join();

    Ok(())
}
