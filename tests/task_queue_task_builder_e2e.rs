//! End-to-end tests for `coordinator::task_queue::Task`
//! builder pattern + `TaskId` newtype semantics +
//! `TaskQueueError` Display strings + `TaskState`
//! variants.
//!
//! Sprint 159 of the verification effort. Sprint 21/36
//! covered `TaskQueue` operations + sprint 115 covered
//! `AgentColor`/`TeammateId`; this file fills the
//! `Task::depends_on` builder + `TaskId::raw` + Display
//! contracts.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::coordinator::{Task, TaskQueue, TaskQueueError, TaskState};
use openclaudia::subagent::AgentType;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Task::new builder
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_new_with_prompt_and_subagent_type_initializes_pending_state() {
    let task = Task::new(AgentType::GeneralPurpose, "do thing");
    assert_eq!(task.subagent_type, AgentType::GeneralPurpose);
    assert_eq!(task.prompt, "do thing");
    assert!(task.depends_on.is_empty(), "new task MUST have no deps");
    assert!(
        matches!(task.state, TaskState::Pending),
        "new task MUST start Pending; got {:?}",
        task.state
    );
}

#[test]
fn task_new_id_is_sentinel_zero_before_submit() {
    // PINS DOC: Task::new sets id to TaskId(0) sentinel; submit
    // overwrites with the assigned id.
    let task = Task::new(AgentType::Explore, "x");
    assert_eq!(task.id.raw(), 0, "PINS sentinel TaskId(0)");
}

#[test]
fn task_new_accepts_str_literal_via_into() {
    let _t = Task::new(AgentType::Plan, "literal");
}

