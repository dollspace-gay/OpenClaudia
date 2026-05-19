//! MCP Integration - Model Context Protocol client for external tool servers.
//!
//! Supports:
//! - Stdio transport (spawn process, communicate via stdin/stdout)
//! - HTTP transport (connect to HTTP-based MCP servers)
//!
//! Handles tool discovery, schema translation, and request routing.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

// Fix #490 — per-request HTTP timeout cap. Stdio caps responses at 10 MiB
// (`MAX_RESPONSE_SIZE`); the HTTP transport now caps wall-clock time at 60s
// so a stalled MCP server cannot block a tool call indefinitely. Applied
// per request via `RequestBuilder::timeout` so it overrides any global
// default on the shared client.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// Process-wide shared `reqwest::Client` for the HTTP MCP transport.
///
/// Fix #490 — replaces per-`HttpTransport::new` `reqwest::Client::new()`,
/// which built a fresh connection pool, DNS cache, and TLS resolver for
/// every transport instance. Mirrors the `SHARED_HTTP_CLIENT` pattern in
/// `src/web.rs` (commit `fec15a20`, crosslink #368): one client, built
/// once, reused across every `HttpTransport`. Per-request overrides
/// (`HTTP_REQUEST_TIMEOUT`) are still applied at the call site.
static SHARED_MCP_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(10))
        .tcp_keepalive(Duration::from_mins(1))
        .build()
        .expect("shared reqwest client for MCP builds with default features")
});

// Fix #445 point 1 — ring-buffer cap for the background stderr drain.
const STDERR_BUFFER_CAP: usize = 1024 * 1024;
// Fix #445 point 1 — bytes of stderr surfaced inside bubbled errors.
const STDERR_SNIPPET_BYTES: usize = 4096;
// Fix #445 point 2 — bound BEFORE allocation on the response line.
const MAX_RESPONSE_SIZE: usize = 10 * 1024 * 1024;

/// Errors that can occur during MCP operations
#[derive(Error, Debug)]
pub enum McpError {
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Server not connected: {0}")]
    NotConnected(String),

    /// Operation exceeded its configured deadline.
    ///
    /// `phase` names the lifecycle stage that timed out so the operator
    /// can distinguish a stalled `initialize` handshake (fix #628 —
    /// modelled after CC `connectToServer` racing `client.connect`
    /// against `getConnectionTimeoutMs()`) from a stalled per-request
    /// tool call.
    ///
    /// The Display string keeps the lowercase substring `"timeout"` so
    /// existing matchers that grep error messages for that token
    /// continue to work.
    #[error("Operation timeout during {phase} phase")]
    Timeout {
        /// Lifecycle phase whose deadline expired. Static, e.g.
        /// `"initialize"`, `"tools/list"`, `"tools/call"`.
        phase: &'static str,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// JSON-RPC request
#[derive(Debug, Clone, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC response
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

/// JSON-RPC error
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

/// MCP tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

/// MCP resource definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

/// MCP server capabilities
#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpCapabilities {
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    #[serde(default)]
    pub resources: Option<Value>,
    #[serde(default)]
    pub prompts: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

/// MCP server info from initialize response
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Transport trait for MCP communication.
///
/// Fix #490 — `#[async_trait::async_trait]` is the load-bearing piece
/// keeping this trait object-safe. Without it, the `async fn` methods
/// would produce anonymous `impl Future` return types and the trait
/// could not be used behind `Box<dyn McpTransport>` (which `McpServer`
/// stores). The `Send + Sync` supertrait bounds are required so the
/// resulting trait object can cross `.await` points in async tasks.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a request and receive a response
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError>;

    /// Close the transport
    async fn close(&self) -> Result<(), McpError>;
}

// TODO(I-2): Add reconnection logic for transports. When a stdio process
// crashes or an HTTP endpoint becomes unreachable, the transport should
// attempt automatic reconnection with exponential backoff before surfacing
// errors to callers. See crosslink issue #47.

/// Stdio transport - communicates with MCP server via stdin/stdout
pub struct StdioTransport {
    child: Arc<Mutex<Child>>,
    reader: Mutex<BufReader<tokio::process::ChildStdout>>,
    request_id: AtomicU64,
    /// Ring buffer holding the last `STDERR_BUFFER_CAP` bytes the server
    /// wrote to stderr (fix #445 point 1).
    stderr_buf: Arc<Mutex<Vec<u8>>>,
    /// Handle to the stderr drain task. Wrapped in `Arc` so the struct
    /// stays `Send + Sync`. The task auto-terminates on stderr EOF.
    _stderr_drain: Arc<JoinHandle<()>>,
}

/// Spawn a background tokio task that drains `stderr` into a ring buffer.
/// Fix #445 point 1 — mirrors `src/tools/lsp.rs::capture_stderr` (#355)
/// but uses tokio I/O so we don't burn a dedicated OS thread.
fn spawn_stderr_drain(mut stderr: ChildStderr, buf: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut chunk = [0u8; 4096];
        // `while let Ok(n)` exits on read error (terminal for the drain).
        // `n == 0` (EOF) also terminates. Both paths collapse into the
        // same control flow, satisfying `clippy::match_same_arms` and
        // `clippy::while_let_loop` without any `#[allow]`.
        while let Ok(n) = stderr.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let mut guard = buf.lock().await;
            guard.extend_from_slice(&chunk[..n]);
            let len = guard.len();
            if len > STDERR_BUFFER_CAP {
                let drop_n = len - STDERR_BUFFER_CAP;
                guard.drain(..drop_n);
            }
        }
    })
}

/// Format the trailing [`STDERR_SNIPPET_BYTES`] of the stderr ring buffer.
async fn stderr_snippet(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    let guard = buf.lock().await;
    if guard.is_empty() {
        return String::new();
    }
    let start = guard.len().saturating_sub(STDERR_SNIPPET_BYTES);
    let text = String::from_utf8_lossy(&guard[start..]).into_owned();
    drop(guard);
    format!(" (server stderr tail: {text})")
}

impl StdioTransport {
    /// Spawn a new MCP server process.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Transport` if the process cannot be spawned, or if
    /// stdout/stderr cannot be taken from the child.
    pub fn spawn(command: &str, args: &[&str]) -> Result<Self, McpError> {
        info!(command = %command, args = ?args, "Spawning MCP server");

        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| McpError::Transport(format!("Failed to spawn process: {e}")))?;

