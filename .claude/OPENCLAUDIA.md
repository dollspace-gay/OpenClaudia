# OpenClaudia

*Open-source universal agent harness. Because if Claude can have nice things, so can everyone else.*

## Vision

Create an open-source alternative to Claude Code's intermediary layer - giving ANY AI agent the same capabilities that make Claude Code powerful. Works with Anthropic, OpenAI, Google, or local models.

- **Hook injection** - Run scripts on events and inject output into context
- **Tool orchestration** - Standardized tool interface across providers
- **Session persistence** - Context preservation across restarts
- **Behavioral guardrails** - Rules and constraints injected into every interaction

## The Problem

Claude Code works because Anthropic controls the intermediary layer between you and Claude. This layer:
1. Intercepts all communication
2. Runs hooks at key moments (session start, tool use, prompts)
3. Injects hook output directly into the model's context window
4. Enforces behavioral patterns through system prompts

Other agents (GPT-4, Gemini, local models, Cursor, Antigravity, etc.) don't have this. They're "raw" - whatever you type goes straight to the API with no intermediary processing.

## The Solution: API Proxy Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        YOUR IDE / TERMINAL                       │
│                   (VS Code, Cursor, Terminal, etc.)              │
└─────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│                          OPENCLAUDIA                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │   HTTP      │  │   Hook      │  │   Context   │              │
│  │   Proxy     │→ │   Engine    │→ │   Injector  │              │
│  │             │  │             │  │             │              │
│  │ localhost:  │  │ Runs Python │  │ Modifies    │              │
│  │ 8080        │  │ /Rust/etc   │  │ messages    │              │
│  └─────────────┘  │ scripts     │  │ before send │              │
│                   └─────────────┘  └─────────────┘              │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │   Session   │  │   Rules     │  │   Plugin    │              │
│  │   Manager   │  │   Engine    │  │   API       │              │
│  │             │  │             │  │             │              │
│  │ Persistence │  │ Loads .md   │  │ Optional    │              │
│  │ across      │  │ rules and   │  │ extensions  │              │
│  │ restarts    │  │ injects     │  │             │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
└─────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│                        ACTUAL AI PROVIDERS                       │
│     Anthropic API  │  OpenAI API  │  Google API  │  Local LLM   │
└─────────────────────────────────────────────────────────────────┘
```

## Core Components

### 1. HTTP Proxy Server
- Listens on `localhost:8080` (or configured port)
- Accepts requests in OpenAI-compatible format (de facto standard)
- Routes to actual provider based on config
- **This is where all the magic happens**

### 2. Hook Engine
Runs scripts at key moments, exactly like Claude Agent SDK:

| Event | Trigger | Example Use | SDK Support |
|-------|---------|-------------|-------------|
| `SessionStart` | First request after timeout | Load project context, rules | TS only |
| `SessionEnd` | Session terminates | Cleanup, save state | TS only |
| `PreToolUse` | Before tool execution | Security checks, validation | TS + Python |
| `PostToolUse` | After tool execution | Lint checks, stub detection | TS + Python |
| `PostToolUseFailure` | Tool execution failed | Error handling, retry logic | TS + Python |
| `UserPromptSubmit` | Every user message | Inject behavioral guards | TS + Python |
| `Stop` | Agent completes task | Final validation | TS + Python |
| `SubagentStart` | Subagent spawned | Track parallel work | TS + Python |
| `SubagentStop` | Subagent completes | Aggregate results | TS + Python |
| `PreCompact` | Before context compaction | Preserve critical info | TS + Python |
| `PermissionRequest` | Tool needs approval | Custom permission logic | TS only |
| `Notification` | System notifications | Logging, alerts | TS only |

> **Note:** Python SDK does not support `SessionStart`, `SessionEnd`, `Notification`, or `PermissionRequest` hooks due to setup limitations.

**Two Hook Types:**

1. **Prompt-Based Hooks** (Recommended) - LLM-driven decision making:
```json
{
  "type": "prompt",
  "prompt": "Evaluate if this tool use is appropriate: $TOOL_INPUT",
  "timeout": 30
}
```

2. **Command Hooks** - Deterministic bash/python scripts:
```json
{
  "type": "command",
  "command": "python3 ${CLAUDE_PLUGIN_ROOT}/hooks/validate.py",
  "timeout": 60
}
```

**Hook I/O Format:**
```json
// Input (stdin) - Common fields for all hooks
{
  "session_id": "abc123",
  "transcript_path": "/path/to/transcript.txt",
  "cwd": "/current/working/dir",
  "permission_mode": "ask|allow",
  "hook_event_name": "PreToolUse",
  // Event-specific fields:
  "tool_name": "Edit",           // PreToolUse/PostToolUse
  "tool_input": {"file_path": "src/main.rs"},
  "tool_result": "...",          // PostToolUse only
  "user_prompt": "...",          // UserPromptSubmit
  "reason": "..."                // Stop/SubagentStop
}

