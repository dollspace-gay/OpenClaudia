# OpenClaudia

**Open-source universal agent harness** — Claude Code-like capabilities for any AI provider.

OpenClaudia is a Rust-based CLI that transforms any LLM into an agentic coding assistant with tools, memory, hooks, and multi-provider support.

![OpenClaudia Logo](images/logo.jpg)

## Features

- **Multi-Provider Support** — Anthropic, OpenAI, Google Gemini, DeepSeek, Qwen, Z.AI/GLM, Ollama, and any OpenAI-compatible server
- **Local LLM Support** — Run with Ollama, LM Studio, LocalAI, or any OpenAI-compatible endpoint
- **Auto-Detect Provider** — Pass `-m gemini-2.5-flash` and the provider is detected automatically
- **28 Agentic Tools** — Bash, file ops, web search, notebooks, task tracking, plan mode, MCP resources
- **Tool Execution Loop** — Multi-turn tool calling with automatic result feedback (works across all providers)
- **Web Search** — DuckDuckGo (free, no API key), Tavily, or Brave APIs
- **Stateful Memory** — Letta/MemGPT-style archival + core memory that persists across sessions
- **Background Shells** — Run long-running processes, check output, and kill them on demand
- **Thinking Mode** — Extended reasoning for Anthropic, OpenAI o1/o3, Gemini 2.5, DeepSeek R1, Qwen QwQ, GLM
- **VDD Adversarial Review** — Verification-Driven Development: a separate adversary model reviews code for bugs/vulnerabilities
- **Hooks System** — Run custom scripts at key moments (session start, tool use, prompt submit, etc.)
- **Guardrails** — Configurable code quality gates, blast radius limiting, and diff size monitoring
- **Plan Mode** — Toggle between Build and Plan modes; plan mode restricts destructive tools
- **Permissions** — Granular tool-level allow/deny rules with glob patterns
- **Task Management** — Built-in task tracking with dependencies and status workflow
- **Cross-Platform** — Windows, macOS, Linux with Git Bash for consistent shell behavior
- **Interactive TUI** — Rich terminal interface with keybindings, themes, and session management
- **Context Compaction** — Automatic summarization when conversations get long
- **Notebook Support** — Read and edit Jupyter notebooks
- **MCP Integration** — Browse and read resources from MCP servers
- **Plugin System** — Install, manage, and extend with plugins (commands, hooks, MCP servers)
- **OAuth Support** — Use your Claude Max subscription via built-in OAuth proxy

## Prerequisites

### Required