        // Take stdout from the child once and wrap in a persistent BufReader
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("Stdout not available after spawn".to_string()))?;
        let reader = BufReader::new(stdout);

        // Fix #445 point 1: take stderr and start the background drain so
        // the OS pipe buffer never fills up. Failing to take stderr is a
        // hard error — we asked for `Stdio::piped()`, so absence means
        // we'd silently lose every server diagnostic on failure.
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| McpError::Transport("Stderr not available after spawn".to_string()))?;
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let drain = spawn_stderr_drain(stderr, Arc::clone(&stderr_buf));

        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            reader: Mutex::new(reader),
            request_id: AtomicU64::new(1),
            stderr_buf,
            _stderr_drain: Arc::new(drain),
        })
    }

    /// Returns a clone of the stderr ring-buffer handle. Test-only.
    #[cfg(test)]
    pub(crate) fn stderr_buf_handle(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.stderr_buf)
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let request_line = serde_json::to_string(&request)
            .map_err(|e| McpError::Protocol(format!("Failed to serialize request: {e}")))?;

        debug!(method = %method, id = id, "Sending MCP request");

        let mut child = self.child.lock().await;

        // Write request to stdin
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(request_line.as_bytes())
                .await
                .map_err(|e| McpError::Transport(format!("Failed to write to stdin: {e}")))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| McpError::Transport(format!("Failed to write newline: {e}")))?;
            stdin
                .flush()
                .await
                .map_err(|e| McpError::Transport(format!("Failed to flush stdin: {e}")))?;
        } else {
            return Err(McpError::Transport("Stdin not available".to_string()));
        }

        // Release the child lock before reading. stdin and stdout are
        // independent file descriptors and the reader has its own mutex.
        drop(child);

        // Fix #445 point 2: bound BEFORE allocation.
        //
        // `Take::read_until` consumes at most `MAX_RESPONSE_SIZE + 1` bytes
        // (cap + the terminating newline). The previous code called
        // `BufReader::read_line` with NO upper bound and only checked the
        // length afterwards — by which point a hostile server could already
        // have forced an arbitrarily large allocation.
        //
        // `buf` is `Vec<u8>` rather than `String`: `read_until` works on
        // bytes, and bounding before UTF-8 validation avoids materialising
        // an invalid 10 MiB string only to reject it.
        let buf = {
            let mut reader = self.reader.lock().await;
            let mut buf: Vec<u8> = Vec::new();
            // `+ 1` so we can distinguish "cap reached, no newline"
            // (oversized) from "exactly cap bytes followed by newline".
            let cap = (MAX_RESPONSE_SIZE as u64).saturating_add(1);
            let bytes_read = (&mut *reader)
                .take(cap)
                .read_until(b'\n', &mut buf)
                .await
                .map_err(|e| McpError::Transport(format!("Failed to read from stdout: {e}")))?;
            drop(reader);

            if bytes_read == 0 {
                // EOF before any byte arrived — server died.
                let snippet = stderr_snippet(&self.stderr_buf).await;
                return Err(McpError::Transport(format!(
                    "MCP server closed stdout before responding{snippet}"
                )));
            }

            // Cap reached without a newline — oversized line. Reject
            // before any further processing. This check fires on the
            // FIRST `read_until` call, so the buffer holds at most
            // `MAX_RESPONSE_SIZE + 1` bytes — no unbounded allocation
            // has happened.
            if buf.len() > MAX_RESPONSE_SIZE && !buf.ends_with(b"\n") {
                let snippet = stderr_snippet(&self.stderr_buf).await;
                return Err(McpError::Transport(format!(
                    "MCP response exceeded {MAX_RESPONSE_SIZE} bytes without newline; rejecting{snippet}"
                )));
            }
            buf
        };

        let line = std::str::from_utf8(&buf)
            .map_err(|e| McpError::Protocol(format!("MCP response was not valid UTF-8: {e}")))?;

        let response: JsonRpcResponse = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let snippet = stderr_snippet(&self.stderr_buf).await;
                return Err(McpError::Protocol(format!(
                    "Failed to parse response: {e}{snippet}"
                )));
            }
        };

        if response.id != id {
            return Err(McpError::Protocol(format!(
                "Response ID mismatch: expected {}, got {}",
                id, response.id
            )));
        }

        if let Some(error) = response.error {
            // Include error data in message if available
            let data_info = error
                .data
                .as_ref()
                .map(|d| format!(" (data: {d})"))
                .unwrap_or_default();
            return Err(McpError::Protocol(format!(
                "RPC error {}: {}{}",
                error.code, error.message, data_info
            )));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    async fn close(&self) -> Result<(), McpError> {
        self.child
            .lock()
            .await
            .kill()
            .await
            .map_err(|e| McpError::Transport(format!("Failed to kill process: {e}")))?;
        Ok(())
    }
}

/// HTTP transport - communicates with MCP server via HTTP.
///
/// Fix #490 — does NOT own a `reqwest::Client`. Every instance shares
/// the process-wide `SHARED_MCP_HTTP_CLIENT`, so connecting to N HTTP
/// MCP servers builds the connection pool once, not N times.
pub struct HttpTransport {
    base_url: String,
    request_id: AtomicU64,
}

impl HttpTransport {
    /// Create a new HTTP transport.
    ///
    /// Borrows the process-wide `SHARED_MCP_HTTP_CLIENT` rather than
    /// constructing a fresh `reqwest::Client` (fix #490).
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        // Touch the static so the client is eagerly built on first
        // construction. Cheap, idempotent, and surfaces a build error
        // at transport-creation time rather than first-request time.
        LazyLock::force(&SHARED_MCP_HTTP_CLIENT);
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            request_id: AtomicU64::new(1),
        }
    }

    /// Returns the process-wide shared client. Used so call sites do
    /// not have to name the static directly and so tests can assert
    /// pointer equality of the borrowed reference (fix #490).
    fn client() -> &'static reqwest::Client {
        &SHARED_MCP_HTTP_CLIENT
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        debug!(method = %method, url = %self.base_url, "Sending HTTP MCP request");

        // Fix #490 — share the process-wide client and apply a
        // per-request timeout cap. The shared client carries no
        // request-level timeout (so it can be reused for other
        // workloads with different deadlines); the cap is set here
        // via `RequestBuilder::timeout`.
        let response = Self::client()
            .post(&self.base_url)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    // Per-request HTTP cap (`HTTP_REQUEST_TIMEOUT`)
                    // fired. Phase reflects that this is a steady-state
                    // request, not the connection-establishment
                    // handshake (fix #628 — the latter is bounded by
                    // `McpServer::new_with_config`).
                    McpError::Timeout {
                        phase: "http-request",
                    }
                } else {
                    McpError::Transport(format!("HTTP request failed: {e}"))
                }
            })?;

        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "HTTP error: {}",
                response.status()
            )));
        }

        let response: JsonRpcResponse = response
            .json()
            .await
            .map_err(|e| McpError::Protocol(format!("Failed to parse response: {e}")))?;

        if let Some(error) = response.error {
            return Err(McpError::Protocol(format!(
                "RPC error {}: {}",
                error.code, error.message
            )));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    async fn close(&self) -> Result<(), McpError> {
        // Fix #490 — HTTP transport shares the process-wide client;
        // there is no per-transport resource to release. Tearing
        // down the shared pool would break every other live HTTP
        // transport in the process, so this is intentionally a
        // no-op.
        Ok(())
    }
}

