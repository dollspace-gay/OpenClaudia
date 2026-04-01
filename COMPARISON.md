# Feature Comparison: OpenClaudia vs Claude Code vs OpenCode

Generated 2026-03-05

## Quick Summary

| Area | OpenClaudia | Claude Code | OpenCode |
|------|:-----------:|:-----------:|:--------:|
| **Overall maturity** | Alpha | Production | Production |
| **Provider support** | 7 + Ollama | Claude-only (3rd-party beta) | 75+ providers |
| **Tool count** | 30 | ~15 core | ~6 core |
| **Memory system** | Auto-learning SQLite | CLAUDE.md + auto-memory | Session SQLite |
| **Safety layers** | 4 (perms + guardrails + hooks + VDD) | 3 (perms + hooks + sandbox) | 1 (permissions) |
| **IDE integration** | None | VS Code, JetBrains, Cursor | VS Code, Cursor |
| **Platform reach** | Terminal only | Terminal, Desktop, Web, iOS, Slack, Chrome | Terminal, Desktop (beta), IDE |

---

## Detailed Feature Matrix

### Core Architecture

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Language | Rust | TypeScript/Node | Go |
| CLI interface | Yes | Yes | Yes |
| TUI (interactive terminal UI) | Yes (ratatui) | Yes | Yes (Bubble Tea) |
| HTTP proxy server mode | Yes | No | No |
| Headless / non-interactive mode | Partial | Yes (`-p` flag, `--output-format json`) | Yes (JSON output) |
| Web UI | No | Yes (claude.ai/code) | No |
| Desktop app | No | Yes (macOS, Windows) | Beta (macOS, Win, Linux) |
| IDE extensions | No | VS Code, Cursor, JetBrains | VS Code, Cursor |
| iOS / mobile | No | Yes (Claude iOS app) | No |
| Chrome extension | No | Yes | No |
| Slack integration | No | Yes (@Claude bot) | No |
| Remote Control (cross-device) | No | Yes (/teleport, /desktop) | No |

### Provider & Model Support

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Anthropic Claude | Yes | Yes (primary) | Yes |
| OpenAI / GPT | Yes | Beta (3rd-party) | Yes |
| Google Gemini | Yes (native) | No | Yes |
| DeepSeek | Yes | No | Yes (via Groq/OpenRouter) |
| Qwen | Yes | No | Yes (via OpenRouter) |
| Z.AI / GLM | Yes | No | No |
| Ollama (local) | Yes | No | Yes |
| AWS Bedrock | No | Yes | Yes |
| Azure OpenAI | No | No | Yes |
| Google Vertex AI | No | Yes | Yes |
| GitHub Copilot auth | No | No | Yes |
| OpenRouter | No | No | Yes |
| Groq | No | No | Yes |
| Provider auto-detection from model name | Yes | No | No |
| Mid-session model switching | Yes (`/model <name>`) | Yes | Yes |
| Total providers | 7 + Ollama | 1 (+3 cloud variants) | 75+ |

### Tools

| Tool | OpenClaudia | Claude Code | OpenCode |
|------|:-----------:|:-----------:|:--------:|
| Bash (shell execution) | Yes | Yes | Yes |
| Background shells (long-running) | Yes | Yes | No |
| Read file | Yes | Yes | Yes |
| Write file | Yes | Yes | Yes |
| Edit file (find-replace) | Yes | Yes | No (full write) |
| Glob (file search) | Yes | Yes | No |
| Grep (content search) | Yes | Yes | Yes |
| Web fetch | Yes | Yes | No |
| Web search | Yes (DDG/Tavily/Brave) | Yes | No |
| NotebookEdit (Jupyter) | Yes | Yes | No |
| AskUserQuestion | Yes | Yes | No |
| Plan mode (EnterPlanMode/ExitPlanMode) | Yes | Yes | No |
| Task management (create/update/list) | Yes | Yes | No |
| MCP tool calling | Yes | Yes | Yes |
| MCP resource browsing | Yes | Yes | No |
| Image/PDF reading | Yes (read_file) | Yes | No |
| LSP integration | Yes (goToDefinition, findReferences, hover, symbols) | No | Yes |

### Subagent / Multi-Agent System

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Subagent spawning | Yes | Full | No |
| Built-in agent types | General-purpose | Explore, Plan, General, Bash, Guide | N/A |
| Custom subagent definitions | No | Yes (markdown + YAML frontmatter) | No |
| Subagent model selection | Yes | Yes (sonnet/opus/haiku/inherit) | N/A |
| Worktree isolation | Yes (enter/exit/list) | Yes | No |
| Background agents | Yes (async tracking) | Yes (Ctrl+B) | No |
| Agent resume | Yes | Yes (by agent ID) | No |
| Per-agent permissions | No | Yes (permission modes) | No |
| Per-agent hooks | No | Yes | No |
| Per-agent MCP servers | No | Yes | No |
| Per-agent persistent memory | No | Yes (user/project/local scopes) | No |
| Agent teams (multi-session) | No | Yes | No |
| Max turns limit | No | Yes | No |
| Agent SDK (build custom agents) | No | Yes | No |
| `/agents` management UI | No | Yes | No |

