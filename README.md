# OpenClaudia

**Open-source universal agent harness** — Claude Code-like capabilities for any AI provider.

OpenClaudia is a Rust-based CLI that transforms any LLM into an agentic coding assistant with tools, memory, hooks, and multi-provider support.

![OpenClaudia Logo](images/logo.jpg)

## Features

- **Behavioral Modes** — Three-axis model (agency, quality, scope) with 8 presets and 6 modifiers for fine-grained control over AI behavior
- **Multi-Provider Support** — Anthropic, OpenAI, Google Gemini, DeepSeek, Qwen, Z.AI/GLM, Kimi/Moonshot, MiniMax, Ollama, and any OpenAI-compatible server
- **Local LLM Support** — Run with Ollama, LM Studio, LocalAI, or any OpenAI-compatible endpoint
- **Auto-Detect Provider** — Pass `-m gemini-2.5-flash` and the provider is detected automatically
- **30+ Agentic Tools** — Bash, file ops, LSP, web search, notebooks, task tracking, plan mode, worktrees, cron scheduling, MCP resources
- **Tool Execution Loop** — Multi-turn tool calling with automatic result feedback (works across all providers)
- **Web Search** — Browser-feature builds support no-key DuckDuckGo/Bing scraping; Tavily or Brave APIs work in all builds
- **Auto-Learning Memory** — Automatically captures coding patterns, error resolutions, file relationships, and user preferences across sessions
- **Background Shells** — Run long-running processes, check output, and kill them on demand
- **Thinking Mode** — Extended reasoning for Anthropic, OpenAI o1/o3, Gemini 2.5, DeepSeek R1, Qwen QwQ, GLM
- **VDD Adversarial Review** — Verification-Driven Development: a separate adversary model reviews code for bugs/vulnerabilities
- **Hooks System** — Run custom scripts at key moments (session start, tool use, prompt submit, etc.)
- **Guardrails** — Configurable code quality gates, blast radius limiting, and diff size monitoring
- **Plan Mode** — Toggle between Build and Plan modes; plan mode restricts destructive tools
- **Permissions** — Granular tool-level allow/deny rules with glob patterns
- **Task Management** — Built-in task tracking with dependencies and status workflow
- **LSP Integration** — Language Server Protocol support for go-to-definition, find-references, hover, and more
- **Subagent System** — Spawn autonomous agents from the agent loop; coordinator infrastructure is experimental and not wired into the default TUI yet
- **ACP Server** — Agent Control Protocol server for agent interoperability via stdin/stdout
- **Git Worktrees** — Create, manage, and switch between isolated git worktrees
- **Cron Scheduling** — Create, list, and delete recurring scheduled jobs
- **Skills System** — Load and invoke reusable prompt skills from markdown files
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

# Start with a behavioral mode
openclaudia --mode create     # Autonomous architect — build from scratch
openclaudia --mode safe       # Collaborative minimal — surgical precision
openclaudia --mode debug      # Investigation-first debugging
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
| `KIMI_API_KEY` or `MOONSHOT_API_KEY` | Kimi/Moonshot | For Kimi |
| `MINIMAX_API_KEY` | MiniMax | For MiniMax |
| `TAVILY_API_KEY` | Web search | Optional |
| `BRAVE_API_KEY` | Web search (alt) | Optional |

### Config File

Configuration is stored in `.openclaudia/config.yaml`:

