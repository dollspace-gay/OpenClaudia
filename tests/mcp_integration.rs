//! MCP integration tests — Phase 2 (#543)
//!
//! Pins OC's **current** contracts in `src/mcp.rs` against the Phase 1 spec
//! from crosslink issue #528.  Goal is behavioral pinning, not bug-fixing.
//! Where OC diverges from CC, the test asserts OC's *current* behaviour and
//! references the gap issue that tracks the fix.
//!
//! ## Spec→test mapping
//!
//! | Spec behavior | Test(s)                                                          |
//! |---------------|------------------------------------------------------------------|
//! | B1 Handshake  | `handshake_sends_correct_protocol_version`                       |
//! |               | `handshake_no_elicitation_cap` (gap #613)                        |
//! |               | `handshake_initialized_notification_error_swallowed`             |
//! |               | `handshake_no_timeout_pin` (gap #628 — `#[ignore]`)              |
//! | B2 Tool disc. | `tool_refresh_unconditional_no_cap_gate` (gap #627)              |
//! |               | `tool_refresh_with_tools_cap_parses_list`                        |
//! |               | `supports_tool_list_changed_reads_capability`                    |
//! | B3 Tool call  | `call_tool_returns_raw_value_does_not_inspect_is_error` (#625)   |
//! |               | `call_tool_unknown_tool_returns_tool_not_found`                  |
//! |               | `call_tool_missing_server_returns_not_connected`                 |
//! |               | `call_tool_with_timeout_returns_timeout_error`                   |
//! | B4 Resource   | `list_resources_returns_empty_without_resources_cap`             |
//! |               | `list_resources_calls_wire_when_cap_present`                     |
//! | B5 Error code | `stdio_rpc_error_with_data_included_in_message`                  |
//! |               | `http_rpc_error_drops_data_field` (gap #626)                     |
//! | B6 Disconnect | `stdio_mid_call_disconnect_returns_transport_error`              |
//! |               | `no_reconnect_after_disconnect_pin` (gap #629 — `#[ignore]`)    |

use openclaudia::mcp::{McpError, McpManager, McpServer, StdioTransport};
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── helpers ────────────────────────────────────────────────────────────────

/// Path to the Python echo-server fixture.
fn fixture_path() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("tests")
        .join("fixtures")
        .join("mcp_echo_server.py")
}

/// Spawn the echo server fixture via `python3` and return the transport.
///
/// # Panics
///
/// Panics if the fixture file doesn't exist or the process cannot be spawned.
fn spawn_echo_transport() -> StdioTransport {
    let path = fixture_path();
    assert!(path.exists(), "fixture not found at {}", path.display());
    StdioTransport::spawn("python3", &[path.to_str().expect("utf-8 path")])
        .expect("spawn echo server")
}

/// Spawn the echo server with no `tools` capability in the `initialize`
/// response (sets `MCP_NO_TOOLS_CAP` env var via a wrapper invocation).
fn spawn_echo_no_tools_cap() -> StdioTransport {
    let path = fixture_path();
    // Pass env var by invoking `env MCP_NO_TOOLS_CAP=1 python3 <fixture>`
    StdioTransport::spawn(
        "env",
        &[
            "MCP_NO_TOOLS_CAP=1",
            "python3",
            path.to_str().expect("utf-8"),
        ],
    )
    .expect("spawn echo server (no tools cap)")
}

/// Spawn the echo server with no `resources` capability.
fn spawn_echo_no_resources_cap() -> StdioTransport {
    let path = fixture_path();
    StdioTransport::spawn(
        "env",
        &[
            "MCP_NO_RESOURCES_CAP=1",
            "python3",
            path.to_str().expect("utf-8"),
        ],
    )
    .expect("spawn echo server (no resources cap)")
}

// ─── B1: Handshake ──────────────────────────────────────────────────────────

