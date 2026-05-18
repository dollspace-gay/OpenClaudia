//! Dependency-aware task queue for the coordinator.
//!
//! Simple data structure: a `Vec<Task>` with newtype-wrapped ids
//! and O(N) readiness polling. Expected task counts per run stay
//! small (<50), so big-O complexity doesn't matter vs. the
//! simplicity of the implementation. Phase 3+ can swap this out
//! for a topological-sort-backed scheduler if batch-job workloads
//! appear.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::subagent::AgentType;

use super::TeammateId;

/// Task identifier — opaque. Assigned by [`TaskQueue::submit`],
/// used by dependency edges and for re-attaching results after
/// teammate completion. Wrapping a `u64` keeps it `Copy` so the
/// usual `Vec<TaskId>` manipulation doesn't force clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(u64);

impl TaskId {
    /// Raw numeric id — useful for log fields. Not for equality
    /// (use `==` on `TaskId` directly; the newtype is the point).
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Where a task sits in its lifecycle. The coordinator reads this
/// to decide which tasks are ready to start, which need retrying,
/// etc.
#[derive(Debug, Clone)]
pub enum TaskState {
    /// Not yet running — may still be blocked by unmet dependencies.
    Pending,
    /// Assigned to a teammate and currently executing.
    Running,
    /// Finished successfully. Payload is the teammate's final text
    /// output — downstream tasks can read it as context.
    Done(String),
    /// Teammate returned an error or crashed. Payload is the
    /// human-readable error message (typically surfaced to the
    /// user in the final coordinator report).
    Failed(String),
}

/// One unit of work. Constructed by the coordinator caller, passed
/// to [`TaskQueue::submit`]. The queue assigns an id on submit and
/// returns it so the caller can reference the task in `depends_on`
/// vectors of subsequent submissions.
#[derive(Debug, Clone)]
pub struct Task {
    /// Assigned by submit — callers leave this at the default
    /// sentinel when building the task.
    pub id: TaskId,
    pub subagent_type: AgentType,
    /// Free-form prompt handed to the teammate verbatim.
    pub prompt: String,
    /// Ids of tasks that must reach `Done` before this one can run.
    pub depends_on: Vec<TaskId>,
    /// Teammate currently working on this task, or `None` if
    /// pending.
    pub assigned_to: Option<TeammateId>,
    pub state: TaskState,
}

impl Task {
    /// Build a new pending task. The id is filled in by
    /// [`TaskQueue::submit`] — users of this builder set it to
    /// the sentinel `TaskId(0)`.
    #[must_use]
    pub fn new(subagent_type: AgentType, prompt: impl Into<String>) -> Self {
        Self {
            id: TaskId(0), // placeholder — submit overwrites
            subagent_type,
            prompt: prompt.into(),
            depends_on: Vec::new(),
            assigned_to: None,
            state: TaskState::Pending,
        }
    }

    /// Chainable `depends_on` setter — convenient when building
    /// fixtures in tests.
    #[must_use]
    pub fn depends_on(mut self, deps: Vec<TaskId>) -> Self {
        self.depends_on = deps;
        self
    }
}

/// Queue errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskQueueError {
    #[error("task {missing:?} is not in the queue")]
    UnknownTask { missing: TaskId },
    #[error("adding dependency {from:?} → {to:?} would form a cycle")]
    CycleDetected { from: TaskId, to: TaskId },
}

/// The queue itself.
#[derive(Debug, Default)]
pub struct TaskQueue {
    /// Monotonic id counter. `0` is reserved for `Task::new`'s
    /// placeholder so tests can't accidentally conflate "unset"
    /// with "task id zero".
    next_id: u64,
    tasks: Vec<Task>,
}