- **Rust** — Install via [rustup](https://rustup.rs/)
- **Git Bash** (Windows only) — Comes with [Git for Windows](https://git-scm.com/download/win)
  - OpenClaudia uses Git Bash on Windows for Unix command compatibility
  - Ensure Git is in your PATH

### Optional

- **Chainlink** — Task tracking CLI for issue management
  - Install from: https://github.com/dollspace-gay/chainlink

## Installation

```bash
# Clone the repository
git clone https://github.com/dollspace-gay/openclaudia.git
cd openclaudia

# Build release version (includes browser/web search support by default)
cargo build --release

# Build without browser feature (lighter binary, no headless Chrome)
cargo build --release --no-default-features

# The binary is at target/release/openclaudia
```

## Quick Start

```bash
# Set your API key (choose your provider)
export ANTHROPIC_API_KEY="your-key-here"
# or: export OPENAI_API_KEY="your-key-here"
# or: export GOOGLE_API_KEY="your-key-here"
# or: export DEEPSEEK_API_KEY="your-key-here"

# Initialize configuration in your project
openclaudia init

# Start chatting (uses default provider from config)
openclaudia

# Use a specific model (provider auto-detected from model name)
openclaudia -m gemini-2.5-flash
openclaudia -m gpt-4o
openclaudia -m claude-sonnet-4-20250514
```

## Configuration

### Environment Variables

| Variable | Provider | Required |
|----------|----------|----------|
| `ANTHROPIC_API_KEY` | Anthropic (Claude) | For Anthropic |
| `OPENAI_API_KEY` | OpenAI (GPT) | For OpenAI |
| `GOOGLE_API_KEY` | Google (Gemini) | For Google |
| `DEEPSEEK_API_KEY` | DeepSeek | For DeepSeek |
| `QWEN_API_KEY` | Qwen/Alibaba | For Qwen |
| `ZAI_API_KEY` | Z.AI (GLM) | For Z.AI |
| `TAVILY_API_KEY` | Web search | Optional |
| `BRAVE_API_KEY` | Web search (alt) | Optional |

### Config File

Configuration is stored in `.openclaudia/config.yaml`:

```yaml
proxy:
  port: 8080
  host: "127.0.0.1"
  target: anthropic  # Provider: anthropic, openai, google, deepseek, qwen, zai, ollama, local

providers:
  anthropic:
    base_url: https://api.anthropic.com
  openai:
    base_url: https://api.openai.com
  google:
    base_url: https://generativelanguage.googleapis.com
  deepseek:
    base_url: https://api.deepseek.com
  # Ollama for local LLM inference
  ollama:
    base_url: http://localhost:11434
  # Any OpenAI-compatible local server (LM Studio, LocalAI, etc.)
  local:
    base_url: http://localhost:1234/v1

# Thinking/reasoning mode configuration
thinking:
  enabled: false
  budget_tokens: 10000        # Anthropic, Google Gemini 2.5
  reasoning_effort: "medium"  # OpenAI o1/o3: low, medium, high

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session
  max_turns: 25  # 0 = unlimited agentic loop iterations

# Verification-Driven Development (VDD) - Adversarial code review
# vdd:
#   enabled: true
#   mode: advisory           # advisory (single pass) or blocking (loop until clean)
#   adversary:
#     provider: google       # Must differ from proxy.target
#     model: gemini-2.5-flash

# Granular tool permissions
# permissions:
#   denied_tools: ["bash"]
#   denied_commands: ["rm -rf /"]

# Customize keybindings
keybindings:
  ctrl-x n: new_session
  ctrl-x x: export
  tab: toggle_mode
  escape: cancel
```

## CLI Commands

```bash
openclaudia                    # Start interactive chat (default)
openclaudia -m <model>         # Use specific model (auto-detects provider)
openclaudia --stateful         # Enable persistent memory
openclaudia -v                 # Verbose logging

openclaudia init               # Initialize config in current directory
openclaudia init --force       # Overwrite existing config

openclaudia auth               # Authenticate with Claude Max (OAuth)
openclaudia auth --status      # Check auth status

openclaudia start              # Start as proxy server
openclaudia start -p 9090      # Custom port
openclaudia start -t openai    # Target specific provider

openclaudia config             # Show current configuration
openclaudia doctor             # Check connectivity and API keys
```

## Slash Commands (In Chat)

| Command | Description |
|---------|-------------|
| `/help`, `/?` | Show help message |
| `/new`, `/clear` | Start new conversation |
| `/sessions` | List saved sessions |
| `/session <id>` | Load a saved session |
| `/export` | Export conversation to markdown |
| `/compact` | Summarize old messages to save context |
| `/undo` | Undo last message exchange |
| `/redo` | Redo last undone exchange |
| `/exit`, `/quit` | Exit the chat |
| `/model` | Show current model |
| `/models` | List available models |
| `/model <name>` | Switch to different model |
| `/status` | Show session status |
| `/rename <title>` | Rename current session |
| `/keys` | Show keybindings |
| `/theme` | Switch color theme |
| `/editor` | Open external editor for long messages |
| `/review` | Review uncommitted changes |
| `/debug` | Show internal state |
| `/history` | Show all messages in current session |
| `!<command>` | Run shell command directly |
| `@<file>` | Attach file to prompt |

### Memory Commands (Stateful Mode)

| Command | Description |
|---------|-------------|
| `/memory` | Show memory stats |
| `/memory list` | List recent memories |
| `/memory search <query>` | Search memories |
| `/memory show <id>` | Show memory by ID |

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl-X N` | New session |
| `Ctrl-X L` | List sessions |
| `Ctrl-X X` | Export conversation |
| `Ctrl-X Y` | Copy last response |
| `Ctrl-X E` | Open external editor |
| `Ctrl-X M` | Show models |
| `Ctrl-X S` | Show status |
| `Ctrl-X H` | Show help |
| `Tab` | Toggle Build/Plan mode |
| `Escape` | Cancel current response |
| `F2` | Show models |

## Available Tools

### Core Tools

| Tool | Description |
|------|-------------|
| `bash` | Execute shell commands with optional timeout and background mode |
| `bash_output` | Get output from background shells or list all running shells |
| `kill_shell` | Terminate a background shell by ID |
| `read_file` | Read file contents (supports images, PDFs, Jupyter notebooks) with optional offset/limit |
| `write_file` | Create or overwrite files |
| `edit_file` | Targeted string replacement edits (requires reading file first) |
| `list_files` | List directory contents with glob patterns |
| `notebook_edit` | Edit Jupyter notebook cells (replace, insert, delete) |
| `web_fetch` | Fetch web pages as markdown |
| `web_search` | Search the web (DuckDuckGo free, or Tavily/Brave APIs) |
| `web_browser` | Full headless browser for JavaScript-heavy pages |
| `chainlink` | Issue and task tracking via Chainlink CLI |

### Planning and Task Tools

| Tool | Description |
|------|-------------|
| `ask_user_question` | Prompt the user for clarification with multiple-choice options |
| `enter_plan_mode` | Switch to plan mode (restricts destructive tools) |
| `exit_plan_mode` | Exit plan mode and proceed with implementation |
| `task_create` | Create a tracked task with subject, description, and active form |
| `task_update` | Update task status (pending/in_progress/completed), add dependencies |
| `task_get` | Get full details of a task by ID |
| `task_list` | List all tasks with status summary |
| `todo_write` | Simple to-do list (fallback when Chainlink unavailable) |
| `todo_read` | Read current to-do list |

### MCP Tools

| Tool | Description |
|------|-------------|
| `list_mcp_resources` | Browse resources from connected MCP servers |
| `read_mcp_resource` | Read a specific MCP resource by URI |

### Memory Tools (Stateful Mode)

| Tool | Description |
|------|-------------|
| `memory_save` | Save information to archival memory with tags |
| `memory_search` | Search archival memory by query |
| `memory_update` | Update an existing memory entry |
| `core_memory_update` | Update persona, project info, or user preferences |

## Supported Models

### Anthropic
- `claude-opus-4-6`, `claude-sonnet-4-6` — Latest (2026)
- `claude-haiku-4-5-20251001` — Fast, near-frontier
- `claude-sonnet-4-5-20250929`, `claude-opus-4-5-20251101`, `claude-opus-4-1-20250805` — Legacy
- `claude-sonnet-4-20250514`, `claude-opus-4-20250514` — Legacy

### OpenAI
- `gpt-5.2`, `gpt-5.2-codex`, `gpt-5.2-pro` — Latest (Dec 2025)
- `gpt-5`, `gpt-5-mini`, `gpt-5-nano` — GPT-5 family (Aug 2025)
- `gpt-4.1`, `gpt-4.1-mini`, `gpt-4.1-nano` — Non-reasoning, 1M context
- `o3`, `o4-mini` — Reasoning models
- `gpt-4o`, `gpt-4o-mini` — Legacy

### Google Gemini
- `gemini-3.1-pro-preview`, `gemini-3-flash-preview` — Latest (2026)
- `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.5-flash-lite` — Stable GA

### DeepSeek
- `deepseek-chat` — V3.2, general (non-thinking)
- `deepseek-reasoner` — V3.2, reasoning (thinking mode)

### Qwen
- `qwen3.5-plus`, `qwen3-max` — Latest (2026)
- `qwen-plus`, `qwen-turbo` — General
- `qwq-plus` — Reasoning
- `qwen3-coder-plus` — Coding specialist

### Z.AI (GLM)
- `glm-5` — Flagship (Feb 2026), 745B MoE
- `glm-4.7`, `glm-4.7-flash` — Coding/reasoning
- `glm-4.6`, `glm-4.5-flash` — Previous gen

### Ollama (Local)
- Popular: `llama3.1`, `deepseek-r1`, `gemma3`, `qwen3`, `mistral`, `phi4`, `llava`
- Any model installed — run `ollama list` to see available models

### OpenAI-Compatible (Local)
- Works with LM Studio, LocalAI, text-generation-webui, vLLM, and any OpenAI-compatible server
- Set `base_url` to your local server (e.g., `http://localhost:1234/v1`)

## Verification-Driven Development (VDD)

OpenClaudia includes a built-in adversarial code review system. When enabled, a separate AI model (the "adversary") reviews every response for bugs, security vulnerabilities, and logic errors.

```yaml
vdd:
  enabled: true
  mode: advisory        # Single-pass review, findings injected as context
  adversary:
    provider: google    # Use a different provider than your builder
    model: gemini-2.5-flash
  static_analysis:
    auto_detect: true   # Automatically runs cargo clippy, cargo test, etc.
```

**Two modes:**
- **Advisory** — Single adversary pass after each response. Findings are displayed and injected into context for the next turn.
- **Blocking** — Full adversarial loop. The builder must revise until the adversary's findings converge to false positives (confabulation threshold).

Findings include CWE classifications, severity levels (CRITICAL/HIGH/MEDIUM/LOW/INFO), and can automatically create Chainlink issues for tracking.

## Hooks

Configure hooks in `.openclaudia/config.yaml` to run scripts at key moments:

```yaml
hooks:
  session_start:
    - hooks:
        - type: command
          command: python .openclaudia/hooks/session-start.py
          timeout: 30

  user_prompt_submit:
    - hooks:
        - type: command
          command: python .openclaudia/hooks/prompt-guard.py

  pre_tool_use:
    - matcher: "Write|Edit"
      hooks:
        - type: command
          command: python .openclaudia/hooks/validate-write.py
```

### Hook Events

- `session_start` — When a session begins
- `session_end` — When a session ends
- `user_prompt_submit` — Before processing user input
- `pre_tool_use` — Before executing a tool (with matcher for specific tools)
- `post_tool_use` — After executing a tool
- `stop` — For iteration/loop mode control

## Stateful Mode

Enable persistent memory with `--stateful`:

```bash
openclaudia --stateful
```

This creates a SQLite database in `.openclaudia/memory.db` with:

- **Archival Memory** — Long-term storage for facts, decisions, patterns
- **Core Memory** — Always-present context (persona, project info, user preferences)
- **Short-term Memory** — Session summaries and recent activity (always active, even without `--stateful`)

The agent can save and recall information across sessions using memory tools.

## Project Structure

```
.openclaudia/
├── config.yaml        # Main configuration
├── session/           # Persisted chat sessions
├── memory.db          # Stateful memory database
├── hooks/             # Custom hook scripts
├── rules/             # Language-specific rules (*.md)
├── plugins/           # Plugin manifests
├── logs/              # Audit logs
└── vdd/               # VDD session logs (if tracking enabled)
```

## Building from Source

```bash
# Development build (includes browser feature by default)
cargo build

# Release build
cargo build --release

# Without browser feature (smaller binary, no headless Chrome)
cargo build --release --no-default-features

# Run all tests
cargo test

# Run integration tests (tests real tool execution)
cargo test --test integration_tests

# Lint
cargo clippy -- -D warnings

# Run with verbose logging
RUST_LOG=debug cargo run
```

## Dependencies

OpenClaudia is built with:

- **tokio** — Async runtime
- **axum** — HTTP server (for proxy mode)
- **reqwest** — HTTP client
- **rusqlite** — SQLite for memory
- **ratatui** — Terminal UI
- **rustyline** — Line editing with history
- **crossterm** — Terminal manipulation
- **serde** — Serialization
- **clap** — CLI argument parsing
- **tracing** — Structured logging

Default features (can be disabled with `--no-default-features`):
- **headless_chrome** — Headless browser for DuckDuckGo web search
- **scraper** — HTML parsing for search result extraction

## License

MIT License — See [LICENSE](LICENSE)

---

*Built with Rust. Powered by curiosity.*
