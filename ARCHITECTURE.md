# OpenClaudia Architecture

## High-Level Overview

```
                         ┌──────────────────────────────────┐
                         │           User / Client          │
                         └──────────┬───────────┬───────────┘
                                    │           │
                            CLI Chat│           │ HTTP API
                                    │           │
                         ┌──────────▼───────────▼───────────┐
                         │          main.rs                  │
                         │   ┌──────────┐  ┌─────────────┐  │
                         │   │ cmd_chat │  │  cmd_start   │  │
                         │   │ (direct) │  │ (proxy srv)  │  │
                         │   └────┬─────┘  └──────┬───────┘  │
                         │        │               │          │
                         │   Subcommands: init, auth,        │
                         │   config, doctor, loop            │
                         └────────┬───────────────┬──────────┘
                                  │               │
                    ┌─────────────▼─┐         ┌───▼──────────────┐
                    │    tui.rs     │         │    proxy.rs       │
                    │  Terminal UI  │         │  HTTP Router      │
                    │  (ratatui)    │         │  (axum)           │
                    └───────────────┘         └───────┬──────────┘
                                                      │
                              ┌────────────────────────┤
                              │                        │
                    ┌─────────▼──────────┐   ┌────────▼─────────────────┐
                    │   ProxyState       │   │   Request Pipeline       │
                    │                    │   │                          │
                    │  config            │   │  1. hooks (pre_tool_use) │
                    │  hook_engine       │   │  2. context injection    │
                    │  rules_engine      │   │  3. compaction check     │
                    │  compactor         │   │  4. provider transform   │
                    │  session_manager   │   │  5. upstream request     │
                    │  plugin_manager    │   │  6. response transform   │
                    │  mcp_manager       │   │  7. tool execution loop  │
                    │  oauth_store       │   │  8. VDD review           │
                    │  vdd_engine        │   │  9. hooks (post_tool)    │
                    └────────────────────┘   └──────────────────────────┘
```

## Module Dependency Graph

```
                              ┌─────────────┐
                              │  main.rs    │
                              │  CLI entry  │
                              └──────┬──────┘
                                     │
           ┌──────────┬──────────┬───┴────┬──────────┬──────────┐
           │          │          │        │          │          │
           ▼          ▼          ▼        ▼          ▼          ▼
      ┌─────────┐┌────────┐┌────────┐┌───────┐┌────────┐┌─────────┐
      │proxy.rs ││tools.rs││tui.rs  ││vdd.rs ││oauth.rs││guardrails│
      │HTTP srv ││execute ││term UI ││review ││auth   ││.rs      │
      └────┬────┘└───┬────┘└────────┘└───┬───┘└────────┘└────┬────┘
           │         │                   │                   │
     ┌─────┼─────┐   │              ┌───┘                   │
     │     │     │   │              │                        │
     ▼     ▼     ▼   ▼              ▼                        ▼
┌────────┐ │ ┌──────────┐    ┌───────────┐           ┌───────────┐
│context │ │ │providers │    │  config   │◄──────────│  config   │
│.rs     │ │ │.rs       │    │  .rs      │           │  .rs      │
│inject  │ │ │adapters  │    └───────────┘           └───────────┘
└────────┘ │ └──────────┘
           │
     ┌─────┼──────┬──────────┬──────────┬───────────┐
     │     │      │          │          │           │
     ▼     ▼      ▼          ▼          ▼           ▼
┌───────┐┌────┐┌──────┐┌────────┐┌──────────┐┌──────────┐
│hooks  ││mcp ││rules ││compact ││session   ││plugins   │
│.rs    ││.rs ││.rs   ││ion.rs  ││.rs       ││.rs       │
│events ││ext ││ctx   ││tokens  ││state     ││loading   │
└───────┘│tool│└──────┘└────────┘└──────────┘└──────────┘
         └────┘
```

## Provider Adapter Layer

```
                    ┌───────────────────────────────────┐
                    │         providers.rs               │
                    │    trait ProviderAdapter            │
                    │                                    │
                    │  transform_request()               │
                    │  transform_request_with_thinking() │
                    │  transform_response()              │
                    │  chat_endpoint()                   │
                    │  get_headers()                     │
                    └───────┬───────────────────────────┘
                            │
        ┌──────────┬────────┼────────┬──────────┬──────────┐
        │          │        │        │          │          │
        ▼          ▼        ▼        ▼          ▼          ▼
   ┌─────────┐┌────────┐┌──────┐┌────────┐┌───────┐┌─────────┐
   │Anthropic││ OpenAI ││Google││DeepSeek││ Qwen  ││  Z.AI   │
   │ Claude  ││GPT/o1  ││Gemini││  R1    ││  QwQ  ││  GLM    │
   └─────────┘└────────┘└──────┘└────────┘└───────┘└─────────┘
                  │
                  ▼
            ┌──────────┐
            │  Ollama   │
            │  (local)  │
            └──────────┘

   All adapters translate OpenAI-compatible ←→ native format
   All support thinking/reasoning mode configuration
```

