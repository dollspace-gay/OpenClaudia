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

/// Parameters for updating an existing task.
#[derive(Default)]
pub struct TaskUpdateParams {
    pub status: Option<String>,
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
        self.tasks.iter().find(|t| t.id == task_id)
    }

    /// Get a mutable reference to a task by ID.
    fn get_task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
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
    #[allow(clippy::too_many_lines)]
    pub fn update_task(
        &mut self,
        task_id: &str,
        params: TaskUpdateParams,
    ) -> Result<Option<&Task>, String> {
        // Validate the task exists
        if self.get_task(task_id).is_none() {
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

        // Parse and validate the new status
        let new_status = if let Some(s) = status.as_deref() {
            match s {
                "pending" => Some(TaskStatus::Pending),
                "in_progress" => Some(TaskStatus::InProgress),
                "completed" => Some(TaskStatus::Completed),
                "deleted" => {
                    // Remove the task entirely
                    self.tasks.retain(|t| t.id != task_id);
                    return Ok(None);
                }
                other => {
                    return Err(format!(
                    "Invalid status '{other}'. Must be: pending, in_progress, completed, deleted"
                ))
                }
            }
        } else {
            None
        };

        // If setting to InProgress, demote any currently in-progress task
        if new_status == Some(TaskStatus::InProgress) {
            for task in &mut self.tasks {
                if task.status == TaskStatus::InProgress && task.id != task_id {
                    task.status = TaskStatus::Pending;
                }
            }
        }

        // Validate dependency references
        if let Some(ref block_ids) = add_blocks {
            for bid in block_ids {
                if bid == task_id {
                    return Err("A task cannot block itself".to_string());
                }
                if !self.tasks.iter().any(|t| t.id == *bid) {
                    return Err(format!("Referenced task '{bid}' not found"));
                }
            }
        }
        if let Some(ref blocked_ids) = add_blocked_by {
            for bid in blocked_ids {
                if bid == task_id {
                    return Err("A task cannot be blocked by itself".to_string());
                }
                if !self.tasks.iter().any(|t| t.id == *bid) {
                    return Err(format!("Referenced task '{bid}' not found"));
                }
            }
        }

        // Apply updates to the task -- task existence validated above
        let task = self
            .get_task_mut(task_id)
            .expect("task must exist after validation");

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
                if !task.blocks.contains(&bid) {
                    task.blocks.push(bid.clone());
                }
                // Also add the reverse relationship on the other task
                // We need to drop the mutable borrow first, so we collect and do it below
            }
        }
        if let Some(blocked_ids) = add_blocked_by {
            for bid in blocked_ids {
                if !task.blocked_by.contains(&bid) {
                    task.blocked_by.push(bid.clone());
                }
            }
        }

        // Now handle reverse relationships for add_blocks/add_blocked_by
        // We need to re-borrow after the first mutable borrow ends
        let task_id_owned = task_id.to_string();

        // For add_blocks: if task A blocks task B, then B.blocked_by should include A
        if let Some(s) = status {
            // Re-read the blocks that were just added (they're on the task now)
            // Actually we need the original add_blocks/add_blocked_by args, but they've been moved.
            // We handle this by doing a second pass.
            let _ = s; // suppress unused warning
        }

        // Second pass: sync reverse dependencies
        // Collect the current blocks and blocked_by for the target task
        let current_blocks: Vec<String> = self
            .get_task(&task_id_owned)
            .map(|t| t.blocks.clone())
            .unwrap_or_default();
        let current_blocked_by: Vec<String> = self
            .get_task(&task_id_owned)
            .map(|t| t.blocked_by.clone())
            .unwrap_or_default();

        // For each task that this task blocks, ensure they have us in blocked_by
        for bid in &current_blocks {
            if let Some(other) = self.get_task_mut(bid) {
                if !other.blocked_by.contains(&task_id_owned) {
                    other.blocked_by.push(task_id_owned.clone());
                }
            }
        }

        // For each task that blocks this task, ensure they have us in blocks
        for bid in &current_blocked_by {
            if let Some(other) = self.get_task_mut(bid) {
                if !other.blocks.contains(&task_id_owned) {
                    other.blocks.push(task_id_owned.clone());
                }
            }
        }

        Ok(Some(
            self.get_task(&task_id_owned)
                .expect("task must exist after update"),
        ))
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
                status: Some("in_progress".into()),
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
                status: Some("in_progress".into()),
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
                status: Some("in_progress".into()),
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
                status: Some("deleted".into()),
                ..Default::default()
            },
        );
        // "deleted" returns Ok(None) — task removed, no reference to return
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
        assert_eq!(tm.list_tasks().len(), 0);
    }

    #[test]
    fn test_task_manager_invalid_status() {
        let mut tm = TaskManager::new();
        tm.create_task("Task".to_string(), "Desc".to_string(), None);

        let result = tm.update_task(
            "task-1",
            TaskUpdateParams {
                status: Some("invalid".into()),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid status"));
    }

    #[test]
    fn test_task_manager_not_found() {
        let mut tm = TaskManager::new();
        let result = tm.update_task(
            "task-999",
            TaskUpdateParams {
                status: Some("completed".into()),
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
                status: Some("in_progress".into()),
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
}