```yaml
proxy:
  port: 8080
  host: "127.0.0.1"
  target: anthropic  # Provider: anthropic, openai, google, deepseek, qwen, zai, kimi, minimax, ollama, local

providers:
  anthropic:
    base_url: https://api.anthropic.com
    thinking:
      enabled: false
      budget_tokens: 10000        # Anthropic thinking budget
  openai:
    base_url: https://api.openai.com
    thinking:
      reasoning_effort: "medium"  # OpenAI o1/o3/o4: low, medium, high
  google:
    base_url: https://generativelanguage.googleapis.com
    thinking:
      budget_tokens: 10000        # Google Gemini 2.5 thinking budget
  zai:
    base_url: https://api.z.ai/api/coding/paas/v4
  deepseek:
    base_url: https://api.deepseek.com
  qwen:
    base_url: https://dashscope.aliyuncs.com/compatible-mode
  kimi:
    base_url: https://api.moonshot.ai/v1
  minimax:
    base_url: https://api.minimax.io/v1
  # Ollama for local LLM inference
  ollama:
    base_url: http://localhost:11434
  # Any OpenAI-compatible local server (LM Studio, LocalAI, etc.)
  local:
    base_url: http://localhost:1234/v1

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session
  max_turns: 0  # 0 = unlimited agentic loop iterations; set nonzero to cap tool loops

# Verification-Driven Development (VDD) - Adversarial code review
# vdd:
#   enabled: true
#   mode: advisory           # advisory (single pass) or blocking (loop until clean)
#   adversary:
#     provider: google       # Must differ from proxy.target
#     model: gemini-2.5-flash

# Granular tool permissions
# permissions:
#   enabled: true
#   default_allow:
#     - "git status"
#     - "src/**"
#   mcp:
#     filesystem: ["read_file", "list_directory"]

# Legacy line REPL keybindings (`openclaudia --tui-mode`)
keybindings:
  ctrl-x n: new_session
  ctrl-x x: export
  tab: toggle_mode
  escape: cancel
```

## CLI Commands

```bash
openclaudia                    # Start full-screen interactive TUI (default)
openclaudia -m <model>         # Use specific model (auto-detects provider)
openclaudia -v                 # Verbose logging
openclaudia --resume           # Resume last session
openclaudia --session-id <id>  # Resume specific session
openclaudia --coordinator --tui-mode  # Legacy REPL coordinator prompt mode
openclaudia --tui-mode         # Legacy line-oriented REPL
openclaudia --mode <preset>    # Start with a behavioral mode preset

openclaudia init               # Initialize config in current directory
openclaudia init --force       # Overwrite existing config

openclaudia auth               # Authenticate with Claude Max (OAuth)
openclaudia auth --status      # Check auth status
openclaudia auth --logout      # Clear stored credentials

openclaudia start              # Start as proxy server
openclaudia start -p 9090      # Custom port
openclaudia start -t openai    # Target specific provider

openclaudia acp                # Start ACP server on stdin/stdout
openclaudia acp -m <model>     # ACP with specific model

openclaudia loop               # Start iteration mode with Stop hooks
openclaudia loop -n 10         # Max 10 iterations

openclaudia config             # Show current configuration
openclaudia doctor             # Check connectivity and API keys
```

## Slash Commands (Default TUI)

The default full-screen TUI intentionally exposes a focused slash-command set. The legacy line-oriented REPL (`openclaudia --tui-mode`) has additional commands; type `/help` there for that registry.

### TUI Core

| Command | Description |
|---------|-------------|
| `/help`, `?` | Show the TUI help overlay |
| `/clear` | Clear the visible transcript |
| `/exit`, `/quit` | Exit the TUI |
| `/status` | Show model, provider, effort, and token estimate |
| `/mode` | Toggle between Build and Plan modes |
| `/effort [low\|medium\|high]` | Set or cycle effort level |

### TUI Sessions

| Command | Description |
|---------|-------------|
| `/sessions`, `/list` | List saved sessions |
| `/resume`, `/continue` | Open the session picker |
| `/load <id>`, `/continue <id>` | Resume a saved session by ID prefix |
| `/rename <title>` | Rename the current session |
| `/export` | Export conversation to markdown |
| `/undo` | Undo last message exchange |
| `/redo` | Redo last undone message exchange |
| `/rewind [N]` | Show turns or rewind the last N turns |

### TUI Diagnostics

| Command | Description |
|---------|-------------|
| `/cost` | Show session cost estimate |
| `/context` | Show context usage breakdown |
| `/files [dir]` | List files in the current or given directory |
| `/diff` | Show git diff summary |
| `/review` | Show a truncated git diff for review |
| `/doctor` | Run inline diagnostics |
| `/init` | Initialize project config if absent |

### TUI Skills

| Command | Description |
|---------|-------------|
| `/skill`, `/skills` | List available skills |
| `/skill <name>` | Invoke a skill as the next prompt |
| `/<skill-name>` | Invoke a skill by name |

### TUI Shell & Files

| Command | Description |
|---------|-------------|
| `!<command>` | Run shell command directly |
| `@<file>` | Attach file to prompt |

## Keyboard Shortcuts (Default TUI)

