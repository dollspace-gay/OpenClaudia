# Design: Background memory agents / memdir (crosslink #508)

## Problem

OpenClaudia has `memory.rs` as a SQLite-backed archival store — you can write a fact and retrieve it by keyword. What's missing is the maintenance loop: the background agents that populate and consolidate memory without the user issuing explicit commands.

Claude Code's equivalent subsystems:
- `memdir/` — MEMORY.md entrypoint handling, memory scanning, loading into prompt (13 files)
- `services/extractMemories/` — worker that distills facts from the live session
- `hooks/usePromptSuggestion.ts` — speculative next-prompt prediction using a forked subagent
- `components/memory/MemoryFileSelector.tsx` — UI for picking memory files

Without these, users must explicitly call `/remember` (which OC already supports) and memory never ages out or consolidates. Five specific gaps:

1. **No autoDream** — memory grows unbounded across sessions; no end-of-session consolidation pass.
2. **No MagicDocs** — the automatic project-documentation writer that syncs CLAUDE.md / docs/ from session observations.
3. **No SessionMemory** — per-session notes file at `$CLAUDE_CONFIG_HOME_DIR/projects/<sanitized-cwd>/<session-id>/session-memory/summary.md`. Referenced in the design doc in claude-code-design-documents but not implemented here.
4. **No PromptSuggestion** — speculative fork that predicts the user's next prompt and kicks off a cache-warm request so the live turn feels faster.
5. **No MEMORY.md entrypoint** — CC injects a MEMORY.md from the project root into the system prompt; OC has memory but no automatic injection surface matching the doc.

## Proposed architecture

New module `src/memdir/` with three concerns separated:

```
┌─────────────────────────────────────────────────────────────┐
│ memdir/mod.rs            Public entry + scheduler           │
├─────────────────────────────────────────────────────────────┤
│ memdir/entrypoint.rs     MEMORY.md read/load/truncate       │
│ memdir/session_notes.rs  summary.md per-session writer      │
│ memdir/extractor.rs      Session → memorable-facts worker   │
│ memdir/scheduler.rs      When to run each agent             │
└─────────────────────────────────────────────────────────────┘
           │                    │                    │
           ▼                    ▼                    ▼
      hooks::SessionEnd    context.rs           subagent.rs
      (trigger autoDream)  (inject MEMORY.md)   (run extractor
                                                 in a fork)
```

### Key abstractions

```rust
/// Entry point — user-facing MEMORY.md that gets injected into the
/// system prompt. Capped at MAX_ENTRYPOINT_LINES (200) / MAX_ENTRYPOINT_BYTES
/// (25 000) to match Claude Code's constants.
pub struct EntrypointFile {
    pub path: PathBuf,
    pub content: String,
    pub truncated: bool,
}

impl EntrypointFile {
    pub fn load(project_cwd: &Path) -> Option<Self>;
    pub fn inject_into(&self, prompt_blocks: &mut SystemPromptBlocks);
}

/// Per-session scratch notes written during the session. Lives next
/// to the session transcript in the Claude Code-style tree:
///   <claude_home>/projects/<sanitized-cwd>/<session-id>/session-memory/summary.md
pub struct SessionNotes {
    pub session_id: String,
    pub cwd: PathBuf,
    bullets: Vec<String>,     // append-only during a session
}

impl SessionNotes {
    pub fn load_or_new(session_id: &str, cwd: &Path) -> Self;
    pub fn add_bullet(&mut self, bullet: impl Into<String>);
    pub fn flush(&self) -> io::Result<()>;   // writes summary.md
}

/// Background worker: extract memorable facts from the session
/// transcript. Runs as a forked subagent with a tools=[] callback
/// so the cache-key matches the parent (Claude Code's
/// prompt-suggestion-speculation design req 1).
pub struct Extractor;

impl Extractor {
    pub async fn extract(
        transcript: &[ChatMessage],
        registry: &ServiceRegistry,
    ) -> Result<Vec<MemoryFact>, ExtractorError>;
}

/// When to run each agent. Pure policy — no I/O — so it's trivially
/// testable and the scheduler doesn't need to know about transports.
pub struct Scheduler;

impl Scheduler {
    /// Call at SessionEnd. Returns the set of agents that want to run.
    pub fn on_session_end(state: &SessionSummary) -> Vec<AutoAgent>;
    /// Call on each user prompt submit. Used by prompt suggestion.
    pub fn on_prompt_submit(state: &SessionSummary) -> Vec<AutoAgent>;
}

/// Which background agent to spawn. Each variant maps to a dedicated
/// entry point in memdir::run.
pub enum AutoAgent {
    AutoDream,                    // consolidate across past sessions
    SessionNotesFlush,            // write summary.md at end of session
    PromptSuggestion(String),     // speculate next prompt
    MagicDocs,                    // regenerate project docs
}
```