// Output (stdout) - Standard format
{
  "continue": true,              // false halts processing
  "suppressOutput": false,       // hide from transcript
  "systemMessage": "Message for Claude"
}

// Output for PreToolUse (controls execution)
{
  "hookSpecificOutput": {
    "permissionDecision": "allow|deny|ask",
    "updatedInput": {"field": "modified_value"}
  }
}

// Output for Stop hooks
{
  "decision": "approve|block",
  "reason": "Explanation"
}
```

**Exit Codes:**
- `0` - Success (stdout shown in transcript)
- `2` - Blocking error (stderr fed back to Claude)
- Other - Non-blocking error

**Environment Variables:**
- `$CLAUDE_PROJECT_DIR` - Project root path
- `$CLAUDE_PLUGIN_ROOT` - Plugin directory (use for portable paths)
- `$CLAUDE_ENV_FILE` - SessionStart only: persist env vars here
- `$CLAUDE_CODE_REMOTE` - Set if running in remote context

**Matchers:**
```json
"matcher": "Write"              // Exact match
"matcher": "Read|Write|Edit"    // Multiple tools
"matcher": "*"                  // Wildcard (all tools)
"matcher": "mcp__.*__delete.*"  // Regex patterns
```

### 3. Context Injector
Modifies API requests to include hook output:

```python
# Before injection
messages = [
    {"role": "user", "content": "Fix the bug in auth.rs"}
]

# After injection (hook output added)
messages = [
    {"role": "system", "content": "<session-context>...</session-context>"},
    {"role": "user", "content": "Fix the bug in auth.rs"},
    {"role": "system", "content": "<behavioral-guard>...</behavioral-guard>"}
]
```

### 4. Rules Engine
Loads `.md` files and injects them as system context:
- `rules/global.md` - Always injected
- `rules/rust.md` - When working on `.rs` files
- `rules/security.md` - For security-sensitive operations

### 5. Session Manager
Anthropic's approach to long-running agents uses a two-part architecture:

**Initializer Agent** (first session):
- Sets up environment with `init.sh` script
- Creates `claude-progress.txt` to log activities across sessions
- Makes initial git commit to record baseline state
- Generates comprehensive `features.json` from user's initial prompt

**Coding Agent** (subsequent sessions):
- Reads git logs and progress files at session start
- Runs basic end-to-end tests to verify state
- Selects next priority feature from feature list
- Works incrementally, leaving codebase in "clean state"
- Commits changes with descriptive messages
- Updates progress documentation before ending

Key insight: Agents are treated like shift-based engineers, using documented artifacts (git history, progress files, feature lists) to bridge contextual gaps between sessions.

### 6. Subagent Coordinator
Claude Agent SDK supports first-class subagents:
- Spawn specialized agents for focused subtasks
- Each subagent has isolated context window
- Messages include `parent_tool_use_id` for tracking
- Prevents recursive explosion (subagents can't spawn subagents)
- Results aggregated back to orchestrator

```python
# Define custom subagents
agents={
    "code-reviewer": AgentDefinition(
        description="Expert code reviewer",
        prompt="Analyze code quality and suggest improvements.",
        tools=["Read", "Glob", "Grep"]
    )
}
```

### 7. MCP Integration
Model Context Protocol connects to external systems:
- Databases, browsers, APIs
- Hundreds of community servers available
- Enables browser automation (Playwright MCP)
- Custom tool providers

```python
mcp_servers={
    "playwright": {"command": "npx", "args": ["@playwright/mcp@latest"]}
}
```

### 8. Tool Adapter
Translates tool definitions between providers:
- OpenAI function calling → Anthropic tool use
- Provider-specific quirks handled transparently

### 9. Plugin API
The harness exposes a plugin interface - it doesn't ship with opinionated plugins. Users bring their own.

**Plugin Interface Specification:**

```
plugin-name/
├── manifest.json            # Required: metadata + capabilities declared
├── hooks/                   # Optional: hook handlers
│   └── hooks.json
├── commands/                # Optional: slash commands
├── agents/                  # Optional: agent definitions
├── mcp.json                 # Optional: MCP server config
└── README.md
```

**Manifest Schema** (`manifest.json`):
```json
{
  "$schema": "https://harness.dev/plugin.schema.json",
  "name": "my-plugin",
  "version": "1.0.0",
  "capabilities": ["hooks", "commands", "agents", "mcp"],
  "hooks": "./hooks/hooks.json",
  "entrypoint": "./main.py"  // Optional: for complex plugins
}
```

**Hook Registration Format:**
```json
{
  "PreToolUse": [
    {
      "matcher": "Write|Edit",
      "hooks": [
        {"type": "command", "command": "${PLUGIN_ROOT}/validate.py"},
        {"type": "prompt", "prompt": "Validate safety..."}
      ]
    }
  ]
}
```

**Plugin Loading:**
1. Scan `~/.openclaudia/plugins/` and `.openclaudia/plugins/`
2. Validate manifests against schema
3. Register declared capabilities (hooks, commands, agents)
4. Expose `${PLUGIN_ROOT}` for portable paths

**Design Principles:**
- Plugins are optional - harness works without any
- All matching hooks run **in parallel** - design for independence
- Plugins can't break the core harness
- Users control which plugins load via config
- No bundled plugins - the harness is the platform, not the opinion

## Configuration

```yaml
# .openclaudia/config.yaml
proxy:
  port: 8080
  target: anthropic  # or openai, google, local