| Shortcut | Action |
|----------|--------|
| `Enter` | Send message |
| `Backspace`, `Delete` | Edit input |
| `Left`, `Right`, `Home`, `End` | Move input cursor |
| `Up`, `Down`, `PageUp`, `PageDown` | Scroll transcript |
| `Esc` | Close overlays, dismiss prompts, or cancel streaming |
| `Ctrl-C` | Cancel current turn or exit when idle |

The `keybindings:` config map customizes the legacy line-oriented REPL (`openclaudia --tui-mode`). The default full-screen TUI currently uses the shortcuts above.

## Available Tools

### Core Tools

| Tool | Description |
|------|-------------|
| `bash` | Execute shell commands with optional timeout and background mode |
| `bash_output` | Get output from background shells or list all running shells |
| `kill_shell` | Terminate a background shell by ID |
| `kill_shells_for_agent` | Terminate all background shells owned by an agent or session |
| `read_file` | Read file contents (supports images, PDFs, Jupyter notebooks) with optional offset/limit |
| `write_file` | Create or overwrite files |
| `edit_file` | Targeted string replacement edits (requires reading file first) |
| `list_files` | List directory contents |
| `glob` | Find files by glob pattern |
| `grep` | Search file contents by regex |
| `notebook_edit` | Edit Jupyter notebook cells (replace, insert, delete) |
| `web_fetch` | Fetch web pages as markdown |
| `web_search` | Search the web; browser builds include no-key DuckDuckGo/Bing scraping, and Tavily/Brave APIs work in all builds |
| `web_browser` | Full headless browser for JavaScript-heavy pages (default `browser` feature) |
| `crosslink` | Issue tracking and cross-session work memory via the embedded Crosslink library |

### Code Intelligence

| Tool | Description |
|------|-------------|
| `lsp` | Language Server Protocol operations (goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, call hierarchy) |

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
| `todo_write` | Simple to-do list (fallback when Crosslink issue tracking is unavailable) |
| `todo_read` | Read current to-do list |
| `skill` | Load a reusable prompt skill by name |
| `tool_search` | Fetch deferred tool schemas by name or keyword |

### Git Worktree Tools

| Tool | Description |
|------|-------------|
| `enter_worktree` | Create an isolated git worktree for parallel work |
| `exit_worktree` | Exit a worktree (keep or remove) |
| `list_worktrees` | List all active worktrees |

### Scheduling Tools

| Tool | Description |
|------|-------------|
| `cron_create` | Create a recurring scheduled job |
| `cron_delete` | Delete a scheduled job |
| `cron_list` | List all scheduled jobs |

### MCP Tools

| Tool | Description |
|------|-------------|
| `list_mcp_resources` | Browse resources from connected MCP servers |
| `read_mcp_resource` | Read a specific MCP resource by URI |

## Supported Models

### Anthropic
- `claude-opus-4-6`, `claude-sonnet-4-6` — Latest (2026)
- `claude-haiku-4-5-20251001` — Fast, near-frontier
- `claude-sonnet-4-5-20250929`, `claude-opus-4-5-20251101`, `claude-opus-4-1-20250805` — Legacy
- `claude-sonnet-4-20250514`, `claude-opus-4-20250514` — Legacy

### OpenAI
- `gpt-5.2`, `gpt-5.2-codex` — Latest (Dec 2025)
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

### Kimi
- `kimi-k2.7-code`, `kimi-k2.7-code-highspeed` — Coding-focused Kimi K2.7 models
- `kimi-k2.6`, `kimi-k2.5` — General Kimi K-series models
- `moonshot-v1-128k`, `moonshot-v1-32k`, `moonshot-v1-8k` — Moonshot V1 text models

### MiniMax
- `MiniMax-M3` — Latest M-series language model
- `MiniMax-M2.7`, `MiniMax-M2.7-highspeed` — M2.7 family
- `MiniMax-M2.5`, `MiniMax-M2.5-highspeed` — M2.5 family
- `MiniMax-M2.1`, `MiniMax-M2.1-highspeed` — M2.1 family
- `MiniMax-M2` — Earlier agentic reasoning model

### Ollama (Local)
- Popular: `llama3.1`, `deepseek-r1`, `gemma3`, `qwen3`, `mistral`, `phi4`, `llava`
- Any model installed — run `ollama list` to see available models

