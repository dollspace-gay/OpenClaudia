---
title: "ACP Protocol Server"
tags: [design-doc]
sources: []
contributors: [unknown]
created: 2026-03-23
updated: 2026-03-23
---


## Design Specification

### Summary

Add an ACP (Agent Client Protocol) server to OpenClaudia so it can interoperate with `acpx` and other agent harnesses. The server communicates over stdio using JSON-RPC 2.0, implementing all stable ACP methods. This enables multi-agent workflows where OpenClaudia acts as a coding agent alongside Claude Code, Codex, Gemini CLI, and others — all coordinated through `acpx`.

### Requirements

- REQ-1: New `openclaudia acp` CLI subcommand that starts a JSON-RPC 2.0 server on stdin/stdout
- REQ-2: Implement all stable ACP methods: `initialize`, `session/new`, `session/load`, `session/prompt`, `session/update`, `session/cancel`, `session/set_mode`, `session/set_config_option`, `authenticate`
- REQ-3: Delegate file operations to the ACP client via `fs/read_text_file` and `fs/write_text_file` requests instead of local filesystem access
- REQ-4: Delegate shell execution to the ACP client via `terminal/create`, `terminal/output`, `terminal/wait_for_exit`, `terminal/kill`, `terminal/release` requests
- REQ-5: Stream provider responses back as typed `session/update` notifications (thinking, text chunks, tool call status)
- REQ-6: Wrap existing `SessionManager` for session persistence, token tracking, and turn metrics
- REQ-7: Inherit the full `.openclaudia/config.yaml` configuration including hooks, rules, guardrails, and provider settings
- REQ-8: Support cancellation of in-flight prompts via `session/cancel`
- REQ-9: Run the agentic tool loop (prompt → tool calls → tool results → re-prompt) over ACP, delegating each tool through ACP client methods

### Acceptance Criteria

