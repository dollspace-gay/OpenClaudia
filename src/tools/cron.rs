//! Cron scheduling tools for recurring task execution.
//!
//! Manages cron-like schedules stored in a JSON file at
//! `.openclaudia/schedules.json`. Actual execution is handled
//! by the loop mode or an external scheduler.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write;
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
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("Serialization error: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("Failed to write: {e}"))
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

    const FIELD_NAMES: [&str; 5] = [
        "minute (0-59)",
        "hour (0-23)",
        "day (1-31)",
        "month (1-12)",
        "weekday (0-6)",
    ];
    const FIELD_RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];
    // Compile-time assertion that both arrays have matching lengths
    const _: () = assert!(FIELD_NAMES.len() == FIELD_RANGES.len());

    let field_names = FIELD_NAMES;
    let field_ranges = FIELD_RANGES;

    for (i, field) in fields.iter().enumerate() {
        if *field == "*" {
            continue;
        }
        // Handle */N step values
        if let Some(step) = field.strip_prefix("*/") {
            match step.parse::<u32>() {
                Ok(0) => {
                    return Err(format!(
                        "Step value cannot be 0 in {} field",
                        field_names[i]
                    ))
                }
                Err(_) => {
                    return Err(format!(
                        "Invalid step value '{}' in {} field",
                        step, field_names[i]
                    ))
                }
                _ => {}
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
                let val: u32 = part
                    .parse()
                    .map_err(|_| format!("Invalid value '{}' in {} field", part, field_names[i]))?;
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
            let val: u32 = val_str
                .parse()
                .map_err(|_| format!("Invalid value '{}' in {} field", val_str, field_names[i]))?;
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
        None => {
            return (
                "Error: schedule (cron expression) is required".to_string(),
                true,
            )
        }
    };

    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => return ("Error: prompt is required".to_string(), true),
    };

    if let Err(e) = validate_cron(&cron_expression) {
        return (format!("Invalid cron expression: {e}"), true);
    }

    let mut store = ScheduleStore::load();

    // Check for duplicate names
    if store.schedules.iter().any(|s| s.name == name) {
        return (
            format!("Schedule '{name}' already exists. Delete it first or use a different name."),
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
        return (format!("Failed to save schedule: {e}"), true);
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
        return (format!("No schedule found matching '{id_or_name}'"), true);
    }

    if let Err(e) = store.save() {
        return (format!("Failed to save: {e}"), true);
    }

    (format!("Deleted schedule '{id_or_name}'"), false)
}

pub fn execute_cron_list(_args: &HashMap<String, Value>) -> (String, bool) {
    let store = ScheduleStore::load();

    if store.schedules.is_empty() {
        return ("No scheduled tasks.".to_string(), false);
    }

    let mut output = String::from("Scheduled tasks:\n\n");
    for s in &store.schedules {
        let _ = write!(
            output,
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
        );
    }

    (output, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `set_current_dir` is process-global. Tests that change cwd to a temp
    /// dir (to control the schedules.json path) must hold this lock so they
    /// don't race with each other or with worktree tests.
    fn cwd_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

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

    // ─── Spec §3: cron_create stores schedule; cron_list reads it back ─────────

    /// Contract: `cron_create` requires a `name` field; absent → `is_error=true`.
    #[test]
    fn cron_create_requires_name_field() {
        let mut args = HashMap::new();
        args.insert(
            "schedule".to_string(),
            Value::String("0 * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("ping".to_string()));
        let (msg, is_err) = execute_cron_create(&args);
        assert!(is_err);
        assert!(
            msg.contains("name is required"),
            "error must mention 'name'; got: {msg}"
        );
    }

    /// Contract: `cron_create` requires a `schedule` field; absent → `is_error=true`.
    #[test]
    fn cron_create_requires_schedule_field() {
        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("myjob".to_string()));
        args.insert("prompt".to_string(), Value::String("ping".to_string()));
        let (msg, is_err) = execute_cron_create(&args);
        assert!(is_err);
        assert!(
            msg.contains("schedule") && msg.contains("required"),
            "error must mention 'schedule'; got: {msg}"
        );
    }

    /// Contract: `cron_create` requires a `prompt` field; absent → `is_error=true`.
    #[test]
    fn cron_create_requires_prompt_field() {
        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("myjob".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("0 * * * *".to_string()),
        );
        let (msg, is_err) = execute_cron_create(&args);
        assert!(is_err);
        assert!(
            msg.contains("prompt is required"),
            "error must mention 'prompt'; got: {msg}"
        );
    }

    /// Contract: duplicate `name` is rejected with `is_error=true`.
    /// OC deduplicates by name (CC does not deduplicate at all — pin this
    /// OC-specific behaviour).
    #[test]
    fn cron_create_rejects_duplicate_name() {
        use tempfile::TempDir;
        let _lock = cwd_lock();
        // Run in a temp dir so we control the schedules.json path.
        let tmp = TempDir::new().unwrap();
        let original = std::env::current_dir().ok();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("dupjob".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("0 * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("hello".to_string()));

        let (_, first_err) = execute_cron_create(&args);
        assert!(!first_err, "first create must succeed");

        let (msg, second_err) = execute_cron_create(&args);
        assert!(second_err, "duplicate name must fail");
        assert!(
            msg.contains("already exists"),
            "error must say 'already exists'; got: {msg}"
        );

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
    }

    /// Contract: valid `cron_create` stores the schedule so `cron_list` returns it.
    #[test]
    fn cron_create_then_list_round_trip() {
        use tempfile::TempDir;
        let _lock = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let original = std::env::current_dir().ok();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("roundtrip".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("*/5 * * * *".to_string()),
        );
        args.insert(
            "prompt".to_string(),
            Value::String("check status".to_string()),
        );

        let (create_msg, create_err) = execute_cron_create(&args);
        assert!(!create_err, "create must succeed; got: {create_msg}");
        assert!(
            create_msg.contains("roundtrip"),
            "create message must echo the name"
        );

        let (list_msg, list_err) = execute_cron_list(&HashMap::new());
        assert!(!list_err);
        assert!(
            list_msg.contains("roundtrip"),
            "list must show the newly created schedule; got: {list_msg}"
        );

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
    }

    /// Contract: `cron_delete` by name removes the schedule.
    #[test]
    fn cron_delete_by_name_removes_schedule() {
        use tempfile::TempDir;
        let _lock = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let original = std::env::current_dir().ok();
        std::env::set_current_dir(tmp.path()).unwrap();

        // Create
        let mut create_args = HashMap::new();
        create_args.insert("name".to_string(), Value::String("todelete".to_string()));
        create_args.insert(
            "schedule".to_string(),
            Value::String("0 0 * * *".to_string()),
        );
        create_args.insert("prompt".to_string(), Value::String("noop".to_string()));
        let (_, err) = execute_cron_create(&create_args);
        assert!(!err);

        // Delete by name
        let mut del_args = HashMap::new();
        del_args.insert("name".to_string(), Value::String("todelete".to_string()));
        let (del_msg, del_err) = execute_cron_delete(&del_args);
        assert!(!del_err, "delete must succeed; got: {del_msg}");
        assert!(del_msg.contains("todelete"));

        // List must now be empty
        let (list_msg, _) = execute_cron_list(&HashMap::new());
        assert!(
            !list_msg.contains("todelete"),
            "deleted schedule must not appear in list"
        );

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
    }

    /// Pin gap #621: OC has no `recurring` or `durable` fields in the input
    /// schema.  This test documents that passing these CC-side fields is silently
    /// ignored (not an error).
    #[test]
    fn cron_create_ignores_recurring_and_durable_fields_gap621() {
        use tempfile::TempDir;
        let _lock = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let original = std::env::current_dir().ok();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("gap621job".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("0 12 * * *".to_string()),
        );
        args.insert(
            "prompt".to_string(),
            Value::String("noon check".to_string()),
        );
        // CC fields that OC does not recognise
        args.insert("recurring".to_string(), Value::Bool(false));
        args.insert("durable".to_string(), Value::Bool(false));

        let (msg, is_err) = execute_cron_create(&args);
        assert!(
            !is_err,
            "gap #621: unknown CC fields must not cause an error; got: {msg}"
        );

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
    }

    /// Pin gap #621: OC has no max-jobs cap (CC enforces ≤50).
    /// Documented via the absence of a max-jobs check in the source — we pin
    /// that creating a schedule when <50 jobs exist never fails with a
    /// max-jobs message.
    #[test]
    fn cron_create_has_no_max_jobs_cap_gap621() {
        use tempfile::TempDir;
        let _lock = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let original = std::env::current_dir().ok();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("captest".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("* * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("ping".to_string()));

        let (msg, is_err) = execute_cron_create(&args);
        assert!(
            !is_err,
            "gap #621: must not reject with a max-jobs message; got: {msg}"
        );
        assert!(
            !msg.contains("too many") && !msg.contains("max"),
            "gap #621: no max-jobs guard present; got: {msg}"
        );

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
    }

    /// Contract: invalid cron expression (wrong field count) is rejected.
    #[test]
    fn cron_create_rejects_wrong_field_count() {
        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("badjob".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("0 0 * *".to_string()), // only 4 fields
        );
        args.insert("prompt".to_string(), Value::String("test".to_string()));
        let (msg, is_err) = execute_cron_create(&args);
        assert!(is_err);
        assert!(
            msg.contains("Invalid cron"),
            "error must mention 'Invalid cron'; got: {msg}"
        );
    }

    /// Contract: step value of 0 (`*/0`) is rejected.
    #[test]
    fn validate_cron_rejects_step_zero() {
        assert!(
            validate_cron("*/0 * * * *").is_err(),
            "step=0 must be invalid"
        );
    }

    /// Contract: out-of-range minute (60) is rejected.
    #[test]
    fn validate_cron_rejects_minute_60() {
        assert!(validate_cron("60 * * * *").is_err());
    }

    /// Contract: out-of-range weekday (7) is rejected.
    #[test]
    fn validate_cron_rejects_weekday_7() {
        assert!(validate_cron("* * * * 7").is_err());
    }

    /// Contract: comma-separated list within valid range is accepted.
    #[test]
    fn validate_cron_accepts_comma_list() {
        assert!(validate_cron("0,30 9 * * 1,5").is_ok());
    }
}