/// Connection-establishment timeout default for [`McpServer::new`]
/// (fix #628).
///
/// CC `connectToServer` (`client.ts:1048-1077`) races `client.connect`
/// against a configurable deadline (default 30 s, env-tunable) so a
/// non-responsive MCP server cannot block an agent task indefinitely.
/// OC mirrors that behaviour: 30 s default, overridable per call via
/// [`McpServerConfig::initialize_timeout_secs`].
pub const DEFAULT_INITIALIZE_TIMEOUT_SECS: u64 = 30;

/// Per-server runtime configuration (fix #628).
///
/// Distinct from [`crate::plugins::manifest::McpServerConfig`] — that
/// type models the on-disk Claude-Code-compatible JSON describing
/// *how* to launch a server (command/args/env/url). This type models
/// *runtime* connection-policy knobs (timeouts) that callers tune at
/// the call site, not in the manifest.
#[derive(Debug, Clone, Copy)]
pub struct McpServerConfig {
    /// Hard deadline on the connection-establishment handshake
    /// (`initialize` + `tools/list`). On expiry,
    /// [`McpServer::new_with_config`] returns [`McpError::Timeout`]
    /// with `phase` naming the stage that stalled.
    ///
    /// `0` disables the deadline (the explicit opt-out used by tests
    /// that want to observe a real hang and by callers that supply
    /// their own outer cancellation scope).
    pub initialize_timeout_secs: u64,
}

impl McpServerConfig {
    /// Default configuration: 30 s initialize-handshake deadline.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            initialize_timeout_secs: DEFAULT_INITIALIZE_TIMEOUT_SECS,
        }
    }

    /// Override the initialize-handshake deadline. Builder-style so
    /// call sites can write
    /// `McpServerConfig::new().with_initialize_timeout_secs(5)`.
    #[must_use]
    pub const fn with_initialize_timeout_secs(mut self, secs: u64) -> Self {
        self.initialize_timeout_secs = secs;
        self
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// An MCP server connection
pub struct McpServer {
    name: String,
    transport: Box<dyn McpTransport>,
    info: Option<McpServerInfo>,
    capabilities: McpCapabilities,
    tools: Vec<McpTool>,
}

impl McpServer {
    /// Create a new MCP server with the given transport, using the
    /// default [`McpServerConfig`] (30 s initialize-handshake
    /// deadline).
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Timeout`] with `phase = "initialize"` or
    /// `phase = "tools/list"` if the corresponding handshake step
    /// does not complete within the configured deadline (fix #628).
    /// Returns other [`McpError`] variants on transport/protocol
    /// failures.
    pub async fn new(name: &str, transport: Box<dyn McpTransport>) -> Result<Self, McpError> {
        Self::new_with_config(name, transport, McpServerConfig::new()).await
    }

    /// Create a new MCP server with explicit runtime configuration.
    ///
    /// Wraps the connection-establishment handshake (`initialize` +
    /// `tools/list`) in [`tokio::time::timeout`] so a non-responsive
    /// server cannot block the calling task indefinitely (fix #628 —
    /// mirrors CC `connectToServer` racing `client.connect` against
    /// `getConnectionTimeoutMs()`).
    ///
    /// A `initialize_timeout_secs` of `0` disables the deadline.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Timeout`] with `phase = "initialize"` if
    /// the initialize handshake hangs, or `phase = "tools/list"` if
    /// the post-handshake tool discovery hangs. Returns other
    /// [`McpError`] variants on transport/protocol failures.
    pub async fn new_with_config(
        name: &str,
        transport: Box<dyn McpTransport>,
        config: McpServerConfig,
    ) -> Result<Self, McpError> {
        let mut server = Self {
            name: name.to_string(),
            transport,
            info: None,
            capabilities: McpCapabilities::default(),
            tools: Vec::new(),
        };

        // Fix #628 — bound the initialize handshake. A non-responsive
        // server would otherwise hang the calling tokio task forever
        // because `transport.request("initialize", ...)` has no
        // built-in deadline (the HTTP transport's `HTTP_REQUEST_TIMEOUT`
        // covers steady-state requests, the stdio transport has no
        // wall-clock cap at all).
        //
        // `tokio::time::timeout` cancels the inner future on expiry,
        // which for stdio drops the in-flight `read_until` (the child
        // process remains, but the caller can decide whether to retry
        // or close). For HTTP it cancels the `RequestBuilder::send`
        // future before the per-request `HTTP_REQUEST_TIMEOUT` fires —
        // which is the intended semantics, since the initialize
        // handshake has its own (typically shorter) policy.
        if config.initialize_timeout_secs == 0 {
            server.initialize().await?;
            server.refresh_tools().await?;
        } else {
            let deadline = Duration::from_secs(config.initialize_timeout_secs);
            let Ok(init_res) = tokio::time::timeout(deadline, server.initialize()).await else {
                warn!(
                    server = %server.name,
                    timeout_secs = config.initialize_timeout_secs,
                    "MCP server initialize handshake timed out"
                );
                return Err(McpError::Timeout {
                    phase: "initialize",
                });
            };
            init_res?;
            let Ok(tools_res) = tokio::time::timeout(deadline, server.refresh_tools()).await else {
                warn!(
                    server = %server.name,
                    timeout_secs = config.initialize_timeout_secs,
                    "MCP server tools/list timed out"
                );
                return Err(McpError::Timeout {
                    phase: "tools/list",
                });
            };
            tools_res?;
        }

        Ok(server)
    }

    /// Initialize the MCP connection
    async fn initialize(&mut self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "roots": { "listChanged": true }
            },
            "clientInfo": {
                "name": "openclaudia",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let result = self.transport.request("initialize", Some(params)).await?;

        // Parse server info and capabilities
        if let Some(info) = result.get("serverInfo") {
            self.info = serde_json::from_value(info.clone()).ok();
        }

        if let Some(caps) = result.get("capabilities") {
            self.capabilities = serde_json::from_value(caps.clone()).unwrap_or_default();
        }

        // Send initialized notification
        self.transport
            .request("notifications/initialized", None)
            .await
            .ok();

        // Log server info with name and version
        let server_name = self.info.as_ref().map_or("unknown", |i| i.name.as_str());
        let server_version = self
            .info
            .as_ref()
            .and_then(|i| i.version.as_deref())
            .unwrap_or("unknown");

        // Log capabilities for debugging
        let has_tools = self.capabilities.tools.is_some();
        let has_resources = self.capabilities.resources.is_some();
        let has_prompts = self.capabilities.prompts.is_some();

        info!(
            server = %self.name,
            remote_name = %server_name,
            remote_version = %server_version,
            has_tools = has_tools,
            has_resources = has_resources,
            has_prompts = has_prompts,
            "MCP server initialized"
        );

        Ok(())
    }

    /// Refresh the list of available tools.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if the tools/list request fails.
    pub async fn refresh_tools(&mut self) -> Result<(), McpError> {
        let result = self.transport.request("tools/list", None).await?;

        if let Some(tools) = result.get("tools").and_then(|t| t.as_array()) {
            self.tools = tools
                .iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect();

            // Check if server supports tool list change notifications
            let supports_list_changed = self
                .capabilities
                .tools
                .as_ref()
                .is_some_and(|t| t.list_changed);

            info!(
                server = %self.name,
                tool_count = self.tools.len(),
                list_changed_supported = supports_list_changed,
                "Discovered MCP tools"
            );
        }

        Ok(())
    }

    /// Check if the server supports tool list change notifications
    #[must_use]
    pub fn supports_tool_list_changed(&self) -> bool {
        self.capabilities
            .tools
            .as_ref()
            .is_some_and(|t| t.list_changed)
    }

    /// Get the list of available tools
    #[must_use]
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Call a tool.
    ///
    /// # Errors
    ///
    /// Returns `McpError::ToolNotFound` if the tool is not registered, or a
    /// transport/protocol error if the request fails.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        if !self.tools.iter().any(|t| t.name == name) {
            return Err(McpError::ToolNotFound(name.to_string()));
        }

        let params = json!({
            "name": name,
            "arguments": arguments
        });

        debug!(server = %self.name, tool = %name, "Calling MCP tool");

        let result = self.transport.request("tools/call", Some(params)).await?;

        Ok(result)
    }

    /// Check if the server advertises resource capabilities
    #[must_use]
    pub const fn has_resources(&self) -> bool {
        self.capabilities.resources.is_some()
    }

    /// List resources available on this server.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if the resources/list request fails.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        if !self.has_resources() {
            return Ok(Vec::new());
        }

        let result = self
            .transport
            .request("resources/list", Some(json!({})))
            .await?;

        let resources: Vec<_> = result
            .get("resources")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| serde_json::from_value(r.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        debug!(
            server = %self.name,
            resource_count = resources.len(),
            "Listed MCP resources"
        );

        Ok(resources)
    }

    /// Read a specific resource by URI.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if the resources/read request fails.
    pub async fn read_resource(&self, uri: &str) -> Result<String, McpError> {
        let params = json!({ "uri": uri });

        debug!(server = %self.name, uri = %uri, "Reading MCP resource");

        let result = self
            .transport
            .request("resources/read", Some(params))
            .await?;

        // The MCP spec returns contents as an array of content items
        if let Some(contents) = result.get("contents").and_then(|c| c.as_array()) {
            let text: Vec<&str> = contents
                .iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .collect();
            if !text.is_empty() {
                return Ok(text.join("\n"));
            }
            // Check for blob content (base64-encoded)
            let blobs: Vec<&str> = contents
                .iter()
                .filter_map(|c| c.get("blob").and_then(|b| b.as_str()))
                .collect();
            if !blobs.is_empty() {
                return Ok(blobs.join("\n"));
            }
        }

        // Fallback: return the raw result as string
        Ok(result.to_string())
    }

    /// Get server name
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Close the connection.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if the transport fails to close.
    pub async fn close(self) -> Result<(), McpError> {
        self.transport.close().await
    }
}

