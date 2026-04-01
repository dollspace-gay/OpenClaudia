use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

/// Todo item for task tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
}

/// Global todo list storage
static TODO_LIST: std::sync::LazyLock<Mutex<Vec<TodoItem>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Write/update the todo list
pub(crate) fn execute_todo_write(args: &HashMap<String, Value>) -> (String, bool) {
    let todos_value = match args.get("todos") {
        Some(v) => v,
        None => return ("Missing 'todos' argument".to_string(), true),
    };

    let todos_array = match todos_value.as_array() {
        Some(arr) => arr,
        None => return ("'todos' must be an array".to_string(), true),
    };

    let mut new_todos: Vec<TodoItem> = Vec::new();
    let mut in_progress_count = 0;

    for (i, item) in todos_array.iter().enumerate() {
        let content = match item.get("content").and_then(|v| v.as_str()) {
            Some(c) if c.len() > 2000 => {
                return (
                    format!("Todo {} content exceeds maximum length of 2000 characters", i),
                    true,
                );
            }
            Some(c) => c.to_string(),
            None => return (format!("Todo {} missing 'content' field", i), true),
        };

        let status = match item.get("status").and_then(|v| v.as_str()) {
            Some(s) => {
                if !["pending", "in_progress", "completed"].contains(&s) {
                    return (
                        format!(
                            "Todo {} has invalid status '{}'. Must be: pending, in_progress, completed",
                            i, s
                        ),
                        true,
                    );
                }
                if s == "in_progress" {
                    in_progress_count += 1;
                }
                s.to_string()
            }
            None => return (format!("Todo {} missing 'status' field", i), true),
        };

        let active_form = match item.get("activeForm").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return (format!("Todo {} missing 'activeForm' field", i), true),
        };

        new_todos.push(TodoItem {
            content,
            status,
            active_form,
        });
    }

    // Warn if more than one task is in_progress
    let warning = if in_progress_count > 1 {
        format!(
            "\nWarning: {} tasks marked as in_progress. Best practice is to have only one.",
            in_progress_count
        )
    } else {
        String::new()
    };

    // Update the global todo list
    match TODO_LIST.lock() {
        Ok(mut list) => {
            *list = new_todos.clone();
        }
        Err(e) => return (format!("Failed to update todo list: {}", e), true),
    }

    // Format output
    let completed = new_todos.iter().filter(|t| t.status == "completed").count();
    let in_progress = new_todos
        .iter()
        .filter(|t| t.status == "in_progress")
        .count();
    let pending = new_todos.iter().filter(|t| t.status == "pending").count();

    let mut output = format!(
        "Todo list updated: {} total ({} completed, {} in progress, {} pending){}",
        new_todos.len(),
        completed,
        in_progress,
        pending,
        warning
    );

    // Show current in-progress task if any
    if let Some(current) = new_todos.iter().find(|t| t.status == "in_progress") {
        output.push_str(&format!("\n\nCurrently: {}", current.active_form));
    }

    (output, false)
}

/// Read the current todo list
pub(crate) fn execute_todo_read() -> (String, bool) {
    let todos = match TODO_LIST.lock() {
        Ok(list) => list.clone(),
        Err(e) => return (format!("Failed to read todo list: {}", e), true),
    };

    if todos.is_empty() {
        return ("No todos in list.".to_string(), false);
    }

    let mut output = String::new();
    for (i, todo) in todos.iter().enumerate() {
        let status_icon = match todo.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[>]",
            "pending" => "[ ]",
            _ => "[?]",
        };
        output.push_str(&format!("{}. {} {}\n", i + 1, status_icon, todo.content));
    }

    // Summary
    let completed = todos.iter().filter(|t| t.status == "completed").count();
    let in_progress = todos.iter().filter(|t| t.status == "in_progress").count();
    let pending = todos.iter().filter(|t| t.status == "pending").count();

    output.push_str(&format!(
        "\n({} completed, {} in progress, {} pending)",
        completed, in_progress, pending
    ));

    (output, false)
}

/// Get the current todo list (for external use)
pub fn get_todo_list() -> Vec<TodoItem> {
    TODO_LIST.lock().map(|l| l.clone()).unwrap_or_default()
}

/// Clear the todo list
pub fn clear_todo_list() {
    if let Ok(mut list) = TODO_LIST.lock() {
        list.clear();
    }
}