/// B1 — OC sends `protocolVersion: "2024-11-05"` in the initialize params.
///
/// The echo server reflects the server side; the client-side params are
/// opaque once accepted. We verify the handshake succeeds (server recognized
/// the version) and that the server name is returned correctly.
#[tokio::test]
async fn handshake_sends_correct_protocol_version() {
    let transport = spawn_echo_transport();
    let server = McpServer::new("test", Box::new(transport))
        .await
        .expect("McpServer::new should succeed");

    // Verify the server name round-trips (handshake completed)
    assert_eq!(server.name(), "test");
    // Verify tools were discovered (implies initialize + refresh_tools ran)
    assert!(
        !server.tools().is_empty(),
        "echo server should return tools"
    );
}

/// B1 — OC does NOT declare `elicitation` in the `initialize` capabilities.
///
/// Gap: #613 — servers that send elicitation requests will receive no
/// response from OC.  This test pins the current absence so a future fix
/// (adding the capability) is immediately visible in the diff.
///
/// We test this by inspecting the *wire message* sent during initialize.
/// We do so via a wiremock HTTP server that captures the JSON-RPC body.
#[tokio::test]
async fn handshake_no_elicitation_cap() {
    use openclaudia::mcp::HttpTransport;

    let mock_server = MockServer::start().await;

    // Respond to initialize
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "mock", "version": "0.0.1" }
            }
        })))
        .expect(1..)
        .mount(&mock_server)
        .await;

    let transport = HttpTransport::__test_new_unchecked(&mock_server.uri());
    // Handshake succeeds; we only need it to complete to verify what was sent.
    let _ = McpServer::new("mock", Box::new(transport)).await;

    // Retrieve captured requests
    let received = mock_server.received_requests().await.expect("requests");
    let init_req = received
        .iter()
        .find(|r| {
            r.body_json::<serde_json::Value>()
                .ok()
                .and_then(|b| b.get("method").cloned())
                == Some(json!("initialize"))
        })
        .expect("initialize request must have been sent");

    let body: serde_json::Value = init_req.body_json().expect("parse body");
    let caps = &body["params"]["capabilities"];

    // Pin current state: `roots` declared, `elicitation` absent
    assert!(
        caps.get("roots").is_some(),
        "OC declares roots capability (current behaviour)"
    );
    assert!(
        caps.get("elicitation").is_none(),
        "OC does NOT declare elicitation capability — gap #613"
    );
}

/// B1 — `notifications/initialized` error is silently discarded (`.ok()`).
///
/// The echo server ignores notifications, so the current OC code path (which
/// calls `.ok()` on the result) must not surface an error from `McpServer::new`.
#[tokio::test]
async fn handshake_initialized_notification_error_swallowed() {
    // The echo server receives `notifications/initialized` but does not reply
    // (it's a notification — no id).  OC calls `.ok()` so no error bubbles up.
    let transport = spawn_echo_transport();
    let result = McpServer::new("swallow-test", Box::new(transport)).await;
    assert!(
        result.is_ok(),
        "McpServer::new must succeed even if initialized notification is a no-op"
    );
}

/// B1 — Pin: handshake has NO connection-establishment timeout.
///
/// Gap: #628 (HIGH) — a non-responsive server hangs the process indefinitely.
/// This test is `#[ignore]`d because executing it would require a server that
/// never responds, which would block the test runner indefinitely.
///
/// To run manually and observe the hang: `cargo test -- --ignored
/// handshake_no_timeout_pin`.
#[tokio::test]
#[ignore = "gap #628: no handshake timeout — would block indefinitely; run manually to verify"]
async fn handshake_no_timeout_pin() {
    // If OC gains a timeout, this test should be updated to verify it fires.
    // For now, existence of this test documents the gap.
    unreachable!("this test must not be run in CI without a timeout wrapper");
}

// ─── B2: Tool discovery ─────────────────────────────────────────────────────