impl TaskQueue {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_id: 1,
            tasks: Vec::new(),
        }
    }

    /// How many tasks are in the queue, regardless of state.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Is the queue empty?
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Submit a task. Assigns a fresh id, validates every
    /// `depends_on` id exists, and rejects cycles before inserting.
    ///
    /// # Errors
    ///
    /// - `TaskQueueError::UnknownTask` if a declared dependency id
    ///   isn't in the queue.
    /// - `TaskQueueError::CycleDetected` if the new task's
    ///   dependency graph closes a loop (impossible here since new
    ///   tasks can only depend on already-submitted ones, but
    ///   [`Self::add_dependency`] is the real user of this check).
    pub fn submit(&mut self, mut task: Task) -> Result<TaskId, TaskQueueError> {
        for dep in &task.depends_on {
            if !self.tasks.iter().any(|t| t.id == *dep) {
                return Err(TaskQueueError::UnknownTask { missing: *dep });
            }
        }
        let id = TaskId(self.next_id);
        self.next_id += 1;
        task.id = id;
        self.tasks.push(task);
        Ok(id)
    }

    /// Add a `from → to` dependency edge to an existing task. Used
    /// by Phase 3 retry / reorder flows. Detects cycles via a
    /// simple reachability check before inserting — O(N·E) worst
    /// case, fine for small N.
    ///
    /// # Errors
    ///
    /// `UnknownTask` if either id is missing; `CycleDetected` if
    /// the edge would close a loop.
    pub fn add_dependency(&mut self, from: TaskId, to: TaskId) -> Result<(), TaskQueueError> {
        if !self.tasks.iter().any(|t| t.id == from) {
            return Err(TaskQueueError::UnknownTask { missing: from });
        }
        if !self.tasks.iter().any(|t| t.id == to) {
            return Err(TaskQueueError::UnknownTask { missing: to });
        }
        // Would adding the edge `from depends_on to` close a cycle?
        // The new edge says `from` must wait for `to`. A cycle
        // forms if `to` already transitively depends on `from` —
        // then `from` would wait for itself. Check by walking the
        // existing depends_on graph starting at `to` and seeing
        // if we can reach `from`.
        if self.path_exists(to, from) {
            return Err(TaskQueueError::CycleDetected { from, to });
        }
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == from) {
            task.depends_on.push(to);
        }
        Ok(())
    }

    /// True when there's already a dependency path from `start`
    /// to `target` (transitive). Depth-first, terminates on the
    /// finite task set. Used by cycle detection.
    fn path_exists(&self, start: TaskId, target: TaskId) -> bool {
        let mut visited: HashSet<TaskId> = HashSet::new();
        let mut stack: Vec<TaskId> = vec![start];
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if !visited.insert(node) {
                continue;
            }
            // Follow the "depends_on" edges — `node` depends on
            // these next ids.
            let Some(task) = self.tasks.iter().find(|t| t.id == node) else {
                continue;
            };
            stack.extend(task.depends_on.iter().copied());
        }
        false
    }

    /// Return the next pending task whose dependencies have all
    /// completed. O(N) — the expected task count makes that fine.
    /// `None` when either nothing is pending or every pending task
    /// is still blocked.
    pub fn next_ready(&mut self) -> Option<&mut Task> {
        // Collect ids of Done tasks so the search below can answer
        // "are all my deps done?" without reborrowing the vec.
        let done_ids: HashSet<TaskId> = self
            .tasks
            .iter()
            .filter_map(|t| match t.state {
                TaskState::Done(_) => Some(t.id),
                _ => None,
            })
            .collect();
        self.tasks.iter_mut().find(|t| {
            matches!(t.state, TaskState::Pending)
                && t.depends_on.iter().all(|d| done_ids.contains(d))
        })
    }

    /// Lookup by id — used by the coordinator's result-propagation
    /// pass after a teammate finishes a task.
    #[must_use]
    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Mutable lookup.
    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::AgentType;

    fn task(kind: AgentType, label: &str) -> Task {
        Task::new(kind, label)
    }

    #[test]
    fn submit_assigns_monotonic_ids() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        let b = q.submit(task(AgentType::Explore, "b")).unwrap();
        let c = q.submit(task(AgentType::Explore, "c")).unwrap();
        assert!(a.raw() < b.raw() && b.raw() < c.raw());
        // None are the placeholder zero.
        assert!(a.raw() >= 1);
    }

    #[test]
    fn submit_rejects_unknown_dependency() {
        let mut q = TaskQueue::new();
        let fake = TaskId(9999);
        let err = q
            .submit(task(AgentType::Explore, "x").depends_on(vec![fake]))
            .unwrap_err();
        assert_eq!(err, TaskQueueError::UnknownTask { missing: fake });
    }

    #[test]
    fn next_ready_respects_dependencies() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        let _b = q
            .submit(task(AgentType::Plan, "b").depends_on(vec![a]))
            .unwrap();

        // First ready must be `a` — `b` is blocked.
        let first = q.next_ready().expect("a should be ready");
        assert_eq!(first.id, a);
        first.state = TaskState::Done("done".into());

        // After marking a done, b becomes ready.
        let second = q.next_ready().expect("b should be ready after a done");
        assert_ne!(second.id, a);
    }

    #[test]
    fn next_ready_none_when_all_blocked_or_running() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        let _b = q
            .submit(task(AgentType::Plan, "b").depends_on(vec![a]))
            .unwrap();
        // Mark `a` Running — `b` still blocked (needs Done not
        // Running), so next_ready returns None.
        q.get_mut(a).unwrap().state = TaskState::Running;
        assert!(q.next_ready().is_none());
    }

    #[test]
    fn cycle_detected_on_direct_edge_reversal() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        let b = q
            .submit(task(AgentType::Plan, "b").depends_on(vec![a]))
            .unwrap();
        // Adding a → b closes the loop (b already depends on a).
        let err = q.add_dependency(a, b).unwrap_err();
        assert_eq!(err, TaskQueueError::CycleDetected { from: a, to: b });
    }

    #[test]
    fn cycle_detected_on_transitive_edge() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        let b = q
            .submit(task(AgentType::Plan, "b").depends_on(vec![a]))
            .unwrap();
        let c = q
            .submit(task(AgentType::GeneralPurpose, "c").depends_on(vec![b]))
            .unwrap();
        // a → c would make a -> c -> b -> a, via transitive.
        let err = q.add_dependency(a, c).unwrap_err();
        assert!(matches!(err, TaskQueueError::CycleDetected { .. }));
    }

    #[test]
    fn add_dependency_allows_non_cycling_edges() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        let b = q.submit(task(AgentType::Plan, "b")).unwrap();
        let c = q.submit(task(AgentType::GeneralPurpose, "c")).unwrap();
        // a → b and a → c are both fine.
        q.add_dependency(a, b).unwrap();
        q.add_dependency(a, c).unwrap();
        assert_eq!(q.get(a).unwrap().depends_on, vec![b, c]);
    }

    #[test]
    fn get_and_get_mut_return_the_same_task() {
        let mut q = TaskQueue::new();
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        assert_eq!(q.get(a).unwrap().id, a);
        q.get_mut(a).unwrap().state = TaskState::Done("x".into());
        assert!(matches!(q.get(a).unwrap().state, TaskState::Done(_)));
    }

    #[test]
    fn len_counts_all_states() {
        let mut q = TaskQueue::new();
        assert!(q.is_empty());
        let a = q.submit(task(AgentType::Explore, "a")).unwrap();
        q.submit(task(AgentType::Plan, "b")).unwrap();
        q.get_mut(a).unwrap().state = TaskState::Done("x".into());
        assert_eq!(q.len(), 2);
    }
}