- [ ] AC-1: `openclaudia acp` starts and completes an `initialize` handshake with a JSON-RPC client over stdin/stdout
- [ ] AC-2: `acpx --agent "openclaudia acp" sessions new` creates a session and returns a valid session ID
- [ ] AC-3: `acpx --agent "openclaudia acp" "what is 2+2"` sends a prompt, receives streaming text updates, and gets a `stopReason: end_turn` result
- [ ] AC-4: When the model requests a `read_file` tool call, the ACP server sends `fs/read_text_file` to the client and returns the result to the model — no local file access occurs
- [ ] AC-5: When the model requests a `bash` tool call, the ACP server sends `terminal/create` to the client, collects output via `terminal/output`/`terminal/wait_for_exit`, and returns the result to the model
- [ ] AC-6: Anthropic extended thinking blocks are streamed as ACP thinking-type `session/update` notifications
- [ ] AC-7: `session/cancel` interrupts an in-flight prompt and returns `stopReason: cancelled`
- [ ] AC-8: Session state (token usage, turn metrics) persists across `session/load` reconnects
- [ ] AC-9: Hooks (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`) fire during ACP prompt execution with the same behavior as the interactive chat loop
- [ ] AC-10: `cargo test` includes unit tests for JSON-RPC message parsing, ACP method dispatch, and tool-to-ACP-client-method mapping

### Architecture

### New module: `src/acp.rs`

A new `acp` module registered in `src/lib.rs` alongside the existing modules. This is the largest new piece — it contains the ACP server, transport, method handlers, and tool bridge.

#### Transport layer

The ACP transport reads newline-delimited JSON-RPC 2.0 from stdin and writes to stdout. This mirrors the existing `StdioTransport` in `src/mcp.rs:139-255` but in the **server** direction — OpenClaudia reads requests and writes responses/notifications, whereas the MCP transport reads responses and writes requests.

```
stdin  → BufReader → JSON-RPC Request/Notification → dispatch
stdout ← JSON-RPC Response/Notification ← handler result
```

Key types reused from `mcp.rs:46-74`:
- `JsonRpcRequest` (adapted for deserialization on the server side)
- `JsonRpcResponse` (adapted for serialization on the server side)
- `JsonRpcError`

New types:
- `AcpServer` — main server struct, owns the transport, session state, config, and provider connection
- `AcpSession` — wraps `SessionManager` from `src/session.rs:873` with ACP-specific metadata (ACP session ID mapping, mode, config options)
- `AcpNotificationSender` — sends `session/update` notifications to stdout during streaming

#### Method dispatch

The server's main loop reads JSON-RPC messages and dispatches by method name:

| ACP Method | Handler | Notes |
|---|---|---|
| `initialize` | Return server info, capabilities, protocol version | Capabilities: prompts, tools, fs, terminal |
| `authenticate` | Validate credentials from config `auth` section | Pass-through if no auth configured |
| `session/new` | Create `Session::new_initializer()` via `SessionManager` | Return ACP session ID |
| `session/load` | Load persisted session via `SessionManager` | Match by ACP session ID |
| `session/prompt` | Execute the prompt loop (see below) | Streaming via notifications |
| `session/cancel` | Set cancellation flag on in-flight prompt | Checked in streaming loop |
| `session/set_mode` | Map to `SessionMode` (`Initializer`/`Coding`) | Reject unknown modes |
| `session/set_config_option` | Store in ACP session config map | Provider-specific pass-through |

#### Prompt execution flow

`session/prompt` is the core method. It runs the same agentic loop as `cmd_chat` in `src/main.rs:3632` but restructured for ACP:

```
1. Receive session/prompt request with prompt text
2. Load config, provider adapter (reuse config::load_config(), providers::get_adapter())
3. Build messages array from session history + new user message
4. Inject context: rules (RulesEngine), hooks (HookEngine), system prompt (context.rs)
5. Send request to provider (reuse existing reqwest-based streaming)
6. Stream response:
   a. Anthropic thinking blocks → session/update {type: "thinking", content: ...}
   b. Text deltas → session/update {type: "agent_message_chunk", content: {type: "text", text: ...}}
   c. Tool use blocks → session/update {type: "tool_call", title: ..., status: "running"}
7. On tool calls:
   a. Map tool name to ACP client method (see Tool Bridge below)
   b. Send JSON-RPC request to client (write to stdout, read response from stdin)
   c. session/update {type: "tool_call", title: ..., status: "completed", output: ...}
   d. Feed result back to provider, goto step 4
8. On completion:
   a. Record turn metrics in SessionManager
   b. Return session/prompt result with stopReason
```

Cancellation: A `tokio::sync::watch` channel carries the cancel flag. The streaming loop and tool execution check it between chunks/operations. When set, the loop breaks and returns `stopReason: cancelled`.

#### Tool bridge: OpenClaudia tools → ACP client methods

This is the key architectural change. Instead of `tools::execute_tool_full()` (`src/tools.rs:2419`) running tools locally, the ACP mode maps tool calls to ACP client requests:

| OpenClaudia Tool | ACP Client Method | Mapping |
|---|---|---|
| `bash` | `terminal/create` → `terminal/output` → `terminal/wait_for_exit` | command → terminal lifecycle |
| `kill_shell` | `terminal/kill` | shell_id → terminal_id |
| `read_file` | `fs/read_text_file` | file_path, offset, limit → path, range |
| `write_file` | `fs/write_text_file` | file_path, content → path, content |
| `edit_file` | `fs/read_text_file` + `fs/write_text_file` | Read, apply edit, write back |
| `list_files` | `terminal/create` with `ls` command | Delegate as shell command |
| `web_fetch` | Local execution (not a client operation) | Keep local — no ACP method exists |
| `web_search` | Local execution | Keep local |
| `memory_*` | Local execution | Internal to OpenClaudia |
| `task_*` | Local execution | Internal to OpenClaudia |

The tool bridge is implemented as an `AcpToolExecutor` struct that implements a trait compatible with the existing tool execution interface. It holds a reference to the ACP transport for sending client requests.

For `edit_file`, the bridge performs: read via `fs/read_text_file` → apply the string replacement locally → write via `fs/write_text_file`. This preserves the atomic edit semantics while delegating all I/O to the client.

For `bash`, the bridge manages the full terminal lifecycle:
1. `terminal/create` with the command → get terminal_id
2. Poll `terminal/output` for stdout/stderr chunks
3. `terminal/wait_for_exit` for the exit code
4. `terminal/release` to clean up

Background shells (`run_in_background: true`) map to `terminal/create` without `terminal/wait_for_exit`, with the terminal_id stored for later `bash_output` → `terminal/output` calls.

#### Bidirectional JSON-RPC over stdio

ACP requires bidirectional communication over a single stdin/stdout pair:
- **Client → Server**: requests (`initialize`, `session/prompt`, etc.)
- **Server → Client**: responses + notifications (`session/update`)
- **Server → Client → Server**: nested requests (`fs/read_text_file` request, client responds)

This means the stdin reader must handle both client requests and client responses to server-initiated requests. The transport layer uses a message router:

```
stdin → JSON-RPC message
  ├── Has "method" field? → client request or notification → dispatch to handler
  └── Has "result"/"error" field? → response to our pending request → resolve pending future
```

Pending server-initiated requests are tracked in a `HashMap<u64, oneshot::Sender<Value>>` keyed by request ID. When the server sends `fs/read_text_file`, it inserts a sender and awaits the receiver. When a response arrives on stdin with a matching ID, the router resolves the sender.

### CLI subcommand: `Acp` variant in `Commands` enum

Added to `src/main.rs:33-88`:

```rust
/// Start ACP server on stdin/stdout for agent interoperability
Acp {
    /// Target provider (overrides config)
    #[arg(short, long)]
    target: Option<String>,

    /// Model to use
    #[arg(short, long)]
    model: Option<String>,
},
```

Handler `cmd_acp()` loads config, initializes all engines (HookEngine, RulesEngine, GuardrailsEngine, SessionManager), and starts the ACP server loop.

### Module registration

`src/lib.rs` gets one new line:
```rust
pub mod acp;
```

### Config for acpx agent registry

Users register OpenClaudia in their `~/.acpx/config.json`:
```json
{
  "agents": {
    "openclaudia": { "command": "openclaudia acp" }
  }
}
```

Then: `acpx openclaudia "fix the tests"` just works.

### Logging

All logging goes to stderr (via `tracing`), never stdout. Stdout is reserved exclusively for JSON-RPC. This matches the convention used by MCP stdio servers and is critical for protocol correctness.

### Error handling

JSON-RPC errors use standard codes:
- `-32700` Parse error (malformed JSON)
- `-32600` Invalid request (missing required fields)
- `-32601` Method not found (unknown ACP method)
- `-32602` Invalid params
- `-32603` Internal error (provider failure, etc.)

Provider errors (non-2xx, timeout) are surfaced as JSON-RPC internal errors with the provider's error message in the `data` field.

### Out of Scope

- Unstable ACP methods (`session/fork`, `session/list`, `session/resume`, `session/set_model`, `$/cancel_request`) — implement after spec stabilizes
- TUI mode over ACP — the ACP server is headless by design, TUI remains a separate mode
- Multi-session concurrency — the ACP server handles one session at a time per process (acpx spawns separate processes for named sessions)
- ACP client mode (OpenClaudia calling other ACP agents) — this is the inverse direction and a separate feature
- WebSocket or HTTP transport for ACP — stdio only for now, matching acpx's transport expectations
- Custom permission policies beyond what acpx provides (path-based rules, etc.)