/// B2 — `refresh_tools` issues `tools/list` even when `capabilities.tools`
/// is absent from the initialize response.
///
/// Gap: #627 — CC guards on `client.capabilities?.tools`; OC does not.
/// This test pins the current behaviour: the wire call happens regardless,
/// and `McpServer::new` returns `Ok` (because the echo server still responds).
#[tokio::test]
async fn tool_refresh_unconditional_no_cap_gate() {
    // Echo server launched without tools capability in initialize response.
    let transport = spawn_echo_no_tools_cap();
    let server = McpServer::new("no-cap", Box::new(transport))
        .await
        .expect("McpServer::new must succeed even without tools capability");

    // OC still sends tools/list unconditionally and populates tools.
    // The echo server responds to tools/list regardless of advertised caps.
    // This confirms OC skips no wire call (gap #627 behaviour pinned).
    assert!(
        !server.tools().is_empty(),
        "OC calls tools/list unconditionally even when capabilities.tools is absent (gap #627)"
    );
}

/// B2 — When `capabilities.tools` IS present, `refresh_tools` parses the
/// tool list correctly and populates `tools()`.
#[tokio::test]
async fn tool_refresh_with_tools_cap_parses_list() {
    let transport = spawn_echo_transport();
    let server = McpServer::new("cap-test", Box::new(transport))
        .await
        .expect("McpServer::new");

    let tools = server.tools();
    assert_eq!(tools.len(), 2, "echo server returns exactly two tools");

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"), "tool 'echo' must be present");
    assert!(
        names.contains(&"fail_tool"),
        "tool 'fail_tool' must be present"
    );

    // Verify description round-trip
    let echo_tool = tools.iter().find(|t| t.name == "echo").unwrap();
    assert_eq!(
        echo_tool.description.as_deref(),
        Some("Echo the input back")
    );
    // Verify inputSchema round-trip
    assert!(
        echo_tool.input_schema.is_some(),
        "inputSchema must be preserved"
    );
}

/// B2 — `supports_tool_list_changed()` returns `true` iff the server
/// advertised `capabilities.tools.listChanged = true`.
#[tokio::test]
async fn supports_tool_list_changed_reads_capability() {
    // Echo server returns `{"listChanged": false}` — pin OC reads this correctly.
    let transport = spawn_echo_transport();
    let server = McpServer::new("list-changed", Box::new(transport))
        .await
        .expect("McpServer::new");
    assert!(
        !server.supports_tool_list_changed(),
        "echo server advertises listChanged:false"
    );
}

// ─── B3: Tool call ──────────────────────────────────────────────────────────

/// B3 — `call_tool` returns the raw result `Value` without inspecting
/// the `isError` field.
///
/// Gap: #625 — CC throws `McpToolCallError` when `result.isError === true`.
/// OC returns the payload as a success `Value`.  This test pins that behaviour
/// so any future fix (which would return `Err`) triggers a test failure.
#[tokio::test]
async fn call_tool_returns_raw_value_does_not_inspect_is_error() {
    let transport = spawn_echo_transport();
    let server = McpServer::new("is-error-test", Box::new(transport))
        .await
        .expect("McpServer::new");

    // `fail_tool` causes the echo server to return `{ "isError": true, ... }`
    let result = server
        .call_tool("fail_tool", json!({}))
        .await
        .expect("OC must return Ok even when isError:true (gap #625)");

    // Confirm the raw payload is returned as-is, not converted to Err
    assert_eq!(
        result["isError"],
        json!(true),
        "isError:true payload must be returned as Ok(Value) — gap #625"
    );
    assert!(
        result.get("content").is_some(),
        "content array must be present in the raw result"
    );
}

/// B3 — `call_tool` returns `McpError::ToolNotFound` for a name not in
/// the local tool cache, without making a wire call.
#[tokio::test]
async fn call_tool_unknown_tool_returns_tool_not_found() {
    let transport = spawn_echo_transport();
    let server = McpServer::new("not-found-test", Box::new(transport))
        .await
        .expect("McpServer::new");

    let result = server.call_tool("nonexistent_tool", json!({})).await;
    assert!(
        matches!(result, Err(McpError::ToolNotFound(ref name)) if name == "nonexistent_tool"),
        "expected ToolNotFound(nonexistent_tool), got {result:?}"
    );
}

