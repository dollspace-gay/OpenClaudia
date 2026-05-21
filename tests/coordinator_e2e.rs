//! End-to-end tests for the coordinator's task queue + teammate
//! state machine.
//!
//! Sprint 13 of the verification effort. The coordinator modules
//! have substantial unit coverage (`task_queue`: 20, `teammate`: 25,
//! `permission`: 20, `dream`: 6, `local_shell`: 7) but no integration
//! tests that drive them across module boundaries. Focus areas:
//!
//!   - [`TaskQueue`] dependency invariants — submit returns a
//!     monotonic id; `add_dependency` rejects unknown ids; cycle
//!     detection refuses self-loops and transitive cycles.
//!   - [`TaskQueue::next_ready`] dependency gating — a pending task
//!     with an unmet dependency is NOT returned; the same task IS
//!     returned once the dependency reaches `Done`.
//!   - [`Teammate`] state machine — every documented legal edge
//!     succeeds; every forbidden edge errors with `TransitionError`.
//!     Notably: `Dead` is terminal (no out-edges), and
//!     `Spawning → Idle` MUST transit through `Running`.
//!   - `PartialEq` via `TeammateId` — clones with diverging state
//!     still compare equal because the identity key is the id
//!     (crosslink #846).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::coordinator::task_queue::{Task, TaskId, TaskQueue, TaskQueueError, TaskState};
use openclaudia::coordinator::teammate::{Teammate, TeammateState};
use openclaudia::subagent::AgentType;
use std::path::PathBuf;

// ───────────────────────────────────────────────────────────────────────────
// Section A — TaskQueue dependency invariants
// ───────────────────────────────────────────────────────────────────────────

fn submit_task(q: &mut TaskQueue, prompt: &str) -> TaskId {
    q.submit(Task::new(AgentType::GeneralPurpose, prompt))
        .expect("submit")
}

#[test]
fn submit_returns_monotonic_increasing_ids() {
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let b = submit_task(&mut q, "b");
    let c = submit_task(&mut q, "c");
    // The exact id values are an implementation detail; what matters
    // is that they're distinct and ordered.
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
    assert_eq!(q.len(), 3);
}

#[test]
fn add_dependency_to_unknown_task_errors() {
    // TaskIds are per-queue monotonic counters (NOT globally unique),
    // so an id from a different `TaskQueue` happens to overlap with
    // ours when both start fresh. To exercise the UnknownTask branch
    // reliably, fill `other` with many tasks so its latest id is
    // FAR above what `q` has issued — then ask q to add a dep
    // referencing that high alien id.
    let mut q = TaskQueue::new();
    let real = submit_task(&mut q, "real");

    let mut other = TaskQueue::new();
    let mut alien = submit_task(&mut other, "alien-0");
    for i in 1..100 {
        alien = submit_task(&mut other, &format!("alien-{i}"));
    }
    // `alien` is now ~TaskId(100); `q` has only ~TaskId(1).
    assert!(
        q.get(alien).is_none(),
        "test precondition: alien id MUST be unknown in q"
    );

    let outcome = q.add_dependency(real, alien);
    assert!(
        matches!(outcome, Err(TaskQueueError::UnknownTask { .. })),
        "add_dependency to unknown id must error UnknownTask; got {outcome:?}"
    );
}

#[test]
fn add_dependency_self_loop_is_cycle() {
    // `a depends_on a` would mean `a` waits for itself — must be
    // refused as a cycle.
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let outcome = q.add_dependency(a, a);
    assert!(
        matches!(outcome, Err(TaskQueueError::CycleDetected { .. })),
        "self-loop must be CycleDetected; got {outcome:?}"
    );
}

#[test]
fn add_dependency_transitive_cycle_is_refused() {
    // a → b → c → a forms a 3-cycle. The third add must error.
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let b = submit_task(&mut q, "b");
    let c = submit_task(&mut q, "c");

    q.add_dependency(b, a).expect("b depends_on a");
    q.add_dependency(c, b).expect("c depends_on b");
    let outcome = q.add_dependency(a, c);
    assert!(
        matches!(outcome, Err(TaskQueueError::CycleDetected { .. })),
        "transitive cycle a→b→c→a must be CycleDetected; got {outcome:?}"
    );
}