## Integration plan (phased)

**Phase 1 — MEMORY.md entrypoint injection (no agent, pure file read)**
- `memdir::entrypoint::load()` reads `./MEMORY.md` or `./.openclaudia/MEMORY.md` if present, applies the 200-line / 25 KB cap.
- `context.rs` injects the content as a system-reminder block at prompt build time.
- Tests: load + truncate round-trip, cap-boundary edges, missing-file returns None.
- Deliverable: one commit, zero new dependencies.

**Phase 2 — Session notes writer**
- `memdir::session_notes` writes `summary.md` to the Claude Code-style path on `SessionEnd`.
- Hook into `App::run()` cleanup path (already fires `SessionEnd` from #513).
- The note body is seeded from the AI-generated session title + first/last prompt; fuller extraction comes in Phase 4.
- Tests: path sanitization matches transcript.rs, atomic-write semantics, missing-dir creation.

**Phase 3 — Scheduler**
- Pure-state decision function. Unit-testable without touching disk / network.
- `cli::commands::loop_cmd` and `App::run()` call `Scheduler::on_session_end`/`on_prompt_submit` at the right moments and dispatch the returned `AutoAgent` list.

**Phase 4 — Extractor (the subagent)**
- Forks a subagent with the existing subagent machinery; system prompt templated around "summarize memorable facts from this transcript".
- Runs with tools denied (`permission_mgr` configured to `Deny(*)`) to preserve cache hit rates, matching CC's prompt-suggestion-speculation REQ-1.
- Writes extracted facts into `memory.rs` archival store.
- Tests: integration test with a canned transcript + mocked provider response.

**Phase 5 — AutoDream consolidation**
- Reads accumulated memory across past sessions + extracts the "2+ sessions" patterns matching CC's memory threshold rule.
- Uses AskUserQuestion (already wired via #509) for per-entry confirmation before writing to CLAUDE.local.md.
- Tests: 3-session fixture, pattern dedup, confirmation flow.

**Phase 6 — PromptSuggestion + MagicDocs**
- Larger and can land independently. Both reuse the fork-subagent infra from Phase 4.

Phases 1–2 are one commit each. Phase 3 pairs with its first caller. Phases 4–6 are multi-commit; each is a follow-up issue.

## Test strategy

- Phase 1: pure-file round-trip tests + cap boundary.
- Phase 2: tempdir-scoped tests against the path layout.
- Phase 3: policy function is pure — tests pass scripted `SessionSummary` fixtures.
- Phase 4+: integration tests use the existing subagent test harness with stubbed provider responses. No live API.

## Risks and open questions

1. **Cost**. Running autoDream + extractor at every session end doubles API spend. Gate behind `services::feature_flags::is_enabled("memdir_auto")` (false by default) so power users opt in. Document the cost in the README.
2. **Privacy**. SessionMemory writes prompt text to disk. Must respect `CLAUDE_DISABLE_ESSENTIAL_TRAFFIC` / `DISABLE_ERROR_REPORTING` envs the rest of the codebase already honors.
3. **Prompt-cache drift**. The extractor fork's cache-safe parameters (effort, max_tokens, thinking) must match the parent request exactly (CC's REQ-1). Share a single request-builder helper between the live turn and the fork.
4. **MEMORY.md location discovery**. CC looks at the project root + `~/.claude/`. OC should add `./.openclaudia/MEMORY.md` and `~/.openclaudia/MEMORY.md` without removing the unprefixed `./MEMORY.md` support so users sharing repos with CC colleagues see the same file.
5. **Race between live turn and extractor**. If the extractor writes to `memory.rs` while the live turn reads from it, we need the same SQLite write-ahead-log behavior memory.rs already uses. Verify.
6. **Truncation strategy**. CC truncates MEMORY.md by line-count-first then byte-count-second. OC should match so the overlap behavior is identical between harnesses.

## Out of scope

- UI component for memory editing (`components/memory/MemoryFileSelector.tsx` equivalent — covered by the TUI components follow-up #520)
- Team memory (CC's `teamMemPaths`) — single-user for now
- Growth-book-style A/B of memory strategies