### Memory & Context

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Session persistence | Yes (SQLite) | Yes | Yes (SQLite) |
| Instruction file (CLAUDE.md) | Yes (parsed) | Yes (core feature) | No |
| Auto-memory (cross-session learning) | Yes (4 learning channels) | Yes | No |
| Coding pattern capture | Yes | Partial | No |
| Error pattern → resolution tracking | Yes | No | No |
| File relationship / co-edit graph | Yes | No | No |
| User preference detection | Yes | No | No |
| Core memory (always-in-context) | Yes | Yes (CLAUDE.md) | No |
| Archival memory (FTS5 search) | Yes | No | No |
| Context compaction / auto-compact | Yes (85% threshold) | Yes (95% threshold) | Yes (95% threshold) |
| Token estimation | Yes | Yes | Yes |
| Multi-session management | No | Yes | Yes |

### Hooks & Lifecycle

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Hook system | Yes | Yes | No |
| PreToolUse | Yes | Yes | N/A |
| PostToolUse | Yes | Yes | N/A |
| PostToolUseFailure | Yes | No | N/A |
| UserPromptSubmit | Yes | Yes | N/A |
| SessionStart / SessionEnd | Yes | Yes | N/A |
| Stop | Yes | Yes | N/A |
| SubagentStart / SubagentStop | Yes | Yes | N/A |
| PreCompact | Yes | Yes | N/A |
| Notification | Yes | Yes | N/A |
| PermissionRequest | Yes | No | N/A |
| PreAdversaryReview / PostAdversaryReview | Yes | No | N/A |
| VddConflict / VddConverged | Yes | No | N/A |
| HTTP hooks (webhook endpoints) | No | Yes | N/A |
| Async hooks | No | Yes | N/A |
| Prompt hooks (LLM-evaluated) | No | Yes | N/A |
| MCP tool hooks | No | Yes | N/A |
| Hook matchers (regex/glob) | Yes (regex matchers) | Yes (regex matchers) | N/A |
| Total hook events | 16 | 10 | 0 |

### Permissions & Security

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Granular tool permissions | Yes | Yes | Yes |
| Allow/deny glob patterns | Yes | Yes (with specifiers) | Partial |
| Permission persistence | Yes (JSON file) | Yes | Session-only |
| Sandbox mode (filesystem/network) | No | Yes (full sandbox) | No |
| Network domain allowlist | No | Yes | No |
| Permission modes (acceptEdits, dontAsk, etc.) | No | Yes (5 modes) | No |
| Managed/enterprise settings | No | Yes (IT-deployed policies) | No |
| Settings scope layers | 2 (project, user) | 5 (managed, CLI, local, project, user) | 3 (home, XDG, local) |

### Safety & Quality

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| VDD adversarial review | Yes (unique) | No | No |
| Confabulation detection | Yes (unique) | No | No |
| CWE classification of findings | Yes (unique) | No | No |
| Guardrails engine | Yes | No | No |
| Blast radius limiting (path patterns) | Yes | Via sandbox | No |
| Diff monitoring (lines/files per turn) | Yes | No | No |
| Quality gates (auto-run linters) | Yes | No | No |
| Language-specific rules injection | Yes | Via CLAUDE.md | No |

### Git & Workflow

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Git commit creation | Via bash tool | Native (first-class) | Via bash tool |
| Git branch management | Via bash tool | Native | Via bash tool |
| PR creation | Via bash tool | Native (gh integration) | Via bash tool |
| GitHub Actions integration | No | Yes | No |
| GitLab CI/CD integration | No | Yes | No |
| Git worktrees | Yes (enter/exit/list tools) | Yes (for agents) | No |

### Custom Commands & Skills

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Custom slash commands | Yes (plugin commands) | Yes (/commands) | Yes (markdown files) |
| Skills (reusable prompts) | Yes (markdown + YAML) | Yes (with fork/agent context) | No |
| Plugin system | Yes (install, enable, disable, uninstall) | Yes (marketplace, install/uninstall) | No |
| Plugin marketplace | Yes (git, local dir sources) | Yes (GitHub, npm, URL, directory) | No |

### UI & Display

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| Markdown rendering | Yes | Yes | Yes |
| Thinking/reasoning display | Yes | Yes | No |
| Token cost display | Yes | Yes | No |
| Status bar | Yes | Yes (customizable) | No |
| Vim keybindings | Yes (`/vim` toggle) | No | Yes |
| Theme support | Yes (3 themes) | No | No |
| Streaming responses | Yes | Yes | Yes |
| Progress bar | No | Yes (terminal progress) | No |
| External editor integration | Yes (`/editor`, Ctrl+X E) | No | Yes |
| Inline diffs (IDE) | No | Yes (VS Code) | No |
| @-mentions (files) | No | Yes (VS Code) | No |

### Authentication

| Feature | OpenClaudia | Claude Code | OpenCode |
|---------|:-----------:|:-----------:|:--------:|
| API key auth | Yes | Yes | Yes |
| OAuth device flow | Yes (Claude Max) | Yes | No |
| Enterprise SSO / org login | No | Yes | No |
| GitHub Copilot auth passthrough | No | No | Yes |
| ChatGPT Plus/Pro auth | No | No | Yes |
| mTLS client certificates | No | Yes | No |

