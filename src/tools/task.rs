use crate::session::TaskManager;
use crate::tools::args::ToolArgs as _;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::BuildHasher;

/// Execute the `task_create` tool
pub fn execute_task_create<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
    task_mgr: &mut TaskManager,
) -> (String, bool) {
    // crosslink #675: typed accessors. Wording was already canonical
    // ("Missing 'X' argument") so no test churn.
    let subject = match args.arg_string("subject") {
        Ok(s) => s,
        Err(e) => return e.into_tool_error(),
    };
    let description = match args.arg_string("description") {
        Ok(d) => d,
        Err(e) => return e.into_tool_error(),
    };

    let active_form = args
        .arg_str_opt("active_form")
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
pub fn execute_task_update<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
    task_mgr: &mut TaskManager,
) -> (String, bool) {
    let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
        return ("Missing 'task_id' argument".to_string(), true);
    };

    let status = match parse_task_update_status(args.get("status")) {
        Ok(status) => status,
        Err(msg) => return (msg, true),
    };
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
            status,
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

fn parse_task_update_status(
    value: Option<&Value>,
) -> Result<Option<crate::session::TaskUpdateStatus>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(status) = value.as_str() else {
        return Err(
            "Invalid task status '<non-string>'. Must be: pending, in_progress, completed, deleted"
                .to_string(),
        );
    };
    crate::session::TaskUpdateStatus::parse(status)
        .map(Some)
        .ok_or_else(|| {
            format!(
                "Invalid task status '{status}'. Must be: pending, in_progress, completed, deleted"
            )
        })
}

/// Execute the `task_get` tool.
///
/// crosslink #588: a missing `task_id` is a successful lookup of "no such
/// task", not an error — match CC's `TaskGetTool`, which resolves with
/// `null` when the id is unknown. Returning an error here would force the
/// model into a recovery path for what is a legitimate, expected outcome
/// (e.g. polling a task that was deleted). The success payload is the
/// literal JSON `null` so structured consumers can branch on it cheaply.
#[must_use]
pub fn execute_task_get<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
    task_mgr: &TaskManager,
) -> (String, bool) {
    let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
        return ("Missing 'task_id' argument".to_string(), true);
    };

    task_mgr.get_task(task_id).map_or_else(
        || (Value::Null.to_string(), false),
        |task| (TaskManager::format_task_detail(task), false),
    )
}

/// Execute the `task_list` tool
#[must_use]
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