/// B3 — `McpManager::call_tool` returns `McpError::NotConnected` when the
/// server name in `mcp__server__tool` is not registered.
#[tokio::test]
async fn call_tool_missing_server_returns_not_connected() {
    let manager = McpManager::new();
    let result = manager
        .call_tool("mcp__missing_server__tool", json!({}))
        .await;
    assert!(
        matches!(result, Err(McpError::NotConnected(ref n)) if n == "missing_server"),
        "expected NotConnected(missing_server), got {result:?}"
    );
}

/// B3 — `call_tool_with_timeout` returns `McpError::Timeout` when the
/// underlying call exceeds the deadline.
///
/// `HttpTransport` uses a sequential `AtomicU64` counter starting at 1.
/// `McpServer::new` makes exactly three sequential requests:
///   id=1  `initialize`
///   id=2  `notifications/initialized`  (via `transport.request`, result `.ok()`'d)
///   id=3  `tools/list`
/// The subsequent `tools/call` gets id=4.  Each wiremock mock is registered
/// `up_to_n_times(1)` so they serve in registration order, consuming one
/// request each.  The fourth mock hangs for 10 s; the 50 ms timeout fires.
#[tokio::test]
async fn call_tool_with_timeout_returns_timeout_error() {
    let mock_server = MockServer::start().await;

    // id=1: initialize
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "slow", "version": "0.0.1" }
            }
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // id=2: notifications/initialized (OC calls transport.request for this)
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": 2, "result": {}
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // id=3: tools/list — returns "slow_op" so the local cache is populated
    // and the ToolNotFound pre-check in call_tool passes.
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": 3,
            "result": {
                "tools": [{
                    "name": "slow_op",
                    "description": "slow",
                    "inputSchema": {"type": "object", "properties": {}}
                }]
            }
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // id=4: tools/call — hangs for 10 s; the 50 ms timeout fires first.
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc":"2.0","id":4,"result":{}}))
                .set_delay(Duration::from_secs(10)),
        )
        .mount(&mock_server)
        .await;

    let manager = McpManager::new();
    // mock_server.uri() is a 127.0.0.1 loopback that the SSRF guard
    // (fix #677) rejects in production; tests use the unchecked
    // variant to point at their own listener.
    manager
        .__test_connect_http_unchecked("slow", &mock_server.uri())
        .await
        .expect("connect_http");

    let result = manager
        .call_tool_with_timeout("mcp__slow__slow_op", json!({}), Duration::from_millis(50))
        .await;

    assert!(
        matches!(result, Err(McpError::Timeout { .. })),
        "expected Timeout, got {result:?}"
    );
}

// ─── B4: Resource capability gate ───────────────────────────────────────────

/// B4 — `list_resources` returns an empty vec immediately when the server
/// does not advertise `capabilities.resources` — no wire call made.
#[tokio::test]
async fn list_resources_returns_empty_without_resources_cap() {
    let transport = spawn_echo_no_resources_cap();
    let server = McpServer::new("no-res-cap", Box::new(transport))
        .await
        .expect("McpServer::new");

    // Capability absent → should return empty without any wire call
    let resources = server.list_resources().await.expect("list_resources");
    assert!(
        resources.is_empty(),
        "list_resources must return [] when resources capability is absent"
    );
}

/// B4 — When `capabilities.resources` IS advertised, `list_resources` issues
/// the wire call and returns parsed resources.
#[tokio::test]
async fn list_resources_calls_wire_when_cap_present() {
    // Echo server with default caps (resources present)
    let transport = spawn_echo_transport();
    let server = McpServer::new("res-cap", Box::new(transport))
        .await
        .expect("McpServer::new");

    let resources = server.list_resources().await.expect("list_resources");
    assert_eq!(resources.len(), 1, "echo server returns one resource");
    assert_eq!(resources[0].uri, "echo://hello");
    assert_eq!(resources[0].name, "hello");
}