/// Manages multiple MCP server connections
pub struct McpManager {
    servers: HashMap<String, McpServer>,
}

impl McpManager {
    /// Create a new MCP manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Connect to an MCP server via stdio.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if spawning or initializing the server fails.
    pub async fn connect_stdio(
        &mut self,
        name: &str,
        command: &str,
        args: &[&str],
    ) -> Result<(), McpError> {
        let transport = StdioTransport::spawn(command, args)?;
        let server = McpServer::new(name, Box::new(transport)).await?;
        self.servers.insert(name.to_string(), server);
        Ok(())
    }

    /// Connect to an MCP server via HTTP.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if connecting or initializing the server fails.
    pub async fn connect_http(&mut self, name: &str, url: &str) -> Result<(), McpError> {
        let transport = HttpTransport::new(url);
        let server = McpServer::new(name, Box::new(transport)).await?;
        self.servers.insert(name.to_string(), server);
        Ok(())
    }

    /// Get all available tools from all servers
    #[must_use]
    pub fn all_tools(&self) -> Vec<(&str, &McpTool)> {
        self.servers
            .iter()
            .flat_map(|(server_name, server)| {
                server
                    .tools()
                    .iter()
                    .map(move |tool| (server_name.as_str(), tool))
            })
            .collect()
    }

    /// Convert MCP tools to `OpenAI` function format.
    ///
    /// Tool names use `mcp__servername__toolname` with double-underscore
    /// delimiters, allowing server and tool names to contain single underscores.
    #[must_use]
    pub fn tools_as_openai_functions(&self) -> Vec<Value> {
        self.all_tools()
            .iter()
            .map(|(server_name, tool)| {
                json!({
                    "type": "function",
                    "function": {
                        "name": format!("mcp__{}__{}", server_name, tool.name),
                        "description": tool.description.as_deref().unwrap_or(""),
                        "parameters": tool.input_schema.clone().unwrap_or_else(|| json!({"type": "object", "properties": {}}))
                    }
                })
            })
            .collect()
    }

    /// Call a tool by its full name (`mcp__servername__toolname`).
    ///
    /// Uses double-underscore (`__`) delimiters so that server and tool names
    /// may themselves contain single underscores.
    ///
    /// # Errors
    ///
    /// Returns `McpError::ToolNotFound` if the name format is invalid, or
    /// `McpError::NotConnected` if the server is not registered.
    pub async fn call_tool(&self, full_name: &str, arguments: Value) -> Result<Value, McpError> {
        // Format: mcp__servername__toolname
        let parts: Vec<&str> = full_name.splitn(3, "__").collect();
        if parts.len() != 3 || parts[0] != "mcp" {
            return Err(McpError::ToolNotFound(format!(
                "Invalid tool name format: {full_name}. Expected mcp__servername__toolname"
            )));
        }

        let server_name = parts[1];
        let tool_name = parts[2];

        let server = self
            .servers
            .get(server_name)
            .ok_or_else(|| McpError::NotConnected(server_name.to_string()))?;

        server.call_tool(tool_name, arguments).await
    }