providers:
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
    base_url: https://api.anthropic.com
  openai:
    api_key: ${OPENAI_API_KEY}
    base_url: https://api.openai.com

hooks:
  SessionStart:
    - command: python hooks/session-start.py
      timeout: 30
  SessionEnd:
    - command: python hooks/session-end.py
  PreToolUse:
    - matcher: "Bash"
      command: python hooks/bash-security-check.py
  PostToolUse:
    - matcher: "Write|Edit"
      command: python hooks/post-edit-check.py
  PostToolUseFailure:
    - command: python hooks/handle-failure.py
  UserPromptSubmit:
    - command: python hooks/prompt-guard.py
  Stop:
    - command: python hooks/final-validation.py
  SubagentStart:
    - command: python hooks/subagent-start.py
  SubagentStop:
    - command: python hooks/subagent-stop.py
  PreCompact:
    - command: python hooks/pre-compact.py
  PermissionRequest:
    - matcher: "Bash|Write"
      command: python hooks/permission-check.py

mcp_servers:
  playwright:
    command: npx
    args: ["@playwright/mcp@latest"]
  filesystem:
    command: npx
    args: ["@anthropic/mcp-server-filesystem"]

subagents:
  code-reviewer:
    description: "Expert code reviewer for quality and security"
    prompt: "Analyze code quality and suggest improvements"
    tools: ["Read", "Glob", "Grep"]
  test-runner:
    description: "Runs and analyzes test results"
    prompt: "Execute tests and report failures with context"
    tools: ["Bash", "Read", "Glob"]

rules:
  - rules/global.md
  - rules/${language}.md  # Dynamic based on file type

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session/
```

## Usage

### Terminal
```bash
# Start OpenClaudia
openclaudia start

# Configure your AI client to use the proxy
export ANTHROPIC_API_KEY=your-key
export ANTHROPIC_BASE_URL=http://localhost:8080

# Now any tool that talks to Anthropic goes through OpenClaudia
claude-code  # Works normally, hooks injected
aider        # Works, hooks injected
cursor       # Works if you can set base URL
```

### VS Code Extension
```typescript
// Auto-detect environment and start OpenClaudia
if (!isClaudeCode) {
    openclaudia.start();
    // Modify API requests to go through localhost:8080
}
```

### Programmatic
```python
import openclaudia

with openclaudia.session() as session:
    response = session.chat("Fix the bug in auth.rs")
    # Hooks run automatically, context injected
```

## Project Structure

```
openclaudia/
├── Cargo.toml              # Rust for performance-critical proxy
├── src/
│   ├── main.rs             # CLI entry point
│   ├── proxy.rs            # HTTP proxy server
│   ├── hooks.rs            # Hook engine (all 12 event types)
│   ├── injector.rs         # Context injection
│   ├── session.rs          # Session management (initializer + coding agent pattern)
│   ├── subagents.rs        # Subagent coordination
│   ├── mcp.rs              # Model Context Protocol integration
│   ├── compaction.rs       # Context window management
│   ├── rules.rs            # Rules loading
│   └── providers/          # Provider adapters
│       ├── anthropic.rs
│       ├── openai.rs
│       ├── google.rs
│       └── local.rs
├── hooks/                  # Default hook scripts
│   ├── session-start.py
│   ├── session-end.py
│   ├── bash-security-check.py
│   ├── post-edit-check.py
│   ├── handle-failure.py
│   ├── prompt-guard.py
│   ├── final-validation.py
│   ├── subagent-start.py
│   ├── subagent-stop.py
│   ├── pre-compact.py
│   └── permission-check.py
├── rules/                  # Default rules
│   ├── global.md
│   ├── rust.md
│   ├── python.md
│   ├── typescript.md
│   └── security.md
├── mcp-servers/            # Bundled MCP server configs
│   ├── playwright.json
│   └── filesystem.json
├── subagents/              # Default subagent definitions
│   ├── code-reviewer.yaml
│   └── test-runner.yaml
├── vscode-extension/       # VS Code integration
│   ├── package.json
│   └── src/
│       └── extension.ts
└── docs/
    ├── getting-started.md
    ├── hooks.md
    ├── subagents.md
    ├── mcp.md
    └── providers.md
