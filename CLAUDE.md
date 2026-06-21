# CLAUDE.md - OpenClaudia Development Guide

## Project Overview

OpenClaudia is an open-source universal agent harness that provides Claude Code-like capabilities for any AI. It acts as a proxy server translating between OpenAI-compatible formats and multiple provider APIs (Anthropic, OpenAI, Google Gemini, DeepSeek, Qwen, Z.AI/GLM).

## Architecture Map

```
                              ┌─────────────────────────────────────────────────────────────┐
                              │                      OpenClaudia                            │
                              └─────────────────────────────────────────────────────────────┘
                                                          │
                    ┌─────────────────────────────────────┼─────────────────────────────────┐
                    │                                     │                                 │
                    ▼                                     ▼                                 ▼
            ┌───────────────┐                    ┌───────────────┐                 ┌───────────────┐
            │    main.rs    │                    │    tui/       │                 │    web.rs     │
            │  CLI Entry    │                    │  Terminal UI  │                 │  Web Scraping │
            │  (clap)       │                    │  (ratatui)    │                 │  (headless)   │
            └───────┬───────┘                    └───────────────┘                 └───────────────┘
                    │
        ┌───────────┼───────────┬───────────────────────┬───────────────────────┐
        │           │           │                       │                       │
        ▼           ▼           ▼                       ▼                       ▼
┌───────────┐ ┌───────────┐ ┌───────────┐       ┌───────────────┐       ┌───────────────┐
│ config/   │ │ proxy.rs  │ │ session/  │       │   hooks/      │       │   rules.rs    │
│ YAML +    │ │ HTTP Proxy│ │ State Mgmt│       │ Pre/Post Tool │       │ CLAUDE.md     │
│ Env Vars  │ │ (axum)    │ │ Turns     │       │ Lifecycle     │       │ .clauderules  │
└─────┬─────┘ └─────┬─────┘ └───────────┘       └───────────────┘       └───────────────┘
      │             │
      │             ▼
      │     ┌───────────────────────────────────────────────────────────────────────────┐
      │     │                           providers/                                     │
      │     │                    ProviderAdapter trait + Implementations                │
      │     └───────────────────────────────────────────────────────────────────────────┘
      │                                         │
      │         ┌─────────────┬─────────────┬───┴───────┬─────────────┬─────────────┐
      │         ▼             ▼             ▼           ▼             ▼             ▼
      │    ┌─────────┐   ┌─────────┐   ┌─────────┐ ┌─────────┐   ┌─────────┐   ┌─────────┐
      │    │Anthropic│   │ OpenAI  │   │ Google  │ │DeepSeek │   │  Qwen   │   │  Z.AI   │
      │    │ Adapter │   │ Adapter │   │ Gemini  │ │ Adapter │   │ Adapter │   │  GLM    │
      │    └─────────┘   └─────────┘   └─────────┘ └─────────┘   └─────────┘   └─────────┘
      │
      ▼
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                                     tools/                                              │
│        bash | read | write | edit | glob | grep | web_fetch | memory_* | crosslink     │
└─────────────────────────────────────────────────────────────────────────────────────────┘
              │                                           │
              ▼                                           ▼
      ┌───────────────┐                           ┌───────────────┐
      │   memory.rs   │                           │ compaction.rs │
      │ SQLite Store  │                           │ Context Mgmt  │
      │ Core/Archival │                           │ Summarization │
      └───────────────┘                           └───────────────┘
              │
              ▼
      ┌───────────────┐       ┌───────────────┐
      │   mcp.rs      │       │  plugins/     │
      │ MCP Protocol  │       │ Extension     │
      │ (stdio/http)  │       │ Framework     │
      └───────────────┘       └───────────────┘
```

## Module Responsibilities

Most subsystems are directory modules (e.g. `config/mod.rs`), not single
files. Single-file modules keep the `.rs` suffix below.

| Module          | Purpose                                                    |
|-----------------|-----------------------------------------------------------|
| `main.rs`       | CLI entry point, subcommands (init, start, chat, loop)    |
| `cli/`          | Subcommand implementations (auth, init, start, etc.)      |
| `config/`       | YAML config + env var loading, provider/hook definitions  |
| `proxy.rs`      | HTTP server (axum), request/response translation          |
| `providers/`    | Provider adapters (Anthropic, OpenAI, Google, etc.)       |
| `tools/`        | Tool definitions and execution (bash, read, write, edit)  |
| `memory.rs`     | SQLite-based archival + core memory (MemGPT-style)        |
| `memdir/`       | MEMORY.md entrypoint discovery, loading, and truncation    |
| `session/`      | Conversation state, turn management                        |
| `state/`        | Shared runtime/session state types                         |
| `hooks/`        | Lifecycle hooks (pre/post tool, session start/end)        |
| `rules.rs`      | CLAUDE.md and .clauderules parsing                         |
| `tui/`          | Terminal UI with ratatui                                   |
| `web.rs`        | Web scraping with headless Chrome                          |
| `compaction.rs` | Context window management, automatic summarization         |
| `mcp.rs`        | Model Context Protocol server support                      |
| `plugins/`      | Plugin/extension framework                                 |
| `coordinator/`  | Multi-agent / subagent coordination                        |
| `vdd/`          | Verification-driven development workflow                   |
| `modes/`        | Agency/quality/scope prompt axes                           |
| `services/`     | Background and shared services                             |
| `speculation/`  | Speculative execution support                              |
| `context.rs`    | System prompt and context construction                     |
| `prompt.rs`     | Prompt templates and formatting                            |

