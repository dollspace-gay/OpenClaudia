use crate::session::TaskManager;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Execute the `task_create` tool
pub fn execute_task_create(
    args: &HashMap<String, Value>,
    task_mgr: &mut TaskManager,
) -> (String, bool) {
    let subject = match args.get("subject").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return ("Missing 'subject' argument".to_string(), true),
    };

    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d.to_string(),
        None => return ("Missing 'description' argument".to_string(), true),
    };

    let active_form = args
        .get("active_form")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let task = task_mgr.create_task(subject, description, active_form);
    let output = format!(
        "Created task: {}\n{}",
        task.id,
        TaskManager::format_task_detail(task)
    );
    (output, false)
}

/// Execute the `task_update` tool
pub fn execute_task_update(
    args: &HashMap<String, Value>,
    task_mgr: &mut TaskManager,
) -> (String, bool) {
    let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
        return ("Missing 'task_id' argument".to_string(), true);
    };

    let status = args.get("status").and_then(|v| v.as_str());
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let active_form = args
        .get("active_form")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let add_blocks: Option<Vec<String>> =
        args.get("add_blocks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

    let add_blocked_by: Option<Vec<String>> = args
        .get("add_blocked_by")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

    match task_mgr.update_task(
        task_id,
        crate::session::TaskUpdateParams {
            status: status.and_then(crate::session::TaskUpdateStatus::parse),
            subject,
            description,
            active_form,
            add_blocks,
            add_blocked_by,
        },
    ) {
        Ok(Some(task)) => {
            let output = format!(
                "Updated task: {}\n{}",
                task.id,
                TaskManager::format_task_detail(task)
            );
            (output, false)
        }
        Ok(None) => {
            // Task was deleted successfully
            (format!("Task '{task_id}' deleted"), false)
        }
        Err(msg) => (msg, true),
    }
}

/// Execute the `task_get` tool
pub fn execute_task_get(args: &HashMap<String, Value>, task_mgr: &TaskManager) -> (String, bool) {
    let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
        return ("Missing 'task_id' argument".to_string(), true);
    };

    task_mgr.get_task(task_id).map_or_else(
        || (format!("Task '{task_id}' not found"), true),
        |task| (TaskManager::format_task_detail(task), false),
    )
}

/// Execute the `task_list` tool
pub fn execute_task_list(task_mgr: &TaskManager) -> (String, bool) {
    let tasks = task_mgr.list_tasks();

    if tasks.is_empty() {
        return ("No tasks.".to_string(), false);
    }

    let mut output = String::new();
    for task in tasks {
        output.push_str(&TaskManager::format_task_summary(task));
        output.push('\n');
    }

    let completed = tasks
        .iter()
        .filter(|t| t.status == crate::session::TaskStatus::Completed)
        .count();
    let in_progress = tasks
        .iter()
        .filter(|t| t.status == crate::session::TaskStatus::InProgress)
        .count();
    let pending = tasks
        .iter()
        .filter(|t| t.status == crate::session::TaskStatus::Pending)
        .count();

    let _ = write!(
        output,
        "\n({} total: {} completed, {} in progress, {} pending)",
        tasks.len(),
        completed,
        in_progress,
        pending
    );

    (output, false)
}