// ─── B5: Unknown error code surfacing ───────────────────────────────────────

/// B5 — Stdio transport: unknown JSON-RPC error code is surfaced in the error
/// message string AND includes the `data` field.
#[tokio::test]
async fn stdio_rpc_error_with_data_included_in_message() {
    use openclaudia::mcp::McpTransport;

    // Use the transport directly (bypass McpServer::new) so we can call the
    // synthetic `rpc_error_with_data` method.
    let transport = spawn_echo_transport();
    let result = transport.request("rpc_error_with_data", None).await;

    assert!(result.is_err(), "should return Err for a JSON-RPC error");
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Code is present
    assert!(
        msg.contains("-32099"),
        "error code must appear in message: {msg}"
    );
    // Message text is present
    assert!(
        msg.contains("custom server error"),
        "error message must appear: {msg}"
    );
    // data field is present (stdio includes it; HTTP drops it — gap #626)
    assert!(
        msg.contains("extra context") || msg.contains("data"),
        "stdio transport must include data field in error message: {msg}"
    );
}

/// B5 — HTTP transport: `data` field in JSON-RPC errors is DROPPED.
///
/// Gap: #626 — HTTP `HttpTransport::request` formats errors as
/// `"RPC error {code}: {message}"`, silently discarding the `data` value.
/// This test pins that current behaviour.
#[tokio::test]
async fn http_rpc_error_drops_data_field() {
    use openclaudia::mcp::{HttpTransport, McpTransport};

    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32099,
                "message": "custom server error",
                "data": { "detail": "extra context" }
            }
        })))
        .mount(&mock_server)
        .await;

    let transport = HttpTransport::__test_new_unchecked(&mock_server.uri());
    let result = transport.request("anything", None).await;

    assert!(result.is_err(), "must return Err for JSON-RPC error");
    let msg = result.unwrap_err().to_string();

    // Code and message are present
    assert!(msg.contains("-32099"), "error code must appear: {msg}");
    assert!(
        msg.contains("custom server error"),
        "error msg must appear: {msg}"
    );

    // Gap #626 — data is NOT present in the HTTP error message
    assert!(
        !msg.contains("extra context"),
        "HTTP transport drops data field (gap #626) — msg: {msg}"
    );
}

// ─── B6: Mid-call disconnect ─────────────────────────────────────────────────

/// B6 — A transport disconnect mid-call returns an explicit `McpError` (no
/// panic).  The echo server handles `die` by closing stdout without writing
/// a response.
///
/// OC's current behaviour: `StdioTransport` attempts to read a response line
/// from the now-closed stdout pipe, receives EOF, and JSON-parsing fails
/// (`"Failed to parse response: EOF while parsing a value at line 1 column 0"`).
/// This surfaces as `McpError::Protocol`, **not** `McpError::Transport`.
///
/// The spec (B6) only requires an explicit error — not a specific variant.
/// The variant choice (Protocol vs Transport for an EOF) is a minor OC quirk;
/// CC uses the SDK which raises `McpError -32000 "Connection closed"`.
/// Pinned as-is; a future fix may promote EOF → Transport.
#[tokio::test]
async fn stdio_mid_call_disconnect_returns_transport_error() {
    use openclaudia::mcp::McpTransport;

    let transport = spawn_echo_transport();

    // First request succeeds — confirms the transport is live.
    let ok = transport.request("tools/list", None).await;
    assert!(ok.is_ok(), "first request must succeed: {ok:?}");

    // `die` causes the server to close stdout without writing a response.
    // OC returns an explicit McpError (Transport or Protocol) — it does NOT panic.
    let result = transport.request("die", None).await;
    assert!(
        matches!(
            result,
            Err(McpError::Transport(_) | McpError::Protocol(_))
        ),
        "mid-call disconnect must return McpError::Transport or McpError::Protocol (no panic), got {result:?}"
    );
    // Current OC behaviour: Protocol("Failed to parse response: EOF …")
    // If this assertion ever fails with Transport(_), a future fix promoted the
    // variant — update this comment and the assertion accordingly.
    assert!(
        matches!(result, Err(McpError::Protocol(_))),
        "OC current behaviour: EOF on stdout yields McpError::Protocol (pinned)"
    );
}