/// Phase 2 spec pins — #532 behavioral contracts for [`TaskQueue`].
///
/// Each test is labelled with the spec behavior it pins (B1 / B5).
/// Do NOT remove or weaken these tests to fix a queue bug — file a
/// gap issue instead so the spec and code stay in sync.
#[cfg(test)]
mod phase2_spec_pins {
    use super::*;
    use crate::subagent::AgentType;

    fn t(label: &str) -> Task {
        Task::new(AgentType::Explore, label)
    }

    // ── B1: dependency semantics ─────────────────────────────────────

    /// B1a: task with empty `depends_on` is immediately eligible (#532
    /// B1 edge-case: "empty `depends_on` vec is immediately eligible").
    #[test]
    fn b1_no_deps_task_immediately_ready() {
        let mut q = TaskQueue::new();
        q.submit(t("standalone")).unwrap();
        assert!(
            q.next_ready().is_some(),
            "task with no deps must be ready immediately",
        );
    }

    /// B1b: TaskId(0) is the sentinel used by `Task::new` before submit;
    /// submit always returns an id >= 1 (#532 B1).
    #[test]
    fn b1_submitted_id_never_zero() {
        let mut q = TaskQueue::new();
        let id = q.submit(t("first")).unwrap();
        assert!(id.raw() >= 1, "submit must never return TaskId(0)");
    }

    /// B1c: Running dep does NOT unblock a dependent task — only Done
    /// does (#532 B1, OC `task_queue.rs:232`).
    #[test]
    fn b1_running_dep_still_blocks_dependent() {
        let mut q = TaskQueue::new();
        let a = q.submit(t("a")).unwrap();
        q.submit(Task::new(AgentType::Plan, "b").depends_on(vec![a]))
            .unwrap();

        // Mark `a` Running (not Done).
        q.get_mut(a).unwrap().state = TaskState::Running;

        // `b` must still be blocked; next_ready must return None
        // because `a` is Running, not Done.
        assert!(
            q.next_ready().is_none(),
            "Running dep must not unblock a dependent task — only Done does",
        );
    }

    /// B1d: submit with an unknown dep id returns `UnknownTask` and
    /// does NOT insert the task (#532 B1).
    #[test]
    fn b1_submit_unknown_dep_not_inserted() {
        let mut q = TaskQueue::new();
        let ghost = TaskId(42);
        let err = q.submit(t("bad").depends_on(vec![ghost])).unwrap_err();
        assert_eq!(
            err,
            TaskQueueError::UnknownTask { missing: ghost },
            "wrong error variant",
        );
        // Queue must be empty — the task was not inserted.
        assert_eq!(q.len(), 0, "failed submit must not insert the task");
    }

