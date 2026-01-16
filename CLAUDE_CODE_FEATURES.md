# Claude Code Features Analysis

**Source:** `claude-code-unminified.js` (Version 2.1.5, 17.6 MB)
**Analysis Date:** 2026-01-15

This document catalogs features from Anthropic's Claude Code CLI for replication in OpenClaudia.

---

## 1. Core Tools

### File Operations
| Tool | Description | Priority |
|------|-------------|----------|
| **Read** | Read files with line numbers, supports images/PDFs/notebooks | ✅ Have |
| **Write** | Create/overwrite files | ✅ Have |
| **Edit** | String replacement edits with old_string/new_string | ✅ Have |
| **Glob** | File pattern matching (`**/*.rs`) | ✅ Have |
| **Grep** | Content search with ripgrep, supports regex | ✅ Have |
| **NotebookEdit** | Edit Jupyter notebook cells | ❌ Missing |

### System Operations
| Tool | Description | Priority |
|------|-------------|----------|
| **Bash** | Execute shell commands with timeout, supports `run_in_background` | ✅ Have |
| **BashOutput** | Retrieve output from background shells, lists all shells when no ID provided | ✅ Have |
| **KillShell** | Terminate background shells by ID | ✅ Have |

### Web Operations
| Tool | Description | Priority |
|------|-------------|----------|
| **WebFetch** | Fetch URL content, convert HTML to markdown | ✅ Have |
| **WebSearch** | Search the web for current information | ✅ Have (DDG/Tavily/Brave) |

---

## 2. Agent System

### Task Tool (Subagents)
Claude Code spawns specialized subagents for complex tasks:

| Agent Type | Purpose | Priority |
|------------|---------|----------|
| **general-purpose** | Complex multi-step tasks, code search | High |
| **Explore** | Fast codebase exploration, quick/medium/thorough modes | High |
| **Plan** | Software architect for implementation planning | Medium |
| **claude-code-guide** | Answer questions about Claude Code/SDK/API | Low |
| **statusline-setup** | Configure status line settings | Low |
| **test-runner** | Run and analyze tests | Medium |
| **code-reviewer** | Review code after writing | Medium |

### Agent Features
- **Background execution** - `run_in_background` parameter
- **AgentOutputTool** - Retrieve results from background agents
- **Resume capability** - Continue from previous execution transcript
- **Model selection** - sonnet/opus/haiku per agent

---

## 3. Planning & Task Management

### Plan Mode
- **EnterPlanMode** - Enter planning mode for complex tasks
- **ExitPlanMode** - Exit with approval request
- Uses dedicated plan file for writing implementation plans
- User must approve before implementation

### TodoWrite Tool
- Create/manage structured task lists
- Track progress with states: `pending`, `in_progress`, `completed`
- Both `content` (imperative) and `activeForm` (present continuous) forms
- Only one task `in_progress` at a time

### AskUserQuestion
- Ask clarifying questions during planning
- 11 references in codebase

---

## 4. MCP (Model Context Protocol)

**280+ references** in codebase - heavily integrated

### MCP Tools
| Tool | Description |
|------|-------------|
| **ListMcpResourcesTool** | List available MCP resources |
| **ReadMcpResourceTool** | Read specific MCP resource |
| MCP server management | stdio and HTTP transport |

### MCP Features
- Server discovery and connection
- Tool loading from MCP servers (`mcp__servername__toolname`)
- Resource browsing
- Logs in `.jsonl` format

---

## 5. Hooks System

**1111 hook references** - extensive lifecycle management

### Hook Types
| Hook | Count | Description |
|------|-------|-------------|
| **PreToolUse** | 30 | Before tool execution |
| **PostToolUse** | 50 | After tool execution |
| **Notification** | 208 | System notifications |
| **Stop** | 167 | Halt execution conditions |
| **session_end** | 12 | Session cleanup |
| **pre_tool** | 4 | Legacy pre-tool |
| **post_tool** | 4 | Legacy post-tool |

### Hook Configuration
- Defined in `.claude/settings.json`
- Can specify model for hook execution
- Supports agent hooks and prompt hooks

---

## 6. Permission & Sandbox System

**872 allow / 497 permission / 185 Sandbox references**

### Permission Features
- Tool-level permissions (Edit, Write, NotebookEdit)
- Approval workflow for sensitive operations
- Sandbox mode for restricted execution
- `dangerouslyDisableSandbox` parameter