### OpenAI-Compatible (Local)
- Works with LM Studio, LocalAI, text-generation-webui, vLLM, and any OpenAI-compatible server
- Set `base_url` to your local server (e.g., `http://localhost:1234/v1`)

## Behavioral Modes

Control how the AI behaves with a three-axis model. Each axis is independent, and presets are named combinations for common workflows.

### The Axis Model

| Axis | Values | Controls |
|------|--------|----------|
| **Agency** | `autonomous`, `collaborative`, `surgical` | How much initiative the AI takes |
| **Quality** | `architect`, `pragmatic`, `minimal` | What code quality standard to target |
| **Scope** | `unrestricted`, `adjacent`, `narrow` | How far beyond the request to go |

### Presets

| Preset | Agency | Quality | Scope | Use when... |
|--------|--------|---------|-------|-------------|
| `create` | autonomous | architect | unrestricted | Building from scratch with proper structure |
| `extend` | autonomous | pragmatic | adjacent | Extending existing projects, improving as you go |
| `safe` | collaborative | minimal | narrow | Surgical changes to production code |
| `refactor` | autonomous | pragmatic | unrestricted | Moving files, consolidating modules |
| `explore` | collaborative | architect | narrow | Read-only code understanding (+ readonly modifier) |
| `debug` | collaborative | pragmatic | narrow | Investigation-first debugging (+ debug modifier) |
| `methodical` | surgical | architect | narrow | Step-by-step precision (+ methodical modifier) |
| `director` | collaborative | architect | unrestricted | Orchestrate subagents (+ director modifier) |

### Modifiers

Modifiers are behavioral overlays that stack on top of any preset:

| Modifier | Effect |
|----------|--------|
| `bold` | Confident, idiomatic code with no hedging or over-engineering |
| `debug` | Investigation-first: gather evidence, form hypotheses, trace data flow |
| `methodical` | Step-by-step precision, complete each step before the next |
| `director` | Orchestrate subagents, delegate implementation, verify results |
| `readonly` | No file modifications, explain what you would do instead |
| `context-pacing` | Pace work to context limits with clean pause points |

### Usage

```bash
# CLI flag
openclaudia --mode create
openclaudia --mode safe

# In-session switching
/mode                        # Show current mode and list presets
/mode create                 # Switch to create preset
/mode create +bold           # Create preset with bold modifier
/mode debug +context-pacing  # Debug with pacing
/mode safe +bold +readonly   # Stack multiple modifiers
```

The mode system integrates with Anthropic's prompt caching: behavioral axes and modifiers are part of the stable prompt prefix (cached across turns), while hooks, memory, and environment info are in the dynamic suffix (reprocessed each turn). Mode switches naturally invalidate the prefix cache.

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

Findings include CWE classifications, severity levels (CRITICAL/HIGH/MEDIUM/LOW/INFO), and can automatically create Crosslink issues for tracking.

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

## Auto-Learning Memory

OpenClaudia automatically learns from your coding sessions without any flags or model intervention. A SQLite database (`.openclaudia/memory.db`) captures knowledge from tool execution signals:

- **Coding Patterns** — Conventions, pitfalls, and architecture observed from lint output and edit failures
- **Error Resolutions** — Errors encountered and how they were fixed, matched automatically when subsequent commands succeed
- **File Relationships** — Files frequently edited together (co-edit tracking), surfaced when you touch related code
- **User Preferences** — Style and workflow preferences detected from corrections ("no, use tabs") and explicit statements ("always use snake_case")
- **Session Continuity** — Recent session summaries and activity logs for context across restarts

Knowledge is injected into the model's context automatically — file-specific patterns when you read/edit a file, and preferences in every system prompt. Use `/memory` commands to inspect what's been learned.

## Project Structure

```
.openclaudia/
├── config.yaml        # Main configuration
├── session/           # Persisted chat sessions
├── memory.db          # Auto-learning memory database
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

# Without browser feature (smaller binary; web_search requires Tavily or Brave API keys)
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
- **headless_chrome** — Headless browser fallback for web_fetch and no-key DuckDuckGo/Bing web search
- **scraper** — HTML parsing for search result extraction

## License

MIT License — See [LICENSE](LICENSE)

---

*Built with Rust. Powered by curiosity.*
