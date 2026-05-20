//! Structured task management with dependency tracking.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// Status of a managed task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
        }
    }
}

/// A structured task with dependency tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task identifier (auto-incrementing, e.g. "task-1")
    pub id: String,
    /// Brief title in imperative form (e.g. "Add permission system")
    pub subject: String,
    /// Detailed description of the task
    pub description: String,
    /// Present continuous form for spinner display (e.g. "Adding permission system")
    pub active_form: Option<String>,
    /// Current task status
    pub status: TaskStatus,
    /// IDs of tasks that this task blocks (downstream dependencies)
    pub blocks: Vec<String>,
    /// IDs of tasks that block this task (upstream dependencies)
    pub blocked_by: Vec<String>,
    /// When the task was created
    pub created_at: DateTime<Utc>,
}

/// Outcome of [`TaskManager::apply_status_transition`] — distinguishes
/// "no status field supplied" from "status set" from "task deleted" without
/// overloading `Option<Result<…>>`. crosslink #874.
enum StatusOutcome {
    /// Caller omitted the `status` field; keep whatever the task had.
    Unchanged,
    /// Caller supplied `status: "deleted"`; the task has been removed.
    Deleted,
    /// Caller supplied a real status; this is the new value.
    Transitioned(TaskStatus),
}

/// Status values accepted by task updates (includes Deleted which removes the task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskUpdateStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
}

