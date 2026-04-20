# Design documents

Grounded designs for the three remaining heavy issues in the
Claude Code parity tree. Each doc is self-contained: you can hand
it to a fresh agent and have them implement Phase 1 without
reading the rest.

| Issue | Title                             | Design |
|-------|-----------------------------------|--------|
| #507  | Coordinator subsystem             | [507-coordinator.md](507-coordinator.md) |
| #508  | Background memory agents / memdir | [508-memdir.md](508-memdir.md) |
| #510  | Centralized SessionState          | [510-session-state.md](510-session-state.md) |

## Dependencies between them

- **#510 SessionState** is load-bearing for #508: the memdir agents want to subscribe to `SessionSwitched` / `MessageAppended` events that #510's StateStore emits. You can ship memdir without #510 but you'll end up wiring listeners manually in each caller; revisit once SessionState lands.
- **#507 Coordinator** depends on #513 (hook trigger points — already shipped) for `SubagentStart` / `SubagentStop` emission, and on #516 (compact-boundary markers — already shipped) for per-teammate transcripts.
- **#508 memdir** depends on #509 `AskUserQuestion` (shipped) for autoDream confirmation prompts, and on #511 `services::feature_flags` (shipped) for the opt-in gate.

## Phasing recommendation

If picked up in complexity order:

1. **#510 Phase 0–1** first (2 commits) — ship the module, migrate Identity + Conversation. Non-invasive.
2. **#508 Phase 1–2** (2 commits) — MEMORY.md entrypoint + session notes writer. Zero dependency on #510.
3. **#507 Phase 1** (1 commit) — Coordinator infrastructure, no behavioral change.
4. Iterate — each subsequent phase is landable independently.

Total to get all three in a usable state: ~5 commits minimum; ~15 for full feature parity.