## Tool Execution Pipeline

```
   Provider Response
         │
         ▼
   ┌─────────────────────┐
   │  tool_intercept.rs   │  Parse XML tool calls from
   │  (Claude Code mode)  │  Claude's response stream
   └──────────┬──────────┘
              │
              ▼
   ┌─────────────────────┐     ┌──────────────────┐
   │     tools.rs         │────▶│  permissions.rs   │
   │                      │     │  check allow/deny │
   │  30 built-in tools   │     └──────────────────┘
   └──────────┬──────────┘
              │
    ┌─────────┼─────────┬──────────┬──────────┐
    │         │         │          │          │
    ▼         ▼         ▼          ▼          ▼
┌───────┐┌───────┐┌────────┐┌────────┐┌──────────┐
│ bash  ││ file  ││ search ││ memory ││  web     │
│       ││ ops   ││        ││        ││          │
│execute││read   ││glob    ││save    ││fetch_url │
│bg_run ││write  ││grep    ││search  ││search_web│
│kill   ││edit   ││        ││update  ││          │
└───────┘└───────┘└────────┘└───┬────┘└──────────┘
                                │
                                ▼
                          ┌──────────┐
                          │memory.rs │
                          │ SQLite   │
                          └──────────┘
```

## Memory & Learning System

```
   ┌──────────────────────────────────────────────────────┐
   │                    memory.rs                          │
   │                 SQLite Database                       │
   │                                                      │
   │  ┌────────────────┐  ┌───────────────────────────┐   │
   │  │  Core Memory   │  │    Archival Memory        │   │
   │  │  (always in    │  │    (FTS5 full-text search)│   │
   │  │   context)     │  │    tagged long-term store │   │
   │  │                │  └───────────────────────────┘   │
   │  │  - persona     │                                  │
   │  │  - project     │  ┌───────────────────────────┐   │
   │  │  - preferences │  │   Recent Sessions         │   │
   │  └────────────────┘  │   (48h TTL short-term)    │   │
   │                      └───────────────────────────┘   │
   │  ┌──────────────────────────────────────────────┐    │
   │  │          Auto-Learning Tables (v3)           │    │
   │  │                                              │    │
   │  │  coding_patterns   │ conventions, pitfalls,  │    │
   │  │                    │ architecture decisions   │    │
   │  │  error_patterns    │ error sig → resolution   │    │
   │  │  file_relationships│ co-edit frequency        │    │
   │  │  learned_prefs     │ style, workflow, naming  │    │
   │  └──────────────────────────────────────────────┘    │
   └──────────────────────────┬───────────────────────────┘
                              │
                   ┌──────────▼──────────┐
                   │   auto_learn.rs     │
                   │                     │
                   │  Signals:           │
                   │  - tool success     │───▶ file relationships
                   │  - tool failure     │───▶ error patterns
                   │  - user messages    │───▶ preferences
                   │  - lint output      │───▶ coding patterns
                   │  - session end      │───▶ co-edit graph
                   └─────────────────────┘
```

## Safety & Quality Pipeline

```
   ┌───────────────────────────────────────────────────────┐
   │                  Safety Layers                         │
   │                                                       │
   │  ┌─────────────────────────────────────────────────┐  │
   │  │ 1. permissions.rs                               │  │
   │  │    Glob-pattern allow/deny per tool             │  │
   │  │    Persistent rules + session rules             │  │
   │  └─────────────────────────────────────────────────┘  │
   │                        │                              │
   │  ┌─────────────────────▼───────────────────────────┐  │
   │  │ 2. guardrails.rs                                │  │
   │  │    Blast radius: allowed/denied path patterns   │  │
   │  │    Diff monitor: max lines/files per turn       │  │
   │  │    Quality gates: auto-run linters              │  │
   │  └─────────────────────────────────────────────────┘  │
   │                        │                              │
   │  ┌─────────────────────▼───────────────────────────┐  │
   │  │ 3. hooks.rs (16 lifecycle events)               │  │
   │  │    PreToolUse  → block/modify before execution  │  │
   │  │    PostToolUse → validate after execution       │  │
   │  │    Stop        → halt iteration loops           │  │
   │  │    + 13 more event types                        │  │
   │  └─────────────────────────────────────────────────┘  │
   │                        │                              │
   │  ┌─────────────────────▼───────────────────────────┐  │
   │  │ 4. vdd.rs (Verification-Driven Development)    │  │
   │  │    Adversarial model reviews code output        │  │
   │  │    Advisory: findings injected into next turn   │  │
   │  │    Blocking: loop until confabulation detected  │  │
   │  │    CWE classification, severity scoring         │  │
   │  └─────────────────────────────────────────────────┘  │
   └───────────────────────────────────────────────────────┘
```

## Context Management