### Patterns
- `Bash(git:*)` - Git command patterns
- `Bash(npm *)` - NPM command patterns
- `Edit(docs/**)` - File pattern permissions
- `Edit(~/.claude/settings.json)` - Specific file access

---

## 7. Thinking/Reasoning Mode

**172 thinking / 49 reasoning / 17 budget references**

### Features
- Extended thinking mode
- Budget tokens configuration
- Reasoning effort levels (low/medium/high)
- Cross-turn thinking preservation

---

## 8. Context Management

**942 context / 177 compact / 190 summarization / 151 truncate references**

### Features
- Automatic summarization when context fills
- Context window compaction
- Token counting and limits
- Message truncation strategies

---

## 9. Session Management

**1166 session / 187 resume / 128 conversation references**

### Features
- Session state persistence
- Resume from previous conversation
- Conversation history tracking
- `/resume` command

---

## 10. Git Integration

**1128 git / 224 commit / 130 branch / 289 diff references**

### Features
- Git status awareness
- Commit message generation
- PR creation with `gh` CLI
- Diff analysis
- Branch management
- Stash operations

---

## 11. IDE Integration

**475 IDE / 313 cursor / 31 vscode / 46 vim references**

### Supported Environments
- VSCode native extension
- Cursor IDE
- Terminal/CLI mode
- Vim mode (basic)
- Emacs (minimal)

### Features
- Selection context from IDE
- Clickable file references in markdown
- Headless browser support (6 references)

---

## 12. Slash Commands

### Built-in Commands
| Command | Description |
|---------|-------------|
| `/help` | Show help |
| `/clear` | Clear conversation |
| `/model` | Change model |
| `/config` | View/edit configuration |
| `/doctor` | Diagnostics |
| `/status` | Show status |
| `/resume` | Resume previous session |
| `/compact` | Compact context |
| `/init` | Initialize project |
| `/memory` | Memory operations |

### Custom Commands
- `.claude/commands/` directory
- Skill invocation via SlashCommand tool
- Command file parsing (`.md` files)

---

## 13. Configuration System

### Files
| File | Location | Purpose |
|------|----------|---------|
| `settings.json` | `~/.claude/` | User settings |
| `settings.json` | `.claude/` | Project settings |
| `settings.local.json` | `.claude/` | Local overrides |
| `managed-settings.json` | System | Enterprise management |
| `CLAUDE.MD` | Project root | Project instructions |
| `.clauderules` | Project | Additional rules |

### Features
- Environment variable expansion
- JSON schema validation
- Symlink support
- Enterprise managed settings

---

## 14. Skills System

**315 skill / 77 Skill references**

### Available Skills
- PDF processing
- XLSX/Excel files
- DOCX/Word documents
- Custom skill registration

---

## 15. Multimodal Support

**567 image / 107 pdf / 37 screenshot / 81 vision references**

### Features
- Image file reading (PNG, JPG, etc.)
- PDF page-by-page extraction
- Screenshot analysis
- Visual content processing

---

## 16. Usage & Billing

**207 usage / 72 cost / 21 quota / 17 billing references**

### Metrics Tracked
- CLI sessions started
- Lines of code modified (added/removed)
- Pull requests created
- Git commits created
- Token usage
- Session cost
- Active time

---

## 17. Authentication

**1254 Auth / 205 oauth / 575 credential references**

### OAuth Flow
- Device flow authentication
- PKCE support
- Session management
- Credential storage
- Token refresh

---

## Priority Features to Implement

### Critical (Missing)
1. **NotebookEdit** - Jupyter notebook editing
2. **AgentOutputTool** - Background agent result retrieval

### High Priority (Partial/Missing)
1. **Subagent System** - Task tool with specialized agents
2. **MCP Integration** - Full MCP server support
3. **Plan Mode** - EnterPlanMode/ExitPlanMode tools
4. **Enhanced Hooks** - PreToolUse/PostToolUse with model support

### Medium Priority
1. **Skills System** - PDF/XLSX/DOCX processing
2. **Usage Tracking** - Token/cost metrics
3. **Session Resume** - Continue previous conversations
4. **Context Compaction** - Automatic summarization

### Low Priority
1. **IDE Integration** - VSCode extension features
2. **Vim Mode** - Terminal vim keybindings
3. **Enterprise Settings** - Managed configuration