```

## Why Rust for the Proxy?

1. **Performance** - Minimal latency added to API calls
2. **Single binary** - No runtime dependencies
3. **Cross-platform** - Windows, macOS, Linux from one codebase
4. **Memory safety** - Critical for a security-sensitive proxy
5. **Async** - Handle many concurrent connections efficiently

## Migration Path from Chainlink

The existing chainlink hooks can be reused directly:
- `.claude/hooks/*.py` → `openclaudia/hooks/*.py`
- `.claude/settings.json` → `.openclaudia/config.yaml`
- `.chainlink/rules/*.md` → `openclaudia/rules/*.md`

OpenClaudia is the "brain" that chainlink was missing - the intermediary layer that forces context injection.

## Advanced Patterns (Learned from Claude Code)

These are techniques OpenClaudia should support - not dependencies on Claude Code plugins.

### Iteration Loops via Stop Hooks
Use Stop hooks to create self-referential feedback loops:

```bash
# Hook blocks exit and re-injects the same prompt
# Agent sees previous work in files/git and iterates
openclaudia loop "Build a REST API. Output COMPLETE when done." --max-iterations 50
```

The Stop hook intercepts Claude's exit attempts, feeds the same prompt back, and Claude sees its previous work in files/git. This creates autonomous iteration until completion.

**Real results:** $50k contract completed for $297 in API costs using this approach.

### Prompt-Based Hooks for Complex Logic
Use LLM-driven decision making instead of regex:

```json
{
  "type": "prompt",
  "prompt": "Analyze this edit for: syntax errors, security vulnerabilities, breaking changes. Return 'approve' or 'deny' with reason."
}
```

Supported for: `PreToolUse`, `Stop`, `SubagentStop`, `UserPromptSubmit`

### Context Loading at Session Start
Persist environment variables across the session:

```bash
#!/bin/bash
# hooks/session-start.sh
echo "export PROJECT_TYPE=nodejs" >> "$CLAUDE_ENV_FILE"
echo "export TEST_FRAMEWORK=jest" >> "$CLAUDE_ENV_FILE"
```

### Capabilities to Implement (from Claude Code 2.1.x)
Features worth replicating in the harness:
- **Prompt-based hooks** - LLM-driven decision making (not just regex/scripts)
- **Hook middleware** - `updatedInput` lets hooks modify tool input before execution
- **Scoped hooks** - Hooks can be attached to specific agents/commands
- **Wildcard permissions** - Pattern matching for tool permissions (`npm *`, `git * main`)
- **Hot-reload** - Changes to config/hooks take effect without restart
- **`once: true`** - Run hook only once per session
- **Long timeouts** - Up to 10 minutes for complex hooks

## 2026 Industry Context

The industry has converged on "Agent Harness" as the key differentiator:

> "2025 was agents. 2026 is agent harnesses." — Industry consensus

Key trends shaping this space:

1. **Harness > Framework**: Frameworks provide building blocks; harnesses provide the complete operational layer (prompt presets, lifecycle hooks, tool handling, sub-agent management)

2. **Long-running agent problem**: Context windows are limited; complex projects span multiple sessions. The harness bridges sessions via documented artifacts (git history, progress files, feature lists)

3. **Model drift detection**: Harnesses will become the primary tool for detecting when models stop following instructions after many steps. This data feeds back into training

4. **Convergence of training and inference**: The harness environment becomes the training environment

5. **Don't over-engineer control flow**: "If you over-engineer the control flow, the next model update will break your system"

## Success Criteria

A developer should be able to:
1. `cargo install openclaudia`
2. `openclaudia init` (creates config + default structure)
3. `openclaudia start`
4. Point any AI tool at `localhost:8080`
5. Get the same hook injection that Claude Agent SDK provides
6. Connect MCP servers for external integrations
7. Have sessions persist across context window boundaries

**The model doesn't know it's being harnessed. It just sees the injected context as part of the conversation.**

## References

### Architecture & Design
- [Claude Code GitHub Repository](https://github.com/anthropics/claude-code) - Reference implementation, plugin API examples
- [Claude Agent SDK Overview](https://platform.claude.com/docs/en/agent-sdk/overview)
- [Effective Harnesses for Long-Running Agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [Claude Code Hooks Reference](https://code.claude.com/docs/en/hooks) - Hook I/O format, exit codes
- [Agent SDK Hooks Documentation](https://platform.claude.com/docs/en/agent-sdk/hooks)

### Patterns & Techniques
- [Claude Code System Prompts](https://github.com/Piebald-AI/claude-code-system-prompts) - System prompt structure, tool descriptions
- [Ralph Wiggum Technique](https://ghuntley.com/ralph/) - Stop-hook iteration loops