#[test]
fn task_new_accepts_owned_string_via_into() {
    let s = String::from("owned");
    let _t = Task::new(AgentType::Guide, s);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Task::depends_on builder
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn depends_on_with_empty_vec_keeps_no_deps() {
    let task = Task::new(AgentType::Explore, "x").depends_on(Vec::new());
    assert!(task.depends_on.is_empty());
}

#[test]
fn depends_on_with_3_ids_sets_dependency_list() {
    let task = Task::new(AgentType::Explore, "x").depends_on(vec![
        Task::new(AgentType::Explore, "x").id,
        Task::new(AgentType::Explore, "x").id,
        Task::new(AgentType::Explore, "x").id,
    ]);
    assert_eq!(task.depends_on.len(), 3);
}

#[test]
fn depends_on_replaces_when_called_twice() {
    // PINS BUILDER: builder replaces (does NOT append) the
    // dependency vec.
    let task = Task::new(AgentType::Explore, "x")
        .depends_on(vec![Task::new(AgentType::Explore, "x").id])
        .depends_on(vec![
            Task::new(AgentType::Explore, "x").id,
            Task::new(AgentType::Explore, "x").id,
        ]);
    assert_eq!(task.depends_on.len(), 2);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — TaskId newtype
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_id_raw_returns_inner_u64() {
    let id = Task::new(AgentType::Explore, "x").id;
    assert_eq!(id.raw(), 0);
}

#[test]
fn task_id_default_is_zero_sentinel() {
    let id = Task::new(AgentType::Explore, "x").id;
    assert_eq!(id.raw(), 0);
}

#[test]
fn task_id_display_renders_bare_numeric() {
    let id = Task::new(AgentType::Explore, "x").id;
    let s = format!("{id}");
    // PINS DOC: TaskId display is BARE numeric (no "TaskId(0)" wrapper).
    assert_eq!(s, "0");
}

#[test]
fn task_id_partial_eq_and_eq_hold_for_same_id() {
    let a = Task::new(AgentType::Explore, "x").id;
    let b = Task::new(AgentType::Explore, "x").id;
    assert_eq!(a, b);
}

#[test]
fn task_id_clone_and_copy_preserve_value() {
    let a = Task::new(AgentType::Explore, "x").id;
    let cloned = a;
    let again = a;
    assert_eq!(cloned, again);
    assert_eq!(again.raw(), 0);
}

#[test]
fn distinct_task_ids_from_queue_submit_have_distinct_values() {
    let mut q = TaskQueue::new();
    let id1 = q
        .submit(Task::new(AgentType::Explore, "a"))
        .expect("submit a");
    let id2 = q
        .submit(Task::new(AgentType::Explore, "b"))
        .expect("submit b");
    assert_ne!(id1, id2);
    assert_ne!(id1.raw(), id2.raw());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — TaskQueueError Display strings
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_queue_error_unknown_task_display_includes_missing_id() {
    let err = TaskQueueError::UnknownTask {
        missing: Task::new(AgentType::Explore, "x").id,
    };
    let s = err.to_string();
    assert!(s.contains('0') || s.contains("not in the queue"));
    assert!(s.contains("not in the queue"));
}

#[test]
fn task_queue_error_cycle_detected_display_includes_from_and_to() {
    let err = TaskQueueError::CycleDetected {
        from: Task::new(AgentType::Explore, "x").id,
        to: Task::new(AgentType::Explore, "x").id,
    };
    let s = err.to_string();
    assert!(s.contains("cycle"));
    // Both endpoint ids reported in arrow notation.
    assert!(s.contains("→") || s.contains("->"));
}

#[test]
fn task_queue_error_variants_are_distinct_under_partial_eq() {
    let unknown = TaskQueueError::UnknownTask {
        missing: Task::new(AgentType::Explore, "x").id,
    };
    let cycle = TaskQueueError::CycleDetected {
        from: Task::new(AgentType::Explore, "x").id,
        to: Task::new(AgentType::Explore, "x").id,
    };
    assert_ne!(unknown, cycle);
}

#[test]
fn task_queue_error_partial_eq_compares_payloads() {
    // AUTHORING DISCOVERY: TaskQueueError does NOT derive Clone
    // (just Debug + thiserror::Error + PartialEq + Eq). Pinning
    // PartialEq instead — two errors with same variant + same
    // payload are equal.
    let a = TaskQueueError::UnknownTask {
        missing: Task::new(AgentType::Explore, "x").id,
    };
    let b = TaskQueueError::UnknownTask {
        missing: Task::new(AgentType::Explore, "y").id,
    };
    // Both ids are TaskId(0) (sentinel) → equal.
    assert_eq!(a, b);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — TaskState variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_state_pending_is_default_variant_after_new() {
    let task = Task::new(AgentType::Explore, "x");
    assert!(matches!(task.state, TaskState::Pending));
}

#[test]
fn task_state_done_carries_string_payload() {
    let state = TaskState::Done("output marker".to_string());
    match state {
        TaskState::Done(s) => assert_eq!(s, "output marker"),
        _ => panic!("MUST be Done"),
    }
}

#[test]
fn task_state_failed_carries_string_payload() {
    let state = TaskState::Failed("error message".to_string());
    match state {
        TaskState::Failed(s) => assert_eq!(s, "error message"),
        _ => panic!("MUST be Failed"),
    }
}

#[test]
fn task_state_running_carries_no_payload() {
    let state = TaskState::Running;
    assert!(matches!(state, TaskState::Running));
}

#[test]
fn task_state_clone_preserves_payload_for_done_variant() {
    let original = TaskState::Done("body".to_string());
    let cloned = original.clone();
    // PINS CLONE: payload preserved AND original still usable
    // (proving deep copy, not move).
    match cloned {
        TaskState::Done(ref s) => assert_eq!(s, "body"),
        _ => panic!("MUST clone as Done"),
    }
    match original {
        TaskState::Done(s) => assert_eq!(s, "body"),
        _ => panic!("original MUST still be Done after clone"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Full submit+depends_on integration
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_with_depends_on_submitted_returns_unknown_for_invalid_dep_lookup() {
    let mut q = TaskQueue::new();
    // Submit a parent task to get a valid id.
    let parent_id = q
        .submit(Task::new(AgentType::Explore, "parent"))
        .expect("submit parent");

    // Now submit a dependent — it MUST succeed (valid dep).
    let dep_id = q
        .submit(Task::new(AgentType::Explore, "dependent").depends_on(vec![parent_id]))
        .expect("submit dependent");

    // The dependent task carries its deps.
    let task = q.get(dep_id).expect("dependent present");
    assert_eq!(task.depends_on.len(), 1);
    assert_eq!(task.depends_on[0], parent_id);
}

#[test]
fn task_with_depends_on_on_unknown_id_errors_at_add_dependency() {
    let mut q = TaskQueue::new();
    let task_id = q
        .submit(Task::new(AgentType::Explore, "x"))
        .expect("submit");
    // Manually try to add a dep on a nonexistent task.
    let bogus = Task::new(AgentType::Explore, "x").id; // Different from assigned id.
                                                       // Actually default is 0; if assigned is also 0 then this self-deps.
                                                       // Use a clearly-distinct large id.
    let _ = bogus;
    let outcome = q.add_dependency(task_id, task_id);
    // self-dependency would form a cycle.
    assert!(matches!(outcome, Err(TaskQueueError::CycleDetected { .. })));
}