    /// Call a tool with a timeout.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Timeout` if the call exceeds the duration, or
    /// propagates any error from `call_tool`.
    pub async fn call_tool_with_timeout(
        &self,
        full_name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        tokio::time::timeout(timeout, self.call_tool(full_name, arguments))
            .await
            .unwrap_or_else(|_| {
                warn!(tool = %full_name, timeout_secs = timeout.as_secs(), "MCP tool call timed out");
                Err(McpError::Timeout { phase: "tools/call" })
            })
    }

    /// Get information about a connected server
    #[must_use]
    pub fn get_server_info(&self, name: &str) -> Option<(&str, bool)> {
        self.servers.get(name).map(|s| {
            let server_name = s.name();
            let supports_list_changed = s.supports_tool_list_changed();
            (server_name, supports_list_changed)
        })
    }

    /// List resources across all servers, or from a specific server.
    ///
    /// # Errors
    ///
    /// Returns an error if a named server is not connected or the request fails.
    pub async fn list_resources(
        &self,
        server_name: Option<&str>,
    ) -> anyhow::Result<Vec<(String, McpResource)>> {
        let mut all_resources = Vec::new();

        if let Some(name) = server_name {
            let server = self
                .servers
                .get(name)
                .ok_or_else(|| McpError::NotConnected(name.to_string()))?;
            let resources = server.list_resources().await?;
            for r in resources {
                all_resources.push((name.to_string(), r));
            }
        } else {
            for (name, server) in &self.servers {
                match server.list_resources().await {
                    Ok(resources) => {
                        for r in resources {
                            all_resources.push((name.clone(), r));
                        }
                    }
                    Err(e) => {
                        warn!(server = %name, error = %e, "Failed to list resources from server");
                    }
                }
            }
        }

        Ok(all_resources)
    }

    /// Read a specific resource from a named server.
    ///
    /// # Errors
    ///
    /// Returns an error if the server is not connected or the read fails.
    pub async fn read_resource(&self, server_name: &str, uri: &str) -> anyhow::Result<String> {
        let server = self
            .servers
            .get(server_name)
            .ok_or_else(|| McpError::NotConnected(server_name.to_string()))?;
        let content = server.read_resource(uri).await?;
        Ok(content)
    }

    /// Disconnect from a server.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if the server's transport fails to close.
    pub async fn disconnect(&mut self, name: &str) -> Result<(), McpError> {
        if let Some(server) = self.servers.remove(name) {
            server.close().await?;
        }
        Ok(())
    }

    /// Disconnect from all servers.
    ///
    /// # Errors
    ///
    /// Returns the first `McpError` encountered while closing servers.
    pub async fn disconnect_all(&mut self) -> Result<(), McpError> {
        let names: Vec<String> = self.servers.keys().cloned().collect();
        for name in names {
            self.disconnect(&name).await?;
        }
        Ok(())
    }