## Rust Best Practices

### Error Handling
- Use `thiserror` for library errors with structured variants
- Use `anyhow` for application-level error propagation
- Prefer `?` operator over `.unwrap()` in production code
- Provide context with `.context()` from anyhow

```rust
// Good
fn read_config(path: &Path) -> anyhow::Result<Config> {
    let content = fs::read_to_string(path)
        .context("Failed to read config file")?;
    serde_yaml::from_str(&content)
        .context("Failed to parse YAML config")
}

// Avoid
fn read_config(path: &Path) -> Config {
    let content = fs::read_to_string(path).unwrap();  // Panics!
    serde_yaml::from_str(&content).unwrap()
}
```

### Async Patterns
- Use `tokio` runtime (already configured)
- Prefer `async_trait` for trait methods that need async
- Use `tokio::spawn` for concurrent tasks
- Avoid blocking in async contexts

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn send_request(&self, req: Value) -> Result<Value, ProviderError>;
}
```

### Type Safety
- Use newtype pattern for domain types
- Prefer enums over stringly-typed code
- Use `Option<T>` explicitly rather than sentinel values
- Leverage the type system to make invalid states unrepresentable

```rust
// Good
enum MessageRole { User, Assistant, System }

// Avoid
type Role = String;  // "user" | "assistant" | "system"
```

### Code Organization
- Keep modules focused (single responsibility)
- Use `pub(crate)` for internal APIs
- Prefer composition over inheritance
- Use traits for polymorphism

### Testing
- Write unit tests in the same file with `#[cfg(test)]`
- Use `tempfile` crate for filesystem tests
- Mock external services in tests
- Test error paths, not just happy paths

### Performance
- Use `&str` over `String` when borrowing
- Prefer `Vec::with_capacity()` when size is known
- Use `clone()` sparingly; prefer references
- Profile before optimizing

### Formatting and Linting
Always run before committing:
```bash
cargo fmt           # Format code
cargo clippy -- -D warnings   # Lint with warnings as errors
cargo test          # Run tests
```

## Configuration Files

### `.openclaudia/config.yaml`
Provider configuration, hooks, keybindings:
```yaml
proxy:
  port: 8080
  target: anthropic

providers:
  anthropic:
    base_url: https://api.anthropic.com/v1
    api_key: ${ANTHROPIC_API_KEY}
    thinking:
      enabled: true
      budget_tokens: 10000
```

### `.chainlink/` Directory
Issue-tracking SQLite database (`issues.db`) and metadata for the VDD
workflow, managed via the `crosslink` crate/CLI (the `chainlink` name is
legacy; the current tool is `crosslink`).

### `.claude/` Directory
Custom slash commands and project-specific prompts.

## Data Flow

```
User Input → TUI/CLI → Session → Proxy → Provider Adapter → External API
                         ↓
                    Tool Calls ←──────────────────────────────┘
                         ↓
                  Tool Execution (bash/read/write/edit)
                         ↓
                  Memory Storage (if --stateful)
                         ↓
                    Response → TUI/CLI → User
```

## Key Dependencies

| Crate              | Purpose                           |
|--------------------|-----------------------------------|
| `axum`             | HTTP server framework             |
| `reqwest`          | HTTP client for upstream APIs     |
| `serde`/`serde_json`| Serialization                    |
| `tokio`            | Async runtime                     |
| `clap`             | CLI argument parsing              |
| `ratatui`          | Terminal UI                       |
| `rusqlite`         | SQLite for memory storage         |
| `thiserror`        | Error type derivation             |
| `anyhow`           | Error propagation                 |
| `tracing`          | Structured logging                |

## Thinking Mode Support

Providers support thinking/reasoning modes where the underlying model
allows. The primary parameters per provider:

| Provider  | Thinking Parameter                | Notes                        |
|-----------|-----------------------------------|------------------------------|
| Anthropic | `thinking.budget_tokens`          | Extended thinking            |
| OpenAI    | `reasoning_effort` (low/med/high) | o1/o3 models only           |
| Google    | `thinking.budget_tokens`          | Gemini 2.5 Flash/Pro         |
| DeepSeek  | Auto-enabled for deepseek-reasoner| Budget via config           |
| Qwen      | `enable_thinking: true`           | QwQ models                   |
| Z.AI/GLM  | `preserve_across_turns`           | GLM-4 thinking mode          |

## Common Tasks

### Adding a New Provider
1. Add an adapter struct in `providers/` (e.g. a new `providers/foo.rs`)
2. Implement the `ProviderAdapter` trait (defined in `providers/mod.rs`)
3. Add thinking support via `transform_request_with_thinking()`
4. Register the singleton in `get_adapter()` and `SUPPORTED_PROVIDERS` in `providers/mod.rs`
5. Add default `base_url`/config defaults in `config/mod.rs`

### Adding a New Tool
1. Add the tool definition and execution in `tools/` (see `tools/registry.rs`)
2. Update the system prompt if needed
3. Add tests

### Modifying Hooks
1. Edit hook configuration in `config.yaml`
2. Hooks run in order: `pre_tool_use` → tool → `post_tool_use`
3. Stop hooks can halt execution when condition matches