    /// B1e: ids are monotonically increasing across multiple submits
    /// (#532 B1: "monotonically increasing, starting at 1").
    #[test]
    fn b1_ids_monotonically_increasing() {
        let mut q = TaskQueue::new();
        let ids: Vec<u64> = (0..5)
            .map(|i| q.submit(t(&format!("t{i}"))).unwrap().raw())
            .collect();
        for w in ids.windows(2) {
            assert!(w[0] < w[1], "ids must be strictly monotone: {ids:?}");
        }
    }

    /// B1f: `len()` counts tasks in ALL states, not just Pending
    /// (#532 B1 edge-case).
    #[test]
    fn b1_len_counts_all_states_exhaustive() {
        let mut q = TaskQueue::new();
        let a = q.submit(t("a")).unwrap();
        let b = q.submit(t("b")).unwrap();
        let c = q.submit(t("c")).unwrap();
        let d = q.submit(t("d")).unwrap();

        q.get_mut(a).unwrap().state = TaskState::Pending;
        q.get_mut(b).unwrap().state = TaskState::Running;
        q.get_mut(c).unwrap().state = TaskState::Done("ok".into());
        q.get_mut(d).unwrap().state = TaskState::Failed("err".into());

        assert_eq!(q.len(), 4, "len must count Pending+Running+Done+Failed");
    }

    // ── B5: cycle detection ──────────────────────────────────────────

    /// B5a: `add_dependency` with unknown `from` returns `UnknownTask`
    /// (#532 B5).
    #[test]
    fn b5_add_dep_unknown_from_rejected() {
        let mut q = TaskQueue::new();
        let b = q.submit(t("b")).unwrap();
        let ghost = TaskId(999);
        let err = q.add_dependency(ghost, b).unwrap_err();
        assert_eq!(err, TaskQueueError::UnknownTask { missing: ghost });
    }

    /// B5b: `add_dependency` with unknown `to` returns `UnknownTask`
    /// (#532 B5).
    #[test]
    fn b5_add_dep_unknown_to_rejected() {
        let mut q = TaskQueue::new();
        let a = q.submit(t("a")).unwrap();
        let ghost = TaskId(999);
        let err = q.add_dependency(a, ghost).unwrap_err();
        assert_eq!(err, TaskQueueError::UnknownTask { missing: ghost });
    }

    /// B5c: on cycle-detection error the queue is NOT mutated — the
    /// edge is not partially inserted (#532 B5 side-effect clause).
    #[test]
    fn b5_cycle_error_leaves_queue_unchanged() {
        let mut q = TaskQueue::new();
        let a = q.submit(t("a")).unwrap();
        let b = q
            .submit(Task::new(AgentType::Plan, "b").depends_on(vec![a]))
            .unwrap();

        let deps_before = q.get(a).unwrap().depends_on.clone();

        // Adding a → b would cycle: b already depends on a.
        let err = q.add_dependency(a, b).unwrap_err();
        assert!(matches!(err, TaskQueueError::CycleDetected { from, to } if from == a && to == b));

        // `a`'s depends_on must be exactly what it was before.
        assert_eq!(
            q.get(a).unwrap().depends_on,
            deps_before,
            "queue must be unchanged after cycle rejection",
        );
    }

    /// B5d: `CycleDetected` carries the correct from/to ids (#532 B5
    /// error-format clause).
    #[test]
    fn b5_cycle_detected_error_format() {
        let mut q = TaskQueue::new();
        let a = q.submit(t("a")).unwrap();
        let b = q
            .submit(Task::new(AgentType::GeneralPurpose, "b").depends_on(vec![a]))
            .unwrap();

        match q.add_dependency(a, b) {
            Err(TaskQueueError::CycleDetected { from, to }) => {
                assert_eq!(from, a, "from must be the originating node");
                assert_eq!(to, b, "to must be the node that closes the cycle");
            }
            other => panic!("expected CycleDetected, got {other:?}"),
        }
    }

    /// B5e: non-cycling `add_dependency` succeeds and the edge is
    /// appended to `depends_on` (#532 B5 Ok-path).
    #[test]
    fn b5_valid_add_dependency_appended() {
        let mut q = TaskQueue::new();
        let a = q.submit(t("a")).unwrap();
        let b = q.submit(t("b")).unwrap();
        let c = q.submit(t("c")).unwrap();

        q.add_dependency(a, b).unwrap();
        q.add_dependency(a, c).unwrap();

        let deps = &q.get(a).unwrap().depends_on;
        assert!(deps.contains(&b), "b must be in a.depends_on");
        assert!(deps.contains(&c), "c must be in a.depends_on");
    }
}