impl TaskUpdateStatus {
    /// Parse from string. Returns None for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// Parameters for updating an existing task.
#[derive(Default)]
pub struct TaskUpdateParams {
    pub status: Option<TaskUpdateStatus>,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub add_blocks: Option<Vec<String>>,
    pub add_blocked_by: Option<Vec<String>>,
}

/// Manages structured tasks with dependency tracking.
///
/// Enforces the invariant that only one task can be `InProgress` at a time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskManager {
    tasks: Vec<Task>,
    next_id: u64,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    /// Create a new empty `TaskManager`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
        }
    }

    /// Create a new task. Returns the created task.
    ///
    /// # Panics
    ///
    /// Panics if the internal tasks vector is somehow empty after pushing
    /// (should be unreachable).
    pub fn create_task(
        &mut self,
        subject: String,
        description: String,
        active_form: Option<String>,
    ) -> &Task {
        let id = format!("task-{}", self.next_id);
        self.next_id += 1;

        let task = Task {
            id,
            subject,
            description,
            active_form,
            status: TaskStatus::Pending,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            created_at: Utc::now(),
        };
        self.tasks.push(task);
        self.tasks
            .last()
            .expect("tasks must be non-empty after push")
    }

    /// Get a task by ID.
    #[must_use]
    pub fn get_task(&self, task_id: &str) -> Option<&Task> {
        self.index_of(task_id).map(|i| &self.tasks[i])
    }

    /// Get a mutable reference to a task by ID.
    fn get_task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        let idx = self.index_of(task_id)?;
        self.tasks.get_mut(idx)
    }

    /// Locate the position of `task_id` in `self.tasks`, if any.
    ///
    /// crosslink #874: still O(N) in the worst case (`Vec` is the storage),
    /// but centralising the scan here is a prerequisite for the planned move
    /// to a `HashMap<TaskId, usize>` index — every caller now goes through a
    /// single helper instead of open-coding `.iter().find(..)`.
    fn index_of(&self, task_id: &str) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == task_id)
    }

    /// Build a temporary `id` -> `index` map for one call's worth of lookups.
    ///
    /// Used by [`update_task`] which performs O(M) dependency-existence
    /// checks (one per added edge). Building the map up front is O(N); each
    /// lookup is then O(1), turning the previous O(M*N) loop into O(N+M).
    fn build_id_index(&self) -> std::collections::HashMap<&str, usize> {
        self.tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect()
    }

    /// Check if adding an edge from `from_id` blocks `to_id` would create a cycle.
    /// Uses DFS: starting from `to_id`, follow `blocks` edges. If we reach `from_id`,
    /// there's a cycle.
    fn would_create_cycle(&self, from_id: &str, to_id: &str) -> bool {
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![to_id.to_string()];
        while let Some(current) = stack.pop() {
            if current == from_id {
                return true;
            }
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(task) = self.get_task(&current) {
                for blocked in &task.blocks {
                    stack.push(blocked.clone());
                }
            }
        }
        false
    }

    /// Update a task's fields. Returns an error message if validation fails.
    ///
    /// Enforces that only one task can be `InProgress` at a time. When a task
    /// is set to `InProgress`, any currently in-progress task is moved back to
    /// `Pending`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the task is not found, the status is invalid,
    /// a dependency references itself or a nonexistent task, or the task
    /// is deleted (deletion is signaled via `Err` with a message).
    ///
    /// # Panics
    ///
    /// Panics if internal lookups fail after validation (should be unreachable).
    ///
    /// crosslink #874: the previous 150-line god function has been split into
    /// focused helpers (status transition, dependency validation, reverse-
    /// edge sync). Dependency existence checks build a `HashMap<&str, usize>`
    /// once instead of doing an O(N) scan per edge, turning the inner loop
    /// from O(M*N) into O(N+M).
    pub fn update_task(
        &mut self,
        task_id: &str,
        params: TaskUpdateParams,
    ) -> Result<Option<&Task>, String> {
        // Validate the task exists
        if self.index_of(task_id).is_none() {
            return Err(format!("Task '{task_id}' not found"));
        }

        let TaskUpdateParams {
            status,
            subject,
            description,
            active_form,
            add_blocks,
            add_blocked_by,
        } = params;

        // Phase 1: handle status transition (Deleted is a short-circuit return).
        let new_status = match self.apply_status_transition(task_id, status.as_ref())? {
            StatusOutcome::Deleted => return Ok(None),
            StatusOutcome::Unchanged => None,
            StatusOutcome::Transitioned(s) => Some(s),
        };

        // Phase 2: validate every added dependency against an O(1) id index.
        self.validate_dependency_edges(task_id, add_blocks.as_deref(), add_blocked_by.as_deref())?;

        // Phase 3: apply scalar field updates and the new edges.
        Self::apply_task_fields(
            self.get_task_mut(task_id)
                .expect("task must exist after validation"),
            new_status,
            subject,
            description,
            active_form,
            add_blocks.as_deref(),
            add_blocked_by.as_deref(),
        );

        // Phase 4: sync reverse edges (both directions) so blocks/blocked_by
        // are always symmetric.
        self.sync_reverse_edges(task_id);

        Ok(Some(
            self.get_task(task_id)
                .expect("task must exist after update"),
        ))
    }

    /// Apply (or short-circuit) a status transition. Returns the new
    /// `TaskStatus` to set (None means no status field was supplied) or
    /// `Deleted` to tell the caller the task is gone.
    fn apply_status_transition(
        &mut self,
        task_id: &str,
        status: Option<&TaskUpdateStatus>,
    ) -> Result<StatusOutcome, String> {
        let new_status = match status {
            None => return Ok(StatusOutcome::Unchanged),
            Some(TaskUpdateStatus::Deleted) => {
                self.tasks.retain(|t| t.id != task_id);
                return Ok(StatusOutcome::Deleted);
            }
            Some(TaskUpdateStatus::Pending) => TaskStatus::Pending,
            Some(TaskUpdateStatus::InProgress) => TaskStatus::InProgress,
            Some(TaskUpdateStatus::Completed) => TaskStatus::Completed,
        };

        if new_status == TaskStatus::InProgress {
            // Enforce blocked_by: every blocker must be Completed (crosslink #593).
            let blockers: Vec<String> = self
                .get_task(task_id)
                .map(|t| t.blocked_by.clone())
                .unwrap_or_default();
            for blocker_id in &blockers {
                match self.get_task(blocker_id).map(|t| &t.status) {
                    Some(TaskStatus::Completed) => {}
                    Some(status) => {
                        return Err(format!(
                            "Task '{task_id}' cannot transition to in_progress: blocker '{blocker_id}' is {status}"
                        ));
                    }
                    None => {
                        return Err(format!(
                            "Task '{task_id}' references nonexistent blocker '{blocker_id}'"
                        ));
                    }
                }
            }

            // Demote any currently in-progress task to Pending.
            for task in &mut self.tasks {
                if task.status == TaskStatus::InProgress && task.id != task_id {
                    task.status = TaskStatus::Pending;
                }
            }
        }

        Ok(StatusOutcome::Transitioned(new_status))
    }

    /// Validate every edge in `add_blocks` / `add_blocked_by` against the
    /// task store. Uses an `id -> index` [`HashMap`] built once so each
    /// existence check is O(1).
    ///
    /// [`HashMap`]: std::collections::HashMap
    fn validate_dependency_edges(
        &self,
        task_id: &str,
        add_blocks: Option<&[String]>,
        add_blocked_by: Option<&[String]>,
    ) -> Result<(), String> {
        let index = self.build_id_index();

        if let Some(block_ids) = add_blocks {
            for bid in block_ids {
                if bid == task_id {
                    return Err("A task cannot block itself".to_string());
                }
                if !index.contains_key(bid.as_str()) {
                    return Err(format!("Referenced task '{bid}' not found"));
                }
                if self.would_create_cycle(task_id, bid) {
                    return Err(format!(
                        "Adding '{task_id}' blocks '{bid}' would create a circular dependency"
                    ));
                }
            }
        }
        if let Some(blocked_ids) = add_blocked_by {
            for bid in blocked_ids {
                if bid == task_id {
                    return Err("A task cannot be blocked by itself".to_string());
                }
                if !index.contains_key(bid.as_str()) {
                    return Err(format!("Referenced task '{bid}' not found"));
                }
                if self.would_create_cycle(bid, task_id) {
                    return Err(format!(
                        "Adding '{bid}' blocks '{task_id}' would create a circular dependency"
                    ));
                }
            }
        }

        Ok(())
    }

    /// Apply the validated scalar / edge updates to a single task. Operates
    /// on `&mut Task` directly so the caller controls the borrow lifetime.
    fn apply_task_fields(
        task: &mut Task,
        new_status: Option<TaskStatus>,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<String>,
        add_blocks: Option<&[String]>,
        add_blocked_by: Option<&[String]>,
    ) {
        if let Some(s) = new_status {
            task.status = s;
        }
        if let Some(subj) = subject {
            task.subject = subj;
        }
        if let Some(desc) = description {
            task.description = desc;
        }
        if active_form.is_some() {
            task.active_form = active_form;
        }
        if let Some(block_ids) = add_blocks {
            for bid in block_ids {
                if !task.blocks.iter().any(|b| b == bid) {
                    task.blocks.push(bid.clone());
                }
            }
        }
        if let Some(blocked_ids) = add_blocked_by {
            for bid in blocked_ids {
                if !task.blocked_by.iter().any(|b| b == bid) {
                    task.blocked_by.push(bid.clone());
                }
            }
        }
    }

    /// Restore the symmetric invariant: for every `A blocks B`, `B.blocked_by`
    /// must contain `A`, and vice versa. Called after `apply_task_fields` so
    /// new edges are propagated to the other end.
    fn sync_reverse_edges(&mut self, task_id: &str) {
        let task_id_owned = task_id.to_string();
        let current_blocks: Vec<String> = self
            .get_task(&task_id_owned)
            .map(|t| t.blocks.clone())
            .unwrap_or_default();
        let current_blocked_by: Vec<String> = self
            .get_task(&task_id_owned)
            .map(|t| t.blocked_by.clone())
            .unwrap_or_default();

        for bid in &current_blocks {
            if let Some(other) = self.get_task_mut(bid) {
                if !other.blocked_by.contains(&task_id_owned) {
                    other.blocked_by.push(task_id_owned.clone());
                }
            }
        }
        for bid in &current_blocked_by {
            if let Some(other) = self.get_task_mut(bid) {
                if !other.blocks.contains(&task_id_owned) {
                    other.blocks.push(task_id_owned.clone());
                }
            }
        }
    }

    /// List all tasks.
    #[must_use]
    pub fn list_tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Get the currently in-progress task, if any.
    #[must_use]
    pub fn current_task(&self) -> Option<&Task> {
        self.tasks
            .iter()
            .find(|t| t.status == TaskStatus::InProgress)
    }

    /// Format a task summary for display.
    #[must_use]
    pub fn format_task_summary(task: &Task) -> String {
        let status_icon = match task.status {
            TaskStatus::Pending => "[ ]",
            TaskStatus::InProgress => "[>]",
            TaskStatus::Completed => "[x]",
        };

        let mut summary = format!(
            "{status_icon} {} {} ({})",
            task.id, task.subject, task.status
        );

        if let Some(ref af) = task.active_form {
            let _ = write!(summary, " -- {af}");
        }

        if !task.blocks.is_empty() {
            let _ = write!(summary, "\n    blocks: {}", task.blocks.join(", "));
        }
        if !task.blocked_by.is_empty() {
            let _ = write!(summary, "\n    blocked_by: {}", task.blocked_by.join(", "));
        }

        summary
    }

    /// Format full task details for display.
    #[must_use]
    pub fn format_task_detail(task: &Task) -> String {
        let mut detail = String::new();
        let _ = writeln!(detail, "ID: {}", task.id);
        let _ = writeln!(detail, "Subject: {}", task.subject);
        let _ = writeln!(detail, "Status: {}", task.status);
        let _ = writeln!(detail, "Description: {}", task.description);
        if let Some(ref af) = task.active_form {
            let _ = writeln!(detail, "Active form: {af}");
        }
        let _ = writeln!(
            detail,
            "Created: {}",
            task.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        if !task.blocks.is_empty() {
            let _ = writeln!(detail, "Blocks: {}", task.blocks.join(", "));
        }
        if !task.blocked_by.is_empty() {
            let _ = writeln!(detail, "Blocked by: {}", task.blocked_by.join(", "));
        }
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_manager_create() {
        let mut tm = TaskManager::new();
        let task = tm.create_task(
            "Implement feature".to_string(),
            "Add the new feature".to_string(),
            Some("Implementing feature".to_string()),
        );
        assert_eq!(task.id, "task-1");
        assert_eq!(task.subject, "Implement feature");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.active_form, Some("Implementing feature".to_string()));
    }

    #[test]
    fn test_task_manager_auto_increment() {
        let mut tm = TaskManager::new();
        tm.create_task("A".to_string(), "Desc".to_string(), None);
        tm.create_task("B".to_string(), "Desc".to_string(), None);
        tm.create_task("C".to_string(), "Desc".to_string(), None);

        let tasks = tm.list_tasks();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, "task-1");
        assert_eq!(tasks[1].id, "task-2");
        assert_eq!(tasks[2].id, "task-3");
    }

    #[test]
    fn test_task_manager_update_status() {
        let mut tm = TaskManager::new();
        tm.create_task("Task A".to_string(), "Desc".to_string(), None);

        let result = tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        );
        assert!(result.is_ok());
        assert_eq!(
            tm.get_task("task-1").unwrap().status,
            TaskStatus::InProgress
        );
    }

    #[test]
    fn test_task_manager_single_in_progress() {
        let mut tm = TaskManager::new();
        tm.create_task("Task A".to_string(), "Desc".to_string(), None);
        tm.create_task("Task B".to_string(), "Desc".to_string(), None);

        tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            tm.get_task("task-1").unwrap().status,
            TaskStatus::InProgress
        );

        tm.update_task(
            "task-2",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tm.get_task("task-1").unwrap().status, TaskStatus::Pending);
        assert_eq!(
            tm.get_task("task-2").unwrap().status,
            TaskStatus::InProgress
        );
    }

    #[test]
    fn test_task_manager_delete() {
        let mut tm = TaskManager::new();
        tm.create_task("To delete".to_string(), "Desc".to_string(), None);
        assert_eq!(tm.list_tasks().len(), 1);

        let result = tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::Deleted),
                ..Default::default()
            },
        );
        // "deleted" returns Ok(None) — task removed, no reference to return
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
        assert_eq!(tm.list_tasks().len(), 0);
    }

    #[test]
    fn test_task_update_status_parse() {
        assert_eq!(
            TaskUpdateStatus::parse("pending"),
            Some(TaskUpdateStatus::Pending)
        );
        assert_eq!(
            TaskUpdateStatus::parse("in_progress"),
            Some(TaskUpdateStatus::InProgress)
        );
        assert_eq!(
            TaskUpdateStatus::parse("completed"),
            Some(TaskUpdateStatus::Completed)
        );
        assert_eq!(
            TaskUpdateStatus::parse("deleted"),
            Some(TaskUpdateStatus::Deleted)
        );
        assert_eq!(TaskUpdateStatus::parse("invalid"), None);
        assert_eq!(TaskUpdateStatus::parse(""), None);
    }

    #[test]
    fn test_task_manager_not_found() {
        let mut tm = TaskManager::new();
        let result = tm.update_task(
            "task-999",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::Completed),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_task_manager_dependencies() {
        let mut tm = TaskManager::new();
        tm.create_task("Setup".to_string(), "First step".to_string(), None);
        tm.create_task("Build".to_string(), "Second step".to_string(), None);

        // task-2 blocked by task-1
        tm.update_task(
            "task-2",
            TaskUpdateParams {
                add_blocked_by: Some(vec!["task-1".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();

        let task1 = tm.get_task("task-1").unwrap();
        let task2 = tm.get_task("task-2").unwrap();
        assert!(task2.blocked_by.contains(&"task-1".to_string()));
        assert!(task1.blocks.contains(&"task-2".to_string()));
    }

    #[test]
    fn test_task_manager_self_dependency_blocked() {
        let mut tm = TaskManager::new();
        tm.create_task("Task".to_string(), "Desc".to_string(), None);

        let result = tm.update_task(
            "task-1",
            TaskUpdateParams {
                add_blocks: Some(vec!["task-1".to_string()]),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot block itself"));
    }

    #[test]
    fn test_task_manager_current_task() {
        let mut tm = TaskManager::new();
        assert!(tm.current_task().is_none());

        tm.create_task("Task".to_string(), "Desc".to_string(), None);
        assert!(tm.current_task().is_none()); // still pending

        tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(tm.current_task().is_some());
        assert_eq!(tm.current_task().unwrap().id, "task-1");
    }

    #[test]
    fn test_task_manager_format_summary() {
        let mut tm = TaskManager::new();
        let task = tm.create_task(
            "Fix bug".to_string(),
            "Fix the null pointer".to_string(),
            Some("Fixing bug".to_string()),
        );
        let summary = TaskManager::format_task_summary(task);
        assert!(summary.contains("[ ]")); // pending icon
        assert!(summary.contains("task-1"));
        assert!(summary.contains("Fix bug"));
        assert!(summary.contains("Fixing bug"));
    }

    // ── Phase 2 spec-pinning tests (#552 / spec #537 B-session/task) ─────────

    /// Spec — `TaskStatus::Display` renders the canonical strings used by the
    /// tool layer and the session summary.
    #[test]
    fn task_status_display_strings() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::InProgress.to_string(), "in_progress");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
    }

    /// Spec — creating a task always starts it as `Pending`.
    #[test]
    fn new_task_starts_pending() {
        let mut tm = TaskManager::new();
        let t = tm.create_task("Deploy".to_string(), "desc".to_string(), None);
        assert_eq!(t.status, TaskStatus::Pending);
    }

    /// Spec — setting a second task to `InProgress` demotes the current one to
    /// `Pending`. Enforces the single-in-progress invariant.
    #[test]
    fn single_in_progress_invariant() {
        let mut tm = TaskManager::new();
        tm.create_task("A".to_string(), "d".to_string(), None);
        tm.create_task("B".to_string(), "d".to_string(), None);
        tm.create_task("C".to_string(), "d".to_string(), None);

        tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
        tm.update_task(
            "task-2",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
        tm.update_task(
            "task-3",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();

        // Only task-3 should be InProgress; task-1 and task-2 must be Pending.
        assert_eq!(tm.get_task("task-1").unwrap().status, TaskStatus::Pending);
        assert_eq!(tm.get_task("task-2").unwrap().status, TaskStatus::Pending);
        assert_eq!(
            tm.get_task("task-3").unwrap().status,
            TaskStatus::InProgress
        );

        // `current_task()` reflects the single in-progress entry.
        let cur = tm.current_task().unwrap();
        assert_eq!(cur.id, "task-3");
    }

    /// Spec — updating a non-existent task returns `Err`.
    #[test]
    fn update_nonexistent_task_returns_err() {
        let mut tm = TaskManager::new();
        let res = tm.update_task("task-99", TaskUpdateParams::default());
        assert!(res.is_err());
    }

    /// Spec — `Deleted` status removes the task from the list and returns `Ok(None)`.
    #[test]
    fn delete_removes_task_returns_ok_none() {
        let mut tm = TaskManager::new();
        tm.create_task("X".to_string(), "d".to_string(), None);
        let res = tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::Deleted),
                ..Default::default()
            },
        );
        assert!(res.is_ok());
        assert!(res.unwrap().is_none(), "Deleted must return Ok(None)");
        assert!(tm.list_tasks().is_empty(), "task must be gone from list");
    }

    /// Spec — cycle detection: A blocks B, then adding B blocks A must fail.
    #[test]
    fn cycle_detection_rejects_circular_dependency() {
        let mut tm = TaskManager::new();
        tm.create_task("A".to_string(), "d".to_string(), None);
        tm.create_task("B".to_string(), "d".to_string(), None);

        // task-1 blocks task-2
        tm.update_task(
            "task-1",
            TaskUpdateParams {
                add_blocks: Some(vec!["task-2".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();

        // Now try to make task-2 block task-1 — should be rejected as a cycle
        let res = tm.update_task(
            "task-2",
            TaskUpdateParams {
                add_blocks: Some(vec!["task-1".to_string()]),
                ..Default::default()
            },
        );
        assert!(res.is_err(), "circular dependency must be rejected");
        let msg = res.unwrap_err();
        assert!(
            msg.contains("circular") || msg.contains("cycle"),
            "error must mention circularity, got: {msg}"
        );
    }

    /// Spec — `add_blocks` syncs the reverse `blocked_by` on the target task.
    #[test]
    fn add_blocks_syncs_reverse_blocked_by() {
        let mut tm = TaskManager::new();
        tm.create_task("First".to_string(), "d".to_string(), None);
        tm.create_task("Second".to_string(), "d".to_string(), None);

        tm.update_task(
            "task-1",
            TaskUpdateParams {
                add_blocks: Some(vec!["task-2".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();

        let t2 = tm.get_task("task-2").unwrap();
        assert!(
            t2.blocked_by.contains(&"task-1".to_string()),
            "task-2.blocked_by must contain task-1 after add_blocks"
        );
    }

    // ── crosslink #593: blocked_by enforcement on InProgress transition ─────

    /// #593 — A task with empty `blocked_by` can always transition to `InProgress`.
    #[test]
    fn issue_593_empty_blocked_by_allows_in_progress() {
        let mut tm = TaskManager::new();
        tm.create_task("Solo".to_string(), "d".to_string(), None);
        let res = tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        );
        assert!(res.is_ok(), "empty blocked_by must allow InProgress");
        assert_eq!(
            tm.get_task("task-1").unwrap().status,
            TaskStatus::InProgress
        );
    }

    /// #593 — All blockers Completed → `InProgress` transition succeeds.
    #[test]
    fn issue_593_all_blockers_completed_allows_in_progress() {
        let mut tm = TaskManager::new();
        tm.create_task("A".to_string(), "d".to_string(), None);
        tm.create_task("B".to_string(), "d".to_string(), None);
        tm.create_task("C".to_string(), "d".to_string(), None);

        // task-3 is blocked by task-1 and task-2
        tm.update_task(
            "task-3",
            TaskUpdateParams {
                add_blocked_by: Some(vec!["task-1".to_string(), "task-2".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();

        // Complete both blockers
        tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::Completed),
                ..Default::default()
            },
        )
        .unwrap();
        tm.update_task(
            "task-2",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::Completed),
                ..Default::default()
            },
        )
        .unwrap();

        // task-3 should now be allowed to transition
        let res = tm.update_task(
            "task-3",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        );
        assert!(
            res.is_ok(),
            "all-Completed blockers must allow InProgress, got: {res:?}"
        );
        assert_eq!(
            tm.get_task("task-3").unwrap().status,
            TaskStatus::InProgress
        );
    }

    /// #593 — A Pending blocker rejects the `InProgress` transition with a
    /// clear error message naming the blocker.
    #[test]
    fn issue_593_pending_blocker_rejects_in_progress() {
        let mut tm = TaskManager::new();
        tm.create_task("Setup".to_string(), "d".to_string(), None);
        tm.create_task("Build".to_string(), "d".to_string(), None);

        // task-2 blocked by task-1 (Pending by default)
        tm.update_task(
            "task-2",
            TaskUpdateParams {
                add_blocked_by: Some(vec!["task-1".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();

        let res = tm.update_task(
            "task-2",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        );
        assert!(res.is_err(), "Pending blocker must reject InProgress");
        let msg = res.unwrap_err();
        assert!(
            msg.contains("task-1") && msg.contains("pending"),
            "error must name blocker and its status, got: {msg}"
        );
        // Status must not have changed.
        assert_eq!(tm.get_task("task-2").unwrap().status, TaskStatus::Pending);
    }

    /// #593 — An `InProgress` blocker rejects the transition.
    #[test]
    fn issue_593_in_progress_blocker_rejects_in_progress() {
        let mut tm = TaskManager::new();
        tm.create_task("Setup".to_string(), "d".to_string(), None);
        tm.create_task("Build".to_string(), "d".to_string(), None);

        // task-2 blocked by task-1
        tm.update_task(
            "task-2",
            TaskUpdateParams {
                add_blocked_by: Some(vec!["task-1".to_string()]),
                ..Default::default()
            },
        )
        .unwrap();
        // task-1 is now InProgress (single-in-progress rule still holds)
        tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();

        let res = tm.update_task(
            "task-2",
            TaskUpdateParams {
                status: Some(TaskUpdateStatus::InProgress),
                ..Default::default()
            },
        );
        assert!(res.is_err(), "InProgress blocker must reject InProgress");
        let msg = res.unwrap_err();
        assert!(
            msg.contains("task-1") && msg.contains("in_progress"),
            "error must name blocker and its in_progress status, got: {msg}"
        );
        // task-1 must remain InProgress (rejection happens before demote pass).
        assert_eq!(
            tm.get_task("task-1").unwrap().status,
            TaskStatus::InProgress
        );
        assert_eq!(tm.get_task("task-2").unwrap().status, TaskStatus::Pending);
    }

    /// Spec — `format_task_detail` contains all key fields.
    #[test]
    fn format_task_detail_contains_required_fields() {
        let mut tm = TaskManager::new();
        let task = tm.create_task(
            "Write tests".to_string(),
            "Full description here".to_string(),
            Some("Writing tests".to_string()),
        );
        let detail = TaskManager::format_task_detail(task);
        assert!(detail.contains("Write tests"));
        assert!(detail.contains("Full description here"));
        assert!(detail.contains("Writing tests"));
        assert!(detail.contains("pending"), "detail must include status");
    }
}