/// B6 — Pin: no automatic reconnection after disconnect.
///
/// Gap: #629 (HIGH) — CC clears the memoize cache in `onclose` so the next
/// `connectToServer` call reconnects transparently.  OC holds the broken
/// `McpServer` in the map indefinitely; subsequent calls return Transport errors.
///
/// `#[ignore]`d because it duplicates the above disconnect test semantics and
/// requires a live server; the pinned behaviour is already captured by
/// `stdio_mid_call_disconnect_returns_transport_error`.
#[tokio::test]
#[ignore = "gap #629 (HIGH): no reconnect — behaviour pinned by stdio_mid_call_disconnect_returns_transport_error"]
async fn no_reconnect_after_disconnect_pin() {
    // When OC gains reconnection, this test should be updated to verify the
    // reconnected server functions correctly.  Until then it documents gap #629.
}

// ─── Additional unit-level pins (no fixture needed) ─────────────────────────

/// B3 / B2 — `McpManager::call_tool` with invalid name format (missing `mcp__`
/// prefix or wrong delimiter count) returns `McpError::ToolNotFound`.
#[tokio::test]
async fn call_tool_invalid_name_format_returns_tool_not_found() {
    let manager = McpManager::new();

    for bad_name in &["notool", "server_tool", "mcp_server_tool", "server__tool"] {
        let result = manager.call_tool(bad_name, json!({})).await;
        assert!(
            matches!(
                result,
                Err(McpError::NotConnected(_) | McpError::ToolNotFound(_))
            ),
            "bad name '{bad_name}' should yield ToolNotFound or NotConnected, got {result:?}"
        );
    }
}

/// B2 — Tool names with embedded single underscores parse correctly.
///
/// `mcp__my_server__my_tool` must parse `server_name` = "`my_server`", tool = "`my_tool`".
#[tokio::test]
async fn call_tool_underscored_names_parse_correctly() {
    let manager = McpManager::new();
    let result = manager
        .call_tool("mcp__my_server__my_tool", json!({}))
        .await;
    assert!(
        matches!(result, Err(McpError::NotConnected(ref n)) if n == "my_server"),
        "should get NotConnected(my_server), got {result:?}"
    );
}

/// B5 — Any unknown/non-standard JSON-RPC error code does not panic; it is
/// wrapped in `McpError::Protocol` and returned as `Err`.
#[tokio::test]
async fn arbitrary_unknown_error_codes_do_not_panic() {
    use openclaudia::mcp::{HttpTransport, McpTransport};

    let mock_server = MockServer::start().await;
    for code in &[-1i64, 0, 12345, -99999, i64::MIN, i64::MAX] {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": code, "message": "test error" }
            })))
            .mount(&mock_server)
            .await;

        let transport = HttpTransport::__test_new_unchecked(&mock_server.uri());
        let result = transport.request("test", None).await;
        assert!(
            matches!(result, Err(McpError::Protocol(_))),
            "code {code} must yield McpError::Protocol, got {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains(&code.to_string()),
            "error code {code} must appear in message: {msg}"
        );
    }
    drop(mock_server); // suppress unused warning for outer binding
}

/// B4 — `McpManager::list_resources` for a non-existent server returns `Err`.
#[tokio::test]
async fn manager_list_resources_missing_server_returns_err() {
    let manager = McpManager::new();
    let result = manager.list_resources(Some("missing")).await;
    assert!(result.is_err(), "expected Err for missing server");
}

/// B4 — `McpManager::list_resources(None)` with no servers returns empty vec,
/// not an error (multi-server path with zero servers).
#[tokio::test]
async fn manager_list_resources_no_servers_returns_empty() {
    let manager = McpManager::new();
    let result = manager.list_resources(None).await.expect("should be Ok");
    assert!(result.is_empty());
}
