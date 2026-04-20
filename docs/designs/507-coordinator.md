# Design: Coordinator subsystem (crosslink #507)

## Problem

OpenClaudia advertises `--coordinator` mode and has `AgentType::Coordinator` as a subagent variant, but the actual multi-agent orchestration pieces — parallel teammates, dependency-aware task queue, shared context, permission synchronization, reconnection — don't exist. Today's `subagent.rs` is a single-shot 1→1 delegation: one parent spawns one child, waits for its result, done.

Claude Code's equivalent surface area is spread across:
- `coordinator/coordinatorMode.ts` — session-mode detection + coordinator system prompt injection (~369 lines)
- `utils/swarm/` — the actual orchestration: tmux/pane/in-process backends, teammate lifecycle, permission sync, reconnection, layout management (13 files)
- `tools/shared/spawnMultiAgent.ts` — the shared spawn contract (~900 lines)
- `tools/AgentTool/` — the `task` tool with agent color/memory/resume/fork

We need a coordinator subsystem that matches the user-visible behavior (spawn N parallel teammates, share context, reconcile results) without hauling in CC's tmux/pane complexity on day one.

## Proposed architecture

New module `src/coordinator/` with three layers:

```
┌─────────────────────────────────────────────────────────┐
│ coordinator/mod.rs       Public API + state machine     │
├─────────────────────────────────────────────────────────┤
│ coordinator/task_queue.rs  Dependency-aware task graph  │
│ coordinator/teammate.rs    Per-teammate lifecycle       │
│ coordinator/permission.rs  Leader→follower permission   │
└─────────────────────────────────────────────────────────┘
            │                         │
            ▼                         ▼
     subagent.rs                hooks (SubagentStart
     (existing)                  / SubagentStop fire
                                  from teammate.rs)
```

### Key abstractions

```rust
/// What the coordinator owns: a task graph + N live teammates + the
/// result-aggregation logic that decides when the overall run is done.
pub struct Coordinator {
    queue: TaskQueue,
    teammates: HashMap<TeammateId, Teammate>,
    permission_bridge: LeaderPermissionBridge,
    registry: ServiceRegistry,   // analytics + flags from #511
}

/// Newtype over a UUID so teammate ids don't get confused with
/// session ids or agent ids in the existing subagent module.
pub struct TeammateId(Uuid);

/// One unit of work. Depends on zero or more other tasks that must
/// finish before this one can start — the queue resolves the graph
/// on each `next_ready()` call.
pub struct Task {
    pub id: TaskId,
    pub subagent_type: AgentType,
    pub prompt: String,
    pub depends_on: Vec<TaskId>,
    pub assigned_to: Option<TeammateId>,
    pub state: TaskState, // Pending | Running | Done(Result) | Failed(err)
}

/// Per-teammate lifecycle. Wraps subagent::run_subagent with the
/// extra state the coordinator needs (last-heartbeat timestamp,
/// assigned color, accumulated tool calls for the shared transcript).
pub struct Teammate {
    pub id: TeammateId,
    pub agent_type: AgentType,
    pub color: AgentColor,            // from a fixed 7-color palette
    pub state: TeammateState,         // Spawning | Running | Idle | Dead
    pub session_id: String,           // used for per-session todo bucket
    pub transcript_path: PathBuf,     // leverages transcript.rs (#516)
}

/// Leader receives PermissionRequest events on behalf of every
/// teammate. Avoids N simultaneous stdin prompts that would collide.
pub struct LeaderPermissionBridge {
    pending: Mutex<VecDeque<QueuedPermission>>,
    policy: AutoPolicy,               // always-allow rules cached during run
}
```

### Task queue semantics

Rather than implement a full DAG scheduler on day one, ship a simple
readiness-polling queue:

- `TaskQueue::next_ready() -> Option<&mut Task>` — scans pending tasks, returns the first whose `depends_on` are all `Done` and no other teammate is running a same-priority task. O(N) per call is fine — task counts per run will be small.
- Cycle detection at submission time: `TaskQueue::submit(task)` rejects cycles by keeping a topological-sort hash.
- Completion propagation: when a task finishes, the queue re-evaluates readiness for any downstream tasks.

### Permission bridge

Today's subagent path routes `PermissionRequest` events through the event channel to the TUI. In coordinator mode multiple teammates can race to prompt simultaneously. The bridge queues requests and serves them in arrival order, with an "always-allow for this run" cache keyed by `(teammate_id, tool_name)` so an `a` reply doesn't leak across teammates.

## Integration plan (phased)

Three phases, each landable as its own PR:

**Phase 1 — infrastructure (no behavioral change)**
- New `src/coordinator/` module with the types above; `Coordinator::noop()` returns a stub that rejects every `dispatch()` call.
- Wire the module entry but don't use it anywhere — parity tests ensure it compiles and `cargo test --lib coordinator::` passes.
- Tests: task-queue cycle detection, readiness polling, teammate id/color uniqueness.

**Phase 2 — spawn one teammate through the coordinator**
- Coordinator::dispatch(tasks) spawns one teammate per task sequentially — no parallelism yet. Reuses existing `subagent::run_subagent`.
- Fire `SubagentStart` / `SubagentStop` hooks (already defined in #513) from teammate.rs.
- Wire `Coordinator` into `cmd_chat` / `cmd_tui` when `--coordinator` is active.
- Tests: end-to-end task graph of 3 linear tasks, hook fire assertions.

**Phase 3 — parallel teammates + permission bridge**
- Teammates run on independent `tokio::spawn`ed tasks.
- LeaderPermissionBridge intercepts PermissionRequest events from every teammate and serializes them to the user.
- Agent color assignment from the 7-color palette (reuses `AgentType` visual cues).
- Tests: simulate two teammates racing to run a write_file, assert permission prompts serialize in arrival order.

Phases 1–2 are tractable in a single session. Phase 3 is its own commit pair (implementation + integration test).

## Test strategy

- Unit tests in each submodule: `task_queue::tests`, `teammate::tests`, `permission::tests`.
- Integration test: `tests/coordinator_smoke.rs` drives a mock coordinator with scripted subagent responses, asserting task ordering / permission queuing / transcript merge.
- No live API calls in tests — use the existing `subagent::run_subagent` mock path (pipeline layer already supports test injection).

## Risks and open questions

1. **Backend choice (in-process vs tmux vs pane)**. Claude Code supports all three. OC should start in-process only; tmux/pane backends are post-MVP. Document this in the coordinator README so users don't expect visible panes.
2. **Transcript merging**. Each teammate has its own JSONL (via #516); the leader's transcript needs `compact_boundary`-style markers pointing to teammate ids so `/resume` can reconstitute the parallel view. Open question: unify into one transcript or keep them separate and cross-reference? Recommend: separate files, leader transcript carries `{teammate_id, path}` references.
3. **Tool-result ordering determinism**. Parallel teammates finish in non-deterministic order. Transcript should record in finish order (wall-clock) rather than submit order to match what the user sees live.
4. **Cycle detection cost**. Topological sort on every `submit` is O(N²) worst case. Fine for <50 tasks per run; revisit if coordinator is used for long-running batch jobs.
5. **State cleanup on cancellation**. Ctrl+C on the leader needs to reliably kill every live teammate. Use `tokio::select!` with a broadcast cancel channel — same pattern `pipeline::run_turn` uses today.

## Out of scope

- tmux / pane backends
- Remote teammate spawn (another machine)
- Cross-run teammate persistence
- Billing / cost tracking per teammate (future — lives on the `services::analytics` sink)