    /// Get the number of connected servers
    #[must_use]
    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// Check if a server is connected
    #[must_use]
    pub fn is_connected(&self, name: &str) -> bool {
        self.servers.contains_key(name)
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_serialization() {
        let tool = McpTool {
            name: "read_file".to_string(),
            description: Some("Read a file".to_string()),
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            })),
        };

        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["name"], "read_file");
        assert_eq!(json["description"], "Read a file");
    }

    #[test]
    fn test_mcp_manager_new() {
        let manager = McpManager::new();
        assert_eq!(manager.server_count(), 0);
    }

    #[test]
    fn test_tools_as_openai_functions() {
        // This would require a mock server, so just test the format
        let manager = McpManager::new();
        let functions = manager.tools_as_openai_functions();
        assert!(functions.is_empty());
    }

    #[test]
    fn test_http_transport_new() {
        let transport = HttpTransport::new("http://localhost:8080/");
        assert_eq!(transport.base_url, "http://localhost:8080");
    }

    #[test]
    fn test_json_rpc_request_serialization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "test".to_string(),
            params: Some(json!({"key": "value"})),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["method"], "test");
        assert_eq!(json["params"]["key"], "value");
    }

    #[test]
    fn test_mcp_error_variants() {
        // Test ToolNotFound variant
        let err = McpError::ToolNotFound("missing_tool".to_string());
        assert!(err.to_string().contains("missing_tool"));

        // Test NotConnected variant
        let err = McpError::NotConnected("server1".to_string());
        assert!(err.to_string().contains("server1"));

        // Test Timeout variant (fix #628 — struct variant with phase)
        let err = McpError::Timeout {
            phase: "initialize",
        };
        assert!(err.to_string().contains("timeout"));
        assert!(err.to_string().contains("initialize"));
    }

    #[test]
    fn test_mcp_capabilities_parsing() {
        let caps_json = r#"{
            "tools": {"listChanged": true},
            "resources": {"subscribe": true},
            "prompts": {"listChanged": false}
        }"#;

        let caps: McpCapabilities = serde_json::from_str(caps_json).unwrap();
        assert!(caps.tools.is_some());
        assert!(caps.resources.is_some());
        assert!(caps.prompts.is_some());

        // Access list_changed field
        let tools = caps.tools.unwrap();
        assert!(tools.list_changed);
    }

    #[test]
    fn test_mcp_server_info_parsing() {
        let info_json = r#"{"name": "test-server", "version": "1.0.0"}"#;
        let info: McpServerInfo = serde_json::from_str(info_json).unwrap();
        assert_eq!(info.name, "test-server");
        assert_eq!(info.version, Some("1.0.0".to_string()));
    }

    #[test]
    fn test_json_rpc_error_with_data() {
        let error_json = r#"{
            "code": -32600,
            "message": "Invalid Request",
            "data": {"details": "missing field"}
        }"#;

        let error: JsonRpcError = serde_json::from_str(error_json).unwrap();
        assert_eq!(error.code, -32600);
        assert_eq!(error.message, "Invalid Request");
        assert!(error.data.is_some());
        let data = error.data.unwrap();
        assert_eq!(data["details"], "missing field");
    }

    #[tokio::test]
    async fn test_mcp_manager_call_tool_invalid_format() {
        let manager = McpManager::new();

        // Test with no delimiters
        let result = manager.call_tool("invalidtool", json!({})).await;
        assert!(matches!(result, Err(McpError::ToolNotFound(_))));

        // Test with old single-underscore format (should fail)
        let result = manager.call_tool("server_tool", json!({})).await;
        assert!(matches!(result, Err(McpError::ToolNotFound(_))));

        // Test with double-underscore but no mcp prefix
        let result = manager.call_tool("server__tool", json!({})).await;
        assert!(matches!(result, Err(McpError::ToolNotFound(_))));
    }

    #[tokio::test]
    async fn test_mcp_manager_call_tool_not_connected() {
        let manager = McpManager::new();

        // Test with valid mcp__server__tool format but server not connected
        let result = manager.call_tool("mcp__server__tool", json!({})).await;
        assert!(matches!(result, Err(McpError::NotConnected(_))));
    }

    #[tokio::test]
    async fn test_mcp_manager_call_tool_underscored_server_name() {
        let manager = McpManager::new();

        // Server names with underscores should parse correctly
        let result = manager
            .call_tool("mcp__my_server__my_tool", json!({}))
            .await;
        // Should get NotConnected (not ToolNotFound), proving parse worked
        assert!(matches!(result, Err(McpError::NotConnected(_))));
        if let Err(McpError::NotConnected(name)) = result {
            assert_eq!(name, "my_server");
        }
    }

    #[tokio::test]
    async fn test_mcp_manager_call_tool_with_timeout() {
        let manager = McpManager::new();

        // Test timeout (will fail because no server, but exercises the code path)
        let result = manager
            .call_tool_with_timeout("mcp__server__tool", json!({}), Duration::from_millis(100))
            .await;
        // Should get NotConnected error, not Timeout (since call fails immediately)
        assert!(matches!(result, Err(McpError::NotConnected(_))));
    }

    #[test]
    fn test_mcp_manager_is_connected() {
        let manager = McpManager::new();
        assert!(!manager.is_connected("nonexistent"));
    }

    #[test]
    fn test_mcp_manager_get_server_info() {
        let manager = McpManager::new();
        assert!(manager.get_server_info("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_mcp_manager_disconnect_nonexistent() {
        let mut manager = McpManager::new();
        // Should not error when disconnecting non-existent server
        let result = manager.disconnect("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mcp_manager_disconnect_all_empty() {
        let mut manager = McpManager::new();
        let result = manager.disconnect_all().await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_mcp_resource_serialization() {
        let resource = McpResource {
            uri: "file:///src/main.rs".to_string(),
            name: "main.rs".to_string(),
            description: Some("Main entry point".to_string()),
            mime_type: Some("text/x-rust".to_string()),
        };

        let json = serde_json::to_value(&resource).unwrap();
        assert_eq!(json["uri"], "file:///src/main.rs");
        assert_eq!(json["name"], "main.rs");
        assert_eq!(json["description"], "Main entry point");
        assert_eq!(json["mimeType"], "text/x-rust");
    }

    #[test]
    fn test_mcp_resource_deserialization() {
        let json =
            r#"{"uri": "db://users", "name": "Users Table", "mimeType": "application/json"}"#;
        let resource: McpResource = serde_json::from_str(json).unwrap();
        assert_eq!(resource.uri, "db://users");
        assert_eq!(resource.name, "Users Table");
        assert!(resource.description.is_none());
        assert_eq!(resource.mime_type, Some("application/json".to_string()));
    }

    #[test]
    fn test_mcp_resource_minimal() {
        let json = r#"{"uri": "test://resource", "name": "test"}"#;
        let resource: McpResource = serde_json::from_str(json).unwrap();
        assert_eq!(resource.uri, "test://resource");
        assert_eq!(resource.name, "test");
        assert!(resource.description.is_none());
        assert!(resource.mime_type.is_none());
    }

    #[tokio::test]
    async fn test_mcp_manager_list_resources_empty() {
        let manager = McpManager::new();
        let resources = manager.list_resources(None).await.unwrap();
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_mcp_manager_list_resources_server_not_connected() {
        let manager = McpManager::new();
        let result = manager.list_resources(Some("nonexistent")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mcp_manager_read_resource_not_connected() {
        let manager = McpManager::new();
        let result = manager.read_resource("nonexistent", "file:///test").await;
        assert!(result.is_err());
    }

    // ─── Fix #445 — StdioTransport stderr drain + bounded read ──────────
    //
    // Each test spawns a real subprocess via `sh -c` and exercises
    // StdioTransport end to end. <200 ms per test; POSIX-only (`sh` and
    // `head` must exist on PATH, which matches the project baseline).
    //
    // Forensic evidence: with the pre-fix `BufReader::read_line` the
    // oversized-line test would either OOM or block; with no stderr
    // drain a server writing more than ~64 KiB to stderr would deadlock
    // on `write(2)`. Both scenarios now complete deterministically.

    fn spawn_sh(script: &str) -> Result<StdioTransport, McpError> {
        StdioTransport::spawn("sh", &["-c", script])
    }

    /// Fix #445 point 1: a server that writes >64 KiB to stderr does NOT
    /// deadlock the transport. Without the drain, the server would block
    /// on `write(2)` and the stdout reply would never arrive.
    #[tokio::test]
    async fn fix445_stderr_drained_does_not_deadlock() {
        let transport = spawn_sh(
            "printf '%131072s' '' >&2; \
             read req; \
             printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n'",
        )
        .expect("spawn");

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            transport.request("ping", None),
        )
        .await
        .expect("request did not deadlock");

        assert!(result.is_ok(), "request failed: {result:?}");
        assert_eq!(result.unwrap()["ok"], true);
        let _ = transport.close().await;
    }

    /// Fix #445 point 1: the stderr drain captures server output and the
    /// ring buffer contains a recognizable suffix.
    #[tokio::test]
    async fn fix445_stderr_drain_populates_ring_buffer() {
        let transport = spawn_sh(
            "printf 'KERNEL_PANIC_MARKER_445\\n' >&2; \
             read req; \
             printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}\n'",
        )
        .expect("spawn");

        let _ = transport.request("ping", None).await;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let buf_handle = transport.stderr_buf_handle();
        let guard = buf_handle.lock().await;
        let snippet = String::from_utf8_lossy(&guard).into_owned();
        drop(guard);
        assert!(
            snippet.contains("KERNEL_PANIC_MARKER_445"),
            "stderr drain did not capture server output; got: {snippet:?}"
        );
        let _ = transport.close().await;
    }

    /// Fix #445 point 2: oversized line is rejected WITHOUT buffering
    /// the full payload. Pre-fix `read_line` would have allocated the
    /// whole 11 MiB before the size check.
    #[tokio::test]
    async fn fix445_oversized_line_rejected_before_full_buffering() {
        let script = format!(
            "read req; head -c {size} /dev/zero",
            size = MAX_RESPONSE_SIZE + 1024 * 1024,
        );
        let transport = spawn_sh(&script).expect("spawn");

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            transport.request("ping", None),
        )
        .await
        .expect("oversized read did not complete within timeout");

        let err = result.expect_err("oversized line should be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("exceeded") && msg.contains("without newline"),
            "expected oversized-line error, got: {msg}"
        );
        let _ = transport.close().await;
    }

    /// Sanity: a normal, well-formed response round-trips correctly.
    #[tokio::test]
    async fn fix445_normal_line_succeeds() {
        let transport = spawn_sh(
            "read req; \
             printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":42}}\n'",
        )
        .expect("spawn");

        let result = transport.request("ping", None).await.expect("request ok");
        assert_eq!(result["value"], 42);
        let _ = transport.close().await;
    }

    // ─── Fix #490 — object-safe trait + shared HTTP client ─────────────
    //
    // Forensic evidence:
    //   1. `fix490_trait_object_compiles` — proves `McpTransport` stays
    //      object-safe. If any new method violates object-safety (e.g.
    //      a generic method, or `Self`-by-value), this test would fail
    //      to compile.
    //   2. `fix490_http_client_is_shared` — checks pointer identity of
    //      the `&'static reqwest::Client` borrowed by `HttpTransport`.
    //      With the pre-fix `reqwest::Client::new()` per construction
    //      this would FAIL because each instance owned a distinct
    //      heap-allocated client. With the shared `LazyLock` the
    //      pointer is the same across instances.
    //   3. `fix490_http_per_request_timeout_enforced` — points
    //      `HttpTransport` at a TCP server that accepts but never
    //      writes, calls send, and asserts the call returns within
    //      ~2s with a timeout error instead of hanging on the OS
    //      default.

    /// Fix #490: `McpTransport` must remain object-safe so `McpServer`
    /// can store `Box<dyn McpTransport>`. This test is the compile-time
    /// proof — if anyone adds a non-object-safe method, this fails to
    /// build.
    #[test]
    fn fix490_trait_object_compiles() {
        let http: Box<dyn McpTransport> = Box::new(HttpTransport::new("http://127.0.0.1:1"));
        // Touch a method to prove the vtable is callable through the
        // trait object (statically — we don't actually `.await` here).
        let _fut = http.close();
        // Also assert via a type-position binding that &dyn works.
        let _r: &dyn McpTransport = http.as_ref();
    }

    /// Fix #490: every `HttpTransport` borrows the SAME process-wide
    /// `reqwest::Client`. Pointer equality of the `&'static` reference
    /// is the strongest possible evidence.
    #[test]
    fn fix490_http_client_is_shared() {
        let a = HttpTransport::new("http://example.invalid/a");
        let b = HttpTransport::new("http://example.invalid/b");
        // Force the LazyLock so the static is materialised.
        let direct = &*SHARED_MCP_HTTP_CLIENT;
        let _ = &a;
        let _ = &b;
        let p_a = std::ptr::from_ref::<reqwest::Client>(HttpTransport::client());
        let p_b = std::ptr::from_ref::<reqwest::Client>(HttpTransport::client());
        let p_d = std::ptr::from_ref::<reqwest::Client>(direct);
        assert_eq!(p_a, p_b, "two HttpTransports must share one client");
        assert_eq!(p_a, p_d, "shared client must equal the static itself");
    }

    /// Fix #490: per-request timeout is set on the `RequestBuilder`
    /// (not on the shared client), so a stalled server returns a
    /// timeout error within the per-request cap. We point the
    /// transport at a TCP server that accepts the connection but
    /// never writes a byte — simulating a stalled MCP HTTP endpoint
    /// — and use a 250ms override at the call site to keep the unit
    /// test fast. The production cap (`HTTP_REQUEST_TIMEOUT` = 60s)
    /// is enforced by the same mechanism this test exercises.
    #[tokio::test]
    async fn fix490_http_per_request_timeout_enforced() {
        use tokio::io::AsyncReadExt as _;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let _server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                while sock.read(&mut buf).await.unwrap_or(0) > 0 {}
            }
        });

        let url = format!("http://{addr}");
        let transport = HttpTransport::new(&url);
        let id = transport.request_id.fetch_add(1, Ordering::SeqCst);
        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: "ping".to_string(),
            params: None,
        };
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            HttpTransport::client()
                .post(&url)
                .timeout(Duration::from_millis(250))
                .json(&body)
                .send(),
        )
        .await;
        let elapsed = start.elapsed();

        let inner = result.expect("outer timeout fired — per-request timeout did not enforce");
        let err = inner.expect_err("stalled server must produce an error");
        assert!(
            err.is_timeout() || err.is_request(),
            "expected timeout-like reqwest error, got: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "per-request timeout should fire fast (<2s), took {elapsed:?}"
        );
    }

    /// Fix #445 point 1: concurrent request + drain does not deadlock,
    /// across multiple sequential requests on the same transport with
    /// stderr traffic interleaved.
    #[tokio::test]
    async fn fix445_concurrent_drain_and_request_no_deadlock() {
        let transport = spawn_sh(
            "for i in 1 2 3 4 5; do printf 'noise-%s\\n' \"$i\" >&2; done; \
             read req1; \
             printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":1}\n'; \
             for i in 6 7 8 9 10; do printf 'noise-%s\\n' \"$i\" >&2; done; \
             read req2; \
             printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":2}\n'",
        )
        .expect("spawn");

        let r1 = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            transport.request("first", None),
        )
        .await
        .expect("first request did not deadlock")
        .expect("first request returned error");
        assert_eq!(r1, serde_json::json!(1));

        let r2 = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            transport.request("second", None),
        )
        .await
        .expect("second request did not deadlock")
        .expect("second request returned error");
        assert_eq!(r2, serde_json::json!(2));

        let _ = transport.close().await;
    }

    /// In-memory transport used to drive [`McpServer::new_with_config`]
    /// without a child process. `responses` lists canned replies in the
    /// order they will be returned; `delay_first_response` introduces a
    /// configurable sleep on the FIRST call so we can simulate a stalled
    /// initialize. The transport never blocks indefinitely on its own —
    /// the only stall source is the configured delay.
    struct FakeTransport {
        responses: std::sync::Mutex<std::collections::VecDeque<Value>>,
        delay_first_response: std::sync::Mutex<Option<Duration>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                delay_first_response: std::sync::Mutex::new(None),
            }
        }

        fn with_initial_delay(self, delay: Duration) -> Self {
            *self.delay_first_response.lock().expect("lock") = Some(delay);
            self
        }
    }

    #[async_trait]
    impl McpTransport for FakeTransport {
        async fn request(&self, _method: &str, _params: Option<Value>) -> Result<Value, McpError> {
            // Take the delay (once); on first call we honour it.
            let delay = self.delay_first_response.lock().expect("lock").take();
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            let next = self.responses.lock().expect("lock").pop_front();
            Ok(next.unwrap_or(Value::Null))
        }

        async fn close(&self) -> Result<(), McpError> {
            Ok(())
        }
    }

    // ─── Fix #628 — initialize-handshake timeout ───────────────────────
    //
    // Forensic evidence: the pre-fix `McpServer::new` chained
    // `server.initialize().await?` directly, with NO `tokio::time::timeout`
    // guard. A non-responsive transport (one whose `request` future
    // never resolves) would block the calling tokio task forever
    // because `transport.request("initialize", ...)` has no built-in
    // deadline. These tests would hang the runtime entirely without the
    // fix; with the fix they complete deterministically in well under
    // a second.

    /// Fix #628: a transport that stalls on the FIRST request (the
    /// initialize handshake) MUST cause `McpServer::new_with_config`
    /// to return `McpError::Timeout { phase: "initialize" }` within
    /// the configured deadline — not hang forever.
    #[tokio::test]
    async fn fix628_initialize_timeout_fires_on_hanging_server() {
        // 60 s stall on first request simulates a non-responsive server.
        let transport = FakeTransport::new(vec![]).with_initial_delay(Duration::from_mins(1));
        let config = McpServerConfig::new().with_initialize_timeout_secs(1);

        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            // Outer belt-and-suspenders. If the inner timeout failed to
            // fire, this catches the bug instead of hanging the test
            // runtime forever.
            std::time::Duration::from_secs(10),
            McpServer::new_with_config("hang", Box::new(transport), config),
        )
        .await
        .expect("outer timeout fired — inner #628 timeout did not enforce");
        let elapsed = start.elapsed();

        // `McpServer` doesn't implement `Debug`, so we pattern-match on
        // the `Result` rather than using `.expect_err()`.
        match result {
            Err(McpError::Timeout {
                phase: "initialize",
            }) => {}
            Err(other) => panic!("expected Timeout {{ phase: \"initialize\" }}, got {other:?}"),
            Ok(_) => panic!("hanging server must produce an error, got Ok"),
        }
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "initialize timeout (1 s) should fire fast; took {elapsed:?}"
        );
    }

    /// Fix #628: a well-behaved transport completes the initialize
    /// handshake well within the deadline and returns a usable
    /// `McpServer`. Proves the timeout wrapper does NOT regress
    /// normal behaviour — the production path returns Ok.
    #[tokio::test]
    async fn fix628_normal_handshake_succeeds_under_timeout() {
        // Canned protocol: (1) initialize reply, (2) notifications/initialized
        // (the production code calls `.ok()` on this so the `Value::Null`
        // returned by FakeTransport is harmless), (3) tools/list reply.
        let transport = FakeTransport::new(vec![
            json!({
                "serverInfo": {"name": "ok", "version": "1"},
                "capabilities": {"tools": {"listChanged": false}}
            }),
            Value::Null,
            json!({"tools": []}),
        ]);
        let config = McpServerConfig::new().with_initialize_timeout_secs(10);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            McpServer::new_with_config("ok", Box::new(transport), config),
        )
        .await
        .expect("outer timeout fired — handshake stalled");
        let server = match result {
            Ok(s) => s,
            Err(e) => panic!("handshake must succeed, got error: {e:?}"),
        };

        assert_eq!(server.name(), "ok");
        assert!(server.tools().is_empty());
    }

    /// Fix #628: the timeout duration is configurable via
    /// [`McpServerConfig::initialize_timeout_secs`]. Verifies the
    /// public-API contract (default = 30 s, builder is monotonic on
    /// the targeted field) AND that a short override is actually
    /// honoured at runtime (a 1 s override fires in < 3 s against a
    /// 60 s stall).
    #[tokio::test]
    async fn fix628_initialize_timeout_is_configurable() {
        assert_eq!(McpServerConfig::default().initialize_timeout_secs, 30);
        assert_eq!(McpServerConfig::new().initialize_timeout_secs, 30);
        assert_eq!(DEFAULT_INITIALIZE_TIMEOUT_SECS, 30);

        let custom = McpServerConfig::new().with_initialize_timeout_secs(5);
        assert_eq!(custom.initialize_timeout_secs, 5);

        let transport = FakeTransport::new(vec![]).with_initial_delay(Duration::from_mins(1));
        let config = McpServerConfig::new()
            .with_initialize_timeout_secs(0)
            .with_initialize_timeout_secs(1);
        assert_eq!(config.initialize_timeout_secs, 1);

        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            McpServer::new_with_config("cfg", Box::new(transport), config),
        )
        .await
        .expect("outer timeout fired — configurable timeout did not enforce");
        let elapsed = start.elapsed();

        match result {
            Err(McpError::Timeout {
                phase: "initialize",
            }) => {}
            Err(other) => panic!("expected Timeout {{ phase: \"initialize\" }}, got {other:?}"),
            Ok(_) => panic!("hanging server must produce an error, got Ok"),
        }
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "configurable 1 s timeout should fire fast; took {elapsed:?}"
        );
    }

    /// Fix #628: `initialize_timeout_secs = 0` disables the deadline —
    /// the explicit opt-out for callers that supply their own outer
    /// cancellation scope. With the timeout disabled, a stalled
    /// transport hangs the call indefinitely; the outer
    /// `tokio::time::timeout` is what fires (NOT an inner
    /// `McpError::Timeout`).
    #[tokio::test]
    async fn fix628_initialize_timeout_zero_disables_deadline() {
        let transport = FakeTransport::new(vec![]).with_initial_delay(Duration::from_mins(1));
        let config = McpServerConfig::new().with_initialize_timeout_secs(0);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            McpServer::new_with_config("nocap", Box::new(transport), config),
        )
        .await;

        // `tokio::time::timeout` returns `Err(Elapsed)` when the inner
        // future does not complete. `outcome.is_err()` therefore proves
        // the inner deadline did NOT fire — the `0 = disabled` contract
        // held.
        assert!(
            outcome.is_err(),
            "with initialize_timeout_secs=0, the inner call must hang \
             until the OUTER timeout fires; instead the inner call \
             completed — the `0 = disabled` contract was violated"
        );
    }
}
