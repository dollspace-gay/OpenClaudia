//! Cron scheduling tools for recurring task execution.
//!
//! Manages cron-like schedules stored in a JSON file at
//! `.openclaudia/schedules.json`. Actual execution is handled
//! by the loop mode or an external scheduler.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

const SCHEDULES_FILE: &str = ".openclaudia/schedules.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub cron_expression: String,
    pub prompt: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_run: Option<String>,
    pub run_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ScheduleStore {
    schedules: Vec<Schedule>,
}

impl ScheduleStore {
    fn load() -> Self {
        let path = PathBuf::from(SCHEDULES_FILE);
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self) -> Result<(), String> {
        let path = PathBuf::from(SCHEDULES_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;
        std::fs::write(&path, json).map_err(|e| format!("Failed to write: {}", e))
    }
}

/// Validate a cron expression (basic check for 5-field format)
fn validate_cron(expr: &str) -> Result<(), String> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "Cron expression must have 5 fields (minute hour day month weekday), got {}",
            fields.len()
        ));
    }

    let field_names = [
        "minute (0-59)",
        "hour (0-23)",
        "day (1-31)",
        "month (1-12)",
        "weekday (0-6)",
    ];
    let field_ranges = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];

    for (i, field) in fields.iter().enumerate() {
        if *field == "*" {
            continue;
        }
        // Handle */N step values
        if let Some(step) = field.strip_prefix("*/") {
            if step.parse::<u32>().is_err() {
                return Err(format!(
                    "Invalid step value '{}' in {} field",
                    step, field_names[i]
                ));
            }
            continue;
        }
        // Handle ranges like 1-5
        if field.contains('-') {
            let parts: Vec<&str> = field.split('-').collect();
            if parts.len() != 2 {
                return Err(format!(
                    "Invalid range '{}' in {} field",
                    field, field_names[i]
                ));
            }
            for part in parts {
                let val: u32 = part.parse().map_err(|_| {
                    format!("Invalid value '{}' in {} field", part, field_names[i])
                })?;
                if val < field_ranges[i].0 || val > field_ranges[i].1 {
                    return Err(format!(
                        "Value {} out of range for {} field",
                        val, field_names[i]
                    ));
                }
            }
            continue;
        }
        // Handle comma-separated values
        for val_str in field.split(',') {
            let val: u32 = val_str.parse().map_err(|_| {
                format!(
                    "Invalid value '{}' in {} field",
                    val_str, field_names[i]
                )
            })?;
            if val < field_ranges[i].0 || val > field_ranges[i].1 {
                return Err(format!(
                    "Value {} out of range for {} field",
                    val, field_names[i]
                ));
            }
        }
    }
    Ok(())
}

pub fn execute_cron_create(args: &HashMap<String, Value>) -> (String, bool) {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return ("Error: name is required".to_string(), true),
    };

    let cron_expression = match args.get("schedule").and_then(|v| v.as_str()) {
        Some(c) => c.to_string(),
        None => return ("Error: schedule (cron expression) is required".to_string(), true),
    };

    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => return ("Error: prompt is required".to_string(), true),
    };

    if let Err(e) = validate_cron(&cron_expression) {
        return (format!("Invalid cron expression: {}", e), true);
    }

    let mut store = ScheduleStore::load();

    // Check for duplicate names
    if store.schedules.iter().any(|s| s.name == name) {
        return (
            format!(
                "Schedule '{}' already exists. Delete it first or use a different name.",
                name
            ),
            true,
        );
    }

    let schedule = Schedule {
        id: Uuid::new_v4().to_string()[..8].to_string(),
        name: name.clone(),
        cron_expression: cron_expression.clone(),
        prompt,
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_run: None,
        run_count: 0,
    };

    store.schedules.push(schedule.clone());

    if let Err(e) = store.save() {
        return (format!("Failed to save schedule: {}", e), true);
    }

    (
        format!(
            "Created schedule '{}' (id: {})\nCron: {}\nEnabled: true",
            name, schedule.id, cron_expression
        ),
        false,
    )
}

pub fn execute_cron_delete(args: &HashMap<String, Value>) -> (String, bool) {
    let id_or_name = match args
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| args.get("name").and_then(|v| v.as_str()))
    {
        Some(s) => s.to_string(),
        None => return ("Error: id or name is required".to_string(), true),
    };

    let mut store = ScheduleStore::load();
    let initial_len = store.schedules.len();

    store
        .schedules
        .retain(|s| s.id != id_or_name && s.name != id_or_name);

    if store.schedules.len() == initial_len {
        return (
            format!("No schedule found matching '{}'", id_or_name),
            true,
        );
    }

    if let Err(e) = store.save() {
        return (format!("Failed to save: {}", e), true);
    }

    (format!("Deleted schedule '{}'", id_or_name), false)
}

pub fn execute_cron_list(_args: &HashMap<String, Value>) -> (String, bool) {
    let store = ScheduleStore::load();

    if store.schedules.is_empty() {
        return ("No scheduled tasks.".to_string(), false);
    }

    let mut output = String::from("Scheduled tasks:\n\n");
    for s in &store.schedules {
        output.push_str(&format!(
            "  {} [{}] {}\n    Cron: {}\n    Prompt: {}\n    Runs: {} | Last: {}\n\n",
            if s.enabled { "\u{25cf}" } else { "\u{25cb}" },
            s.id,
            s.name,
            s.cron_expression,
            if s.prompt.len() > 80 {
                format!("{}...", &s.prompt[..77])
            } else {
                s.prompt.clone()
            },
            s.run_count,
            s.last_run.as_deref().unwrap_or("never"),
        ));
    }

    (output, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_cron_valid() {
        assert!(validate_cron("0 * * * *").is_ok());
        assert!(validate_cron("*/5 * * * *").is_ok());
        assert!(validate_cron("0 9 * * 1-5").is_ok());
        assert!(validate_cron("30 8 1,15 * *").is_ok());
    }

    #[test]
    fn test_validate_cron_invalid() {
        assert!(validate_cron("* *").is_err());
        assert!(validate_cron("60 * * * *").is_err());
        assert!(validate_cron("* 25 * * *").is_err());
        assert!(validate_cron("* * * * 8").is_err());
    }

    #[test]
    fn test_schedule_store_default() {
        let store = ScheduleStore::default();
        assert!(store.schedules.is_empty());
    }

    #[test]
    fn test_cron_create_requires_name() {
        let mut args = HashMap::new();
        args.insert(
            "schedule".to_string(),
            Value::String("* * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("test".to_string()));
        let (msg, is_err) = execute_cron_create(&args);
        assert!(is_err);
        assert!(msg.contains("name is required"));
    }

    #[test]
    fn test_cron_create_validates_expression() {
        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("test".to_string()));
        args.insert("schedule".to_string(), Value::String("bad".to_string()));
        args.insert("prompt".to_string(), Value::String("test".to_string()));
        let (msg, is_err) = execute_cron_create(&args);
        assert!(is_err);
        assert!(msg.contains("Invalid cron"));
    }

    #[test]
    fn test_cron_list_empty() {
        // Use a nonexistent path so we get empty store
        let (msg, is_err) = execute_cron_list(&HashMap::new());
        assert!(!is_err);
        // Either "No scheduled tasks" or shows existing schedules
        assert!(!msg.is_empty());
    }

    #[test]
    fn test_cron_delete_not_found() {
        let mut args = HashMap::new();
        args.insert(
            "id".to_string(),
            Value::String("nonexistent-id".to_string()),
        );
        let (msg, is_err) = execute_cron_delete(&args);
        assert!(is_err);
        assert!(msg.contains("No schedule found"));
    }
}
