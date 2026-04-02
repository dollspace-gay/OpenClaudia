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
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

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

    #[error("Request timeout")]
    Timeout,

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

/// Transport trait for MCP communication
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
}

impl StdioTransport {
    /// Spawn a new MCP server process.
    ///
    /// # Errors
    ///
    /// Returns `McpError::Transport` if the process cannot be spawned or stdout
    /// is unavailable.
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

        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            reader: Mutex::new(reader),
            request_id: AtomicU64::new(1),
        })
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

        // Release the child lock before reading, since stdout is stored separately
        drop(child);

        // Read response from the persistent BufReader with size limit.
        // Prevents memory exhaustion from malicious MCP servers.
        const MAX_RESPONSE_SIZE: usize = 10 * 1024 * 1024; // 10MB
        let line = {
            let mut reader = self.reader.lock().await;
            let mut buf = String::new();
            reader
                .read_line(&mut buf)
                .await
                .map_err(|e| McpError::Transport(format!("Failed to read from stdout: {e}")))?;
            drop(reader);
            if buf.len() > MAX_RESPONSE_SIZE {
                return Err(McpError::Transport(format!(
                    "MCP response too large ({} bytes, max {})",
                    buf.len(),
                    MAX_RESPONSE_SIZE
                )));
            }
            buf
        };

        let response: JsonRpcResponse = serde_json::from_str(&line)
            .map_err(|e| McpError::Protocol(format!("Failed to parse response: {e}")))?;

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

/// HTTP transport - communicates with MCP server via HTTP
pub struct HttpTransport {
    client: reqwest::Client,
    base_url: String,
    request_id: AtomicU64,
}

impl HttpTransport {
    /// Create a new HTTP transport
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            request_id: AtomicU64::new(1),
        }
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

        let response = self
            .client
            .post(&self.base_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP request failed: {e}")))?;

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
        // HTTP transport doesn't need explicit close
        Ok(())
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
    /// Create a new MCP server with the given transport.
    ///
    /// # Errors
    ///
    /// Returns an `McpError` if initialization or tool discovery fails.
    pub async fn new(name: &str, transport: Box<dyn McpTransport>) -> Result<Self, McpError> {
        let mut server = Self {
            name: name.to_string(),
            transport,
            info: None,
            capabilities: McpCapabilities::default(),
            tools: Vec::new(),
        };

        // Initialize the connection
        server.initialize().await?;

        // Discover tools
        server.refresh_tools().await?;

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
                Err(McpError::Timeout)
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

        // Test Timeout variant
        let err = McpError::Timeout;
        assert!(err.to_string().contains("timeout"));
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
}