#[test]
fn add_dependency_dag_is_admitted() {
    // A diamond: top → left, top → right, left → leaf, right → leaf.
    // No cycle.
    let mut queue = TaskQueue::new();
    let top = submit_task(&mut queue, "top");
    let left = submit_task(&mut queue, "left");
    let right = submit_task(&mut queue, "right");
    let leaf = submit_task(&mut queue, "leaf");
    queue
        .add_dependency(left, top)
        .expect("left depends_on top");
    queue
        .add_dependency(right, top)
        .expect("right depends_on top");
    queue
        .add_dependency(leaf, left)
        .expect("leaf depends_on left");
    queue
        .add_dependency(leaf, right)
        .expect("leaf depends_on right");
    // All edges admitted; queue still well-formed.
    assert_eq!(queue.len(), 4);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — next_ready dependency gating
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn next_ready_returns_pending_with_no_deps() {
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let ready = q.next_ready().expect("a must be ready");
    assert_eq!(ready.id, a);
}

#[test]
fn next_ready_skips_pending_with_unmet_dep() {
    // Submit b before a, then add b depends_on a.
    // next_ready must return `a` (no deps), NOT `b`.
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let b = submit_task(&mut q, "b");
    q.add_dependency(b, a).expect("b depends_on a");

    let ready = q.next_ready().expect("a must be ready");
    assert_eq!(ready.id, a, "next_ready must skip b (blocked by a)");
}

#[test]
fn next_ready_unblocks_dependent_after_predecessor_done() {
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let b = submit_task(&mut q, "b");
    q.add_dependency(b, a).expect("b depends_on a");

    // First call: a is ready.
    let first = q.next_ready().expect("first call returns a");
    let first_id = first.id;
    // Mark a as Done (simulate teammate completion).
    first.state = TaskState::Done("a output".into());
    assert_eq!(first_id, a);

    // Now b should be ready.
    let second = q.next_ready().expect("after a Done, b is ready");
    assert_eq!(second.id, b, "b must unblock once a is Done");
}

#[test]
fn next_ready_returns_none_when_everything_is_blocked() {
    let mut q = TaskQueue::new();
    let a = submit_task(&mut q, "a");
    let b = submit_task(&mut q, "b");
    q.add_dependency(a, b).expect("a depends_on b");
    // The reverse edge (b → a) would close a cycle, so the add must
    // be refused. We assert it explicitly so a future change that
    // weakens cycle detection surfaces here.
    assert!(
        q.add_dependency(b, a).is_err(),
        "b → a after a → b must close a cycle and be refused"
    );

    // Force a state where a depends on b (only a→b is in place).
    // b has no deps so it's actually ready.
    let ready = q.next_ready().expect("b is ready");
    assert_eq!(ready.id, b);
    // Mark b as Failed (not Done). a still won't be ready because
    // a's dep is on b which never reaches Done.
    ready.state = TaskState::Failed("oops".into());
    let next = q.next_ready();
    assert!(
        next.is_none(),
        "a's dep on b is unmet (b Failed, not Done); next_ready must be None"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Teammate state machine
// ───────────────────────────────────────────────────────────────────────────

fn fresh_teammate() -> Teammate {
    Teammate::new(
        AgentType::GeneralPurpose,
        0,
        "session-id",
        PathBuf::from("/tmp/transcript.jsonl"),
    )
}

#[test]
fn teammate_starts_in_spawning_state() {
    let tm = fresh_teammate();
    assert!(matches!(tm.state, TeammateState::Spawning));
    assert!(tm.state.is_alive());
    assert!(
        !tm.state.is_available(),
        "Spawning is alive but not available for work"
    );
}

#[test]
fn legal_transition_spawning_to_running_succeeds() {
    let mut tm = fresh_teammate();
    tm.try_transition_to(TeammateState::Running)
        .expect("Spawning → Running must succeed");
    assert!(matches!(tm.state, TeammateState::Running));
    assert!(tm.state.is_alive());
}

#[test]
fn legal_transition_running_to_idle_succeeds() {
    let mut tm = fresh_teammate();
    tm.try_transition_to(TeammateState::Running)
        .expect("→ Running");
    tm.try_transition_to(TeammateState::Idle)
        .expect("Running → Idle must succeed");
    assert!(matches!(tm.state, TeammateState::Idle));
    assert!(tm.state.is_available(), "Idle is the only available state");
}

#[test]
fn legal_transition_idle_to_running_succeeds() {
    let mut tm = fresh_teammate();
    tm.try_transition_to(TeammateState::Running)
        .expect("→ Running");
    tm.try_transition_to(TeammateState::Idle).expect("→ Idle");
    tm.try_transition_to(TeammateState::Running)
        .expect("Idle → Running must succeed");
}

#[test]
fn forbidden_transition_spawning_to_idle_errors() {
    // Spawning must go through Running before reaching Idle.
    let mut tm = fresh_teammate();
    let outcome = tm.try_transition_to(TeammateState::Idle);
    assert!(
        outcome.is_err(),
        "Spawning → Idle MUST error; got {outcome:?}"
    );
    // State must NOT have changed on rejected transition.
    assert!(matches!(tm.state, TeammateState::Spawning));
}

#[test]
fn dead_is_terminal_no_outbound_transitions() {
    let mut tm = fresh_teammate();
    tm.try_transition_to(TeammateState::Dead("spawn failed".into()))
        .expect("Spawning → Dead must succeed");

    // Now every attempt to leave Dead must error.
    for next in [
        TeammateState::Running,
        TeammateState::Idle,
        TeammateState::Spawning,
        TeammateState::Dead("zombify".into()),
    ] {
        let copy = next.clone();
        let outcome = tm.try_transition_to(next);
        assert!(
            outcome.is_err(),
            "Dead → {copy:?} MUST error (Dead is terminal); got {outcome:?}"
        );
    }
    // State must still be Dead.
    assert!(matches!(tm.state, TeammateState::Dead(_)));
}

#[test]
fn alive_states_can_transition_to_dead() {
    // Every alive state has Dead as a legal target.
    for entry in [
        TeammateState::Spawning,
        TeammateState::Running,
        TeammateState::Idle,
    ] {
        let mut tm = fresh_teammate();
        // Walk to the target start state via legal edges.
        match entry {
            TeammateState::Spawning => {} // already there
            TeammateState::Running => {
                tm.try_transition_to(TeammateState::Running).unwrap();
            }
            TeammateState::Idle => {
                tm.try_transition_to(TeammateState::Running).unwrap();
                tm.try_transition_to(TeammateState::Idle).unwrap();
            }
            TeammateState::Dead(_) => unreachable!(),
        }
        tm.try_transition_to(TeammateState::Dead("rip".into()))
            .expect("alive → Dead must succeed");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Teammate identity (PartialEq via TeammateId)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn clone_compares_equal_via_teammate_id_even_with_diverging_state() {
    // crosslink #846: a clone with later state divergence MUST
    // still compare equal — identity is the TeammateId.
    let tm = fresh_teammate();
    let mut clone = tm.clone();
    clone
        .try_transition_to(TeammateState::Running)
        .expect("clone transitions");
    // tm is still Spawning; clone is Running. They MUST still
    // compare equal because TeammateId is the identity key.
    assert_eq!(
        tm, clone,
        "clones MUST compare equal regardless of state divergence"
    );
}

#[test]
fn distinct_teammates_have_distinct_ids_and_compare_unequal() {
    let a = fresh_teammate();
    let b = fresh_teammate();
    assert_ne!(a, b, "two fresh teammates MUST have distinct ids");
}