```
   ┌────────────────────────────────────────────────────┐
   │               Context Assembly                     │
   │                                                    │
   │  prompt.rs ──────▶ Base system prompt              │
   │                    + memory injection              │
   │                    + custom instructions           │
   │                                                    │
   │  rules.rs ───────▶ Language-specific rules         │
   │                    matched by file extension       │
   │                                                    │
   │  context.rs ─────▶ Hook outputs as                 │
   │                    <system-reminder> tags           │
   │                                                    │
   │  compaction.rs ──▶ Token estimation                │
   │                    85% threshold → summarize       │
   │                    Preserve: recent, system, tools │
   └────────────────────────────────────────────────────┘
```

## External Integration Points

```
   ┌───────────────┐    ┌───────────────┐    ┌───────────────┐
   │   mcp.rs      │    │   oauth.rs    │    │   web.rs      │
   │               │    │               │    │               │
   │  Stdio:       │    │  Device Flow  │    │  fetch_url    │
   │   spawn proc  │    │  PKCE auth    │    │   (Jina)      │
   │   JSON-RPC    │    │  Token store  │    │               │
   │               │    │  API key gen  │    │  search_web   │
   │  HTTP:        │    │               │    │   DuckDuckGo  │
   │   REST client │    │  Claude Max   │    │   Tavily      │
   │               │    │  integration  │    │   Brave       │
   └───────────────┘    └───────────────┘    └───────────────┘

   ┌───────────────┐    ┌───────────────┐    ┌───────────────┐
   │  plugins.rs   │    │   acp.rs      │    │  subagent.rs  │
   │               │    │               │    │               │
   │  Claude Code  │    │  Agent Control│    │  Multi-agent  │
   │  format:      │    │  Protocol     │    │  orchestration│
   │  - commands/  │    │  stdin/stdout │    │  async spawn  │
   │  - hooks/     │    │  interop      │    │  coordinator  │
   │  - .mcp.json  │    └───────────────┘    └───────────────┘
   │  - agents/    │
   │  - skills/    │    ┌───────────────┐
   └───────────────┘    │  skills.rs    │
                        │               │
                        │  Markdown +   │
                        │  YAML skills  │
                        │  invocation   │
                        └───────────────┘
```

## Data Flow Summary

```
  User Input
      │
      ▼
  TUI / HTTP API
      │
      ├──▶ Session (state, turns, tokens)
      │
      ├──▶ Hooks (pre-tool)
      │
      ├──▶ Context (rules + memory + prompts)
      │
      ├──▶ Compaction (if near token limit)
      │
      ├──▶ Provider Adapter (format translation)
      │
      ├──▶ Upstream API ──▶ Response
      │                        │
      │    ┌───────────────────┘
      │    │
      │    ├──▶ Tool Calls ──▶ Permission Check
      │    │                        │
      │    │         ┌──────────────┘
      │    │         │
      │    │    Tool Execution ──▶ Auto-Learn
      │    │         │
      │    │         ├──▶ Memory (persist)
      │    │         │
      │    │         └──▶ Result ──▶ back to provider
      │    │                         (multi-turn loop)
      │    │
      │    ├──▶ VDD Review (if enabled)
      │    │
      │    └──▶ Hooks (post-tool)
      │
      ▼
  Response to User
```

## Module Index

| Module | Lines | Purpose |
|--------|-------|---------|
| `main.rs` | ~3500 | CLI entry, chat loop, subcommands |
| `tools/` | ~1920 | 30 tool definitions and execution |
| `cli/repl/slash.rs` | ~1870 | 45+ slash command handlers |
| `proxy.rs` | ~1770 | HTTP proxy server (axum) |
| `plugins/` | ~1670 | Plugin discovery, loading, management |
| `acp.rs` | ~1610 | Agent Control Protocol server |
| `subagent.rs` | ~1490 | Multi-agent orchestration |
| `memory.rs` | ~1480 | SQLite memory with FTS5 search |
| `hooks/` | ~1380 | 16 lifecycle hook events |
| `vdd/` | ~1290 | Adversarial review engine |
| `compaction.rs` | ~1230 | Context window management |
| `guardrails.rs` | ~1190 | Safety constraints and diff monitoring |
| `tool_intercept.rs` | ~1160 | XML tool call parsing (Claude mode) |
| `mcp.rs` | ~1080 | MCP protocol client |
| `tui/` | ~990 | Terminal UI with ratatui |
| `oauth.rs` | ~760 | OAuth 2.0 device flow |
| `tools/lsp.rs` | ~790 | LSP integration |
| `permissions.rs` | ~640 | Tool allow/deny rules |
| `providers/` | ~540 | 8 provider adapters |
| `web.rs` | ~470 | Web fetch and search |
| `auto_learn.rs` | ~400 | Automatic learning from signals |
| `config/` | ~400+ | YAML config loading |
| `session/` | ~900+ | Session, tasks, pricing, audit |
| `rules.rs` | ~275 | Language-specific rule injection |
| `prompt.rs` | ~230 | System prompt construction |
| `context.rs` | ~200 | Context injection utility |
| `skills.rs` | ~180 | Reusable prompt skills |