---

## Where OpenClaudia Leads

1. **Multi-provider with tool loop** — 7 native provider adapters with full tool execution loop across ALL providers. Claude Code only does tools with Claude; OpenCode supports many providers but has fewer tools.

2. **VDD adversarial review** — Unique feature. A separate adversary model reviews code output for bugs and vulnerabilities with CWE classification, severity scoring, and confabulation detection. Neither competitor has this.

3. **Guardrails engine** — Blast radius limiting, diff monitoring, and automated quality gates. Claude Code has sandbox but not the same granularity on diff/change monitoring.

4. **Auto-learning memory depth** — Four learning channels (coding patterns, error resolutions, file relationships, user preferences) with confidence scoring. Claude Code has auto-memory but it's less structured. OpenCode has no cross-session learning.

5. **Archival memory with FTS5** — Full-text searchable long-term memory. Neither competitor has this.

6. **Provider auto-detection** — Pass `-m gemini-2.5-flash` and the provider is auto-detected. Neither competitor does this.

7. **HTTP proxy mode** — Can act as a proxy server, translating between OpenAI-compatible format and native provider APIs. Unique architecture.

8. **Hook event coverage** — 16 lifecycle events including VDD-specific and adversary-specific hooks. Claude Code has 10 events.

9. **LSP integration** — Built-in Language Server Protocol client for goToDefinition, findReferences, hover, document/workspace symbols, and call hierarchy. Claude Code has no LSP support.

10. **Tool count** — 30 built-in tools covering shells, files, web, LSP, worktrees, scheduling, tasks, MCP, and planning. More tools than either competitor.

## Where OpenClaudia Falls Short

### Critical Gaps (blocking for serious adoption)

1. **No IDE integration** — No VS Code, JetBrains, or Cursor extension. Claude Code and OpenCode both have IDE support. This is table stakes.

2. **No desktop/web/mobile app** — Terminal-only. Claude Code runs everywhere (terminal, desktop, web, iOS, Slack, Chrome). OpenCode has a desktop beta.

3. **No sandbox mode** — Claude Code has a full filesystem + network sandbox. OpenClaudia has guardrails but no true process isolation.

### Significant Gaps

4. **Subagent system lacks depth** — Claude Code has per-agent permissions/hooks/MCP/memory, agent teams, and Agent SDK. OpenClaudia has spawning, model selection, background execution, resume, and worktree isolation, but not per-agent policies or SDK.

5. **No CI/CD integration** — Claude Code has first-class GitHub Actions and GitLab CI/CD support. OpenClaudia has nothing.

6. **No HTTP/async/prompt hooks** — Claude Code hooks support HTTP webhooks, async execution, LLM-evaluated prompts, and MCP tool hooks. OpenClaudia hooks are command-only.

7. **No enterprise/managed settings** — Claude Code has IT-deployed managed settings, org SSO, 5-layer settings hierarchy. OpenClaudia has 2 layers.

8. **Permission system is simpler** — Claude Code has 5 permission modes, tool-specific specifiers (`Bash(npm run *)`), and managed lockdown. OpenClaudia has basic allow/deny.

9. **No headless JSON output mode** — Claude Code and OpenCode both support clean JSON output for scripting. OpenClaudia's non-interactive mode is limited.

### Minor Gaps

10. **No session sharing** — OpenCode can generate shareable session links.
11. **No @-mentions or inline diffs** — Claude Code's IDE extensions support these.

---

## Strategic Assessment

**OpenClaudia's unique value proposition**: Multi-provider tool execution + VDD adversarial review + deep auto-learning memory. No other tool combines these three.

**Biggest risk**: The IDE integration gap. Developers increasingly work from IDE-embedded agents rather than standalone terminals. Without VS Code/JetBrains support, OpenClaudia competes for a shrinking audience.

**Closed since last review**: Custom slash commands, mid-session model switching, image/PDF reading, LSP integration, plugin marketplace, worktree isolation, vim keybindings, external editor, skills system.

**Remaining high-impact wins**:
1. Headless JSON output mode (low effort)
2. VS Code extension (high effort but high impact)
3. Sandbox mode / process isolation (high effort)
4. CI/CD integration (moderate effort)
5. HTTP/async hooks (moderate effort)

---

Sources:
- [Claude Code Overview](https://code.claude.com/docs/en/overview)
- [Claude Code Hooks Reference](https://code.claude.com/docs/en/hooks)
- [Claude Code Subagents](https://code.claude.com/docs/en/sub-agents)
- [Claude Code Settings](https://code.claude.com/docs/en/settings)
- [OpenCode GitHub](https://github.com/opencode-ai/opencode)
- [OpenCode Website](https://opencode.ai/)
- [2026 Guide to Coding CLI Tools](https://www.tembo.io/blog/coding-cli-tools-comparison)
- [OpenCode InfoQ Coverage](https://www.infoq.com/news/2026/02/opencode-coding-agent/)
