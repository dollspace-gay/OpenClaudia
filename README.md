# OpenClaudia

**Open-source universal agent harness** — Claude Code like capabilities for any AI provider.

OpenClaudia is a Rust-based CLI that transforms any LLM into an agentic coding assistant with tools, memory, hooks, and multi-provider support.

![OpenClaudia Logo](images/logo.jpg)

## Features

- **Multi-Provider Support** — Anthropic, OpenAI, Google, DeepSeek, Qwen, Z.AI, Ollama, and any OpenAI-compatible server
- **Local LLM Support** — Run with Ollama, LM Studio, LocalAI, or any OpenAI-compatible endpoint
- **Agentic Tools** — Bash execution, file operations, web search, background shells, task tracking
- **Web Search** — DuckDuckGo (free, no API key), Tavily, or Brave APIs
- **Stateful Memory** — Letta/MemGPT-style archival memory that persists across sessions
- **Background Shells** — Run long-running processes, check output, and kill them on demand
- **Thinking Mode** — Extended reasoning support for Anthropic, OpenAI o1/o3, DeepSeek R1, Qwen QwQ, GLM
- **Hooks System** — Run custom scripts at key moments (session start, tool use, etc.)
- **Cross-Platform** — Windows, macOS, Linux with Git Bash for consistent shell behavior
- **Interactive TUI** — Rich terminal interface with keybindings and session management

## Prerequisites

### Required

- **Rust** — Install via [rustup](https://rustup.rs/)
- **Git Bash** (Windows only) — Comes with [Git for Windows](https://git-scm.com/download/win)
  - OpenClaudia uses Git Bash on Windows for Unix command compatibility
  - Ensure Git is in your PATH

### Optional

- **Chainlink** — Task tracking CLI for issue management
  - Install from: https://github.com/dollspace-gay/chainlink
  - Claudia uses this to track her work automatically

## Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/openclaudia.git
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
# or: export DEEPSEEK_API_KEY="your-key-here"

# Initialize configuration in your project
openclaudia init

# Start chatting
openclaudia
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
  budget_tokens: 10000        # Anthropic, Google
  reasoning_effort: "medium"  # OpenAI o1/o3: low, medium, high

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session

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
openclaudia --model <name>     # Use specific model
openclaudia --stateful         # Enable persistent memory
openclaudia -v                 # Verbose logging

openclaudia init               # Initialize config in current directory
openclaudia init --force       # Overwrite existing config

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

Claudia has access to these tools:

| Tool | Description |
|------|-------------|
| `bash` | Execute shell commands with optional timeout and background mode |
| `bash_output` | Get output from background shells or list all running shells |
| `kill_shell` | Terminate a background shell by ID |
| `read_file` | Read file contents with optional offset/limit for large files |
| `write_file` | Create/overwrite files |
| `edit_file` | Make targeted edits with string replacement |
| `list_files` | List directory contents with glob patterns |
| `web_fetch` | Fetch web pages as markdown (via Jina Reader) |
| `web_search` | Search the web (DuckDuckGo free, or Tavily/Brave APIs) |
| `chainlink` | Task and issue tracking |

### Memory Tools (Stateful Mode)

| Tool | Description |
|------|-------------|
| `memory_save` | Save information to archival memory |
| `memory_search` | Search archival memory |
| `memory_update` | Update existing memory entry |
| `core_memory_update` | Update persona/project/preferences |

## Supported Models

### Anthropic
- `claude-sonnet-4-20250514`
- `claude-opus-4-20250514`
- `claude-3-5-sonnet-20241022`
- `claude-3-5-haiku-20241022`
- `claude-3-opus-20240229`

### OpenAI
- `gpt-4`, `gpt-4-turbo`, `gpt-4o`, `gpt-4o-mini`
- `gpt-3.5-turbo`
- `o1-preview`, `o1-mini`

### Google
- `gemini-pro`, `gemini-1.5-pro`, `gemini-1.5-flash`
- `gemini-2.0-flash-exp`

### DeepSeek
- `deepseek-chat`, `deepseek-coder`, `deepseek-reasoner`

### Qwen
- `qwen-turbo`, `qwen-plus`, `qwen-max`, `qwen-long`

### Z.AI (GLM)
- `glm-4.7`, `glm-4-plus`, `glm-4-air`, `glm-4-flash`

### Ollama (Local)
- Any model installed: `llama3`, `codellama`, `mistral`, `mixtral`, `phi`, `gemma`, etc.
- Run `ollama list` to see available models
- Install models with `ollama pull <model-name>`

### OpenAI-Compatible (Local)
- Works with LM Studio, LocalAI, text-generation-webui, vLLM, and any OpenAI-compatible server
- Set `base_url` to your local server (e.g., `http://localhost:1234/v1`)

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
- `pre_tool_use` — Before executing a tool
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

Claudia can save and recall information across sessions using memory tools.

## Project Structure

```
.openclaudia/
├── config.yaml        # Main configuration
├── session/           # Persisted chat sessions
├── memory.db          # Stateful memory database
├── hooks/             # Custom hook scripts
├── rules/             # Language-specific rules (*.md)
└── plugins/           # Plugin manifests
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
- **rustyline** — Line editing
- **crossterm** — Terminal manipulation
- **serde** — Serialization

Default features (can be disabled with `--no-default-features`):
- **headless_chrome** — Headless browser for DuckDuckGo web search
- **scraper** — HTML parsing for search result extraction

## License

MIT License — See [LICENSE](LICENSE)

---

*Built with Rust. Powered by curiosity.*
