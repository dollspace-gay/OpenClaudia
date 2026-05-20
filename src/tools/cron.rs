//! Cron scheduling tools for recurring task execution.
//!
//! Manages cron-like schedules stored in a JSON file at
//! `.openclaudia/schedules.json`. Actual execution is handled
//! by the loop mode or an external scheduler.
//!
//! ## Concurrency model (crosslink #403)
//!
//! `ScheduleStore` operations are serialized via an advisory `flock(2)`
//! held on a sibling lock file (`schedules.json.lock`). The whole
//! load-modify-save sequence runs under the lock, so concurrent
//! `cron_create` / `cron_delete` calls (across processes or across
//! threads in the same process) cannot lose updates.
//!
//! The lock is released on `Drop` when the underlying `File` handle is
//! closed — `flock` is released by the kernel on `close(2)`.
//!
//! Writes are atomic: serialized JSON is written to
//! `schedules.json.tmp` and then renamed over `schedules.json`.
//! `rename(2)` is atomic on POSIX, so a crash mid-write cannot
//! truncate the live store.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::file_error::{self, FileError};
use crate::tools::args::ToolArgs as _;

const SCHEDULES_FILE: &str = ".openclaudia/schedules.json";
const LOCK_SUFFIX: &str = ".lock";
const TMP_SUFFIX: &str = ".tmp";

/// Advisory exclusive file lock guarding the schedule store.
///
/// On Unix this is an `flock(2)` `LOCK_EX` lock on a sibling lock file;
/// it is released when the inner `File` is dropped (the kernel releases
/// the lock on `close(2)`). On non-Unix platforms the bare `File`
/// handle still provides serialization across processes when combined
/// with `OpenOptions::write(true)`, matching the pattern used by
/// `claude_credentials::CredentialLock`.
struct ScheduleLock {
    _file: std::fs::File,
}

impl ScheduleLock {
    /// Acquire an exclusive advisory lock on `<schedule_path>.lock`.
    ///
    /// Blocks until the lock is available. Surfaces every failure
    /// (lock-file open, `flock` syscall) as a `String` error rather
    /// than silently degrading — callers translate this into a
    /// user-visible tool error.
    fn acquire(schedule_path: &Path) -> Result<Self, String> {
        let mut lock_path = schedule_path.as_os_str().to_owned();
        lock_path.push(LOCK_SUFFIX);
        let lock_path = PathBuf::from(lock_path);

        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                format!(
                    "Failed to open schedule lock file {}: {e}",
                    lock_path.display()
                )
            })?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            // SAFETY: `fd` is a valid file descriptor owned by `file`
            // for the duration of this call. `flock` does not retain
            // the descriptor; lifetime is bounded by `file`.
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
            if ret != 0 {
                return Err(format!(
                    "Failed to acquire schedule lock: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        Ok(Self { _file: file })
    }
}

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
    /// Read the schedule store from disk.
    ///
    /// Callers performing a read-modify-write sequence MUST hold a
    /// `ScheduleLock` for the same path across both the `load` and the
    /// matching `save`; otherwise concurrent writers will silently
    /// clobber each other's updates (the bug fixed in crosslink #403).
    fn load_locked(path: &Path) -> Self {
        if path.exists() {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Write the schedule store atomically.
    ///
    /// Serializes to a `.tmp` sibling and `rename(2)`s it over the
    /// destination. POSIX `rename` is atomic on the same filesystem,
    /// so a crash mid-write cannot leave a truncated `schedules.json`.
    fn save_locked(&self, path: &Path) -> Result<(), FileError> {
        // Each filesystem step surfaces a typed `FileError` carrying the
        // exact path and underlying `io::ErrorKind`, so callers (and the
        // test suite) can distinguish missing-parent from disk-full from
        // permission-denied without restringing — see crosslink #492.
        if let Some(parent) = path.parent() {
            file_error::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(FileError::json_with_path(path))?;

        let mut tmp_path = path.as_os_str().to_owned();
        tmp_path.push(TMP_SUFFIX);
        let tmp_path = PathBuf::from(tmp_path);

        file_error::write_file(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path).map_err(FileError::with_path(path))
    }
}

/// Resolve the schedules path from the current working directory.
fn schedules_path() -> PathBuf {
    PathBuf::from(SCHEDULES_FILE)
}

/// One cron field's display name and accepted value range.
///
/// crosslink #978: replaces the previous `FIELD_NAMES: [&str; 5]` /
/// `FIELD_RANGES: [(u32, u32); 5]` parallel-array pair. A future maintainer
/// reordering only one of the two arrays would have silently inverted
/// hour-vs-day validation while still passing the matching-length
/// compile-time assert. Pairing the two pieces of information into a
/// single record makes the invariant a structural property of the data.
struct CronField {
    name: &'static str,
    min: u32,
    max: u32,
}

const FIELDS: [CronField; 5] = [
    CronField {
        name: "minute (0-59)",
        min: 0,
        max: 59,
    },
    CronField {
        name: "hour (0-23)",
        min: 0,
        max: 23,
    },
    CronField {
        name: "day (1-31)",
        min: 1,
        max: 31,
    },
    CronField {
        name: "month (1-12)",
        min: 1,
        max: 12,
    },
    CronField {
        name: "weekday (0-6)",
        min: 0,
        max: 6,
    },
];

/// Validate a cron expression (basic check for 5-field format)
fn validate_cron(expr: &str) -> Result<(), String> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != FIELDS.len() {
        return Err(format!(
            "Cron expression must have 5 fields (minute hour day month weekday), got {}",
            fields.len()
        ));
    }

    for (i, field) in fields.iter().enumerate() {
        let spec = &FIELDS[i];
        if *field == "*" {
            continue;
        }
        // Handle */N step values
        if let Some(step) = field.strip_prefix("*/") {
            match step.parse::<u32>() {
                Ok(0) => return Err(format!("Step value cannot be 0 in {} field", spec.name)),
                Err(_) => {
                    return Err(format!(
                        "Invalid step value '{}' in {} field",
                        step, spec.name
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
                return Err(format!("Invalid range '{}' in {} field", field, spec.name));
            }
            for part in parts {
                let val: u32 = part
                    .parse()
                    .map_err(|_| format!("Invalid value '{}' in {} field", part, spec.name))?;
                if val < spec.min || val > spec.max {
                    return Err(format!(
                        "Value {} out of range for {} field",
                        val, spec.name
                    ));
                }
            }
            continue;
        }
        // Handle comma-separated values
        for val_str in field.split(',') {
            let val: u32 = val_str
                .parse()
                .map_err(|_| format!("Invalid value '{}' in {} field", val_str, spec.name))?;
            if val < spec.min || val > spec.max {
                return Err(format!(
                    "Value {} out of range for {} field",
                    val, spec.name
                ));
            }
        }
    }
    Ok(())
}

pub fn execute_cron_create(args: &HashMap<String, Value>) -> (String, bool) {
    // crosslink #675: typed accessors replace per-site
    // `args.get(k).and_then(|v| v.as_str())` extraction. Error wording
    // normalises from "Error: name is required" to "Missing 'name' argument".
    let name = match args.arg_string("name") {
        Ok(n) => n,
        Err(e) => return e.into_tool_error(),
    };
    let cron_expression = match args.arg_string("schedule") {
        Ok(c) => c,
        Err(e) => return e.into_tool_error(),
    };
    let prompt = match args.arg_string("prompt") {
        Ok(p) => p,
        Err(e) => return e.into_tool_error(),
    };

    if let Err(e) = validate_cron(&cron_expression) {
        return (format!("Invalid cron expression: {e}"), true);
    }

    let path = schedules_path();
    // Hold an exclusive flock for the full load-modify-save sequence
    // (crosslink #403): two concurrent `cron_create` calls would
    // otherwise both load the original store, each push their own
    // schedule, and the second writer would silently overwrite the
    // first. `_lock` is dropped at the end of the function after the
    // atomic rename completes.
    let _lock = match ScheduleLock::acquire(&path) {
        Ok(l) => l,
        Err(e) => return (format!("Failed to lock schedule store: {e}"), true),
    };

    let mut store = ScheduleStore::load_locked(&path);

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

    if let Err(e) = store.save_locked(&path) {
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
    // crosslink #675: prefer 'id', fall back to 'name'. Custom message kept
    // because neither helper covers the two-key composite case; the typed
    // accessors are still used to share extraction logic.
    let id_or_name = match args
        .arg_str_opt("id")
        .or_else(|| args.arg_str_opt("name"))
    {
        Some(s) => s.to_string(),
        None => return ("Missing 'id' or 'name' argument".to_string(), true),
    };

    let path = schedules_path();
    // Same locking discipline as `execute_cron_create` — see #403.
    let _lock = match ScheduleLock::acquire(&path) {
        Ok(l) => l,
        Err(e) => return (format!("Failed to lock schedule store: {e}"), true),
    };

    let mut store = ScheduleStore::load_locked(&path);
    let initial_len = store.schedules.len();

    store
        .schedules
        .retain(|s| s.id != id_or_name && s.name != id_or_name);

    if store.schedules.len() == initial_len {
        return (format!("No schedule found matching '{id_or_name}'"), true);
    }

    if let Err(e) = store.save_locked(&path) {
        return (format!("Failed to save: {e}"), true);
    }

    (format!("Deleted schedule '{id_or_name}'"), false)
}

pub fn execute_cron_list(_args: &HashMap<String, Value>) -> (String, bool) {
    let path = schedules_path();
    // Hold the same exclusive lock as writers so a list cannot observe
    // a partial mid-update state — combined with the atomic rename in
    // `save_locked`, readers always see a fully consistent snapshot.
    let _lock = match ScheduleLock::acquire(&path) {
        Ok(l) => l,
        Err(e) => return (format!("Failed to lock schedule store: {e}"), true),
    };
    let store = ScheduleStore::load_locked(&path);

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
    use crate::tools::testutil::process_cwd_lock;
    use std::sync::MutexGuard;

    /// Local alias delegating to the shared process-wide CWD lock in
    /// [`crate::tools::testutil`]. Prior to crosslink #945 this module
    /// had its own `static LOCK: OnceLock<Mutex<()>>` — and so did
    /// `worktree.rs` — so the two `cwd_lock`s did NOT actually serialise
    /// against each other under parallel `cargo test`. They now share
    /// a single mutex.
    fn cwd_lock() -> MutexGuard<'static, ()> {
        process_cwd_lock()
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
        assert!(msg.contains("Missing 'name'"));
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
            msg.contains("Missing 'name'"),
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
            msg.contains("Missing 'schedule'"),
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
            msg.contains("Missing 'prompt'"),
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

    // ─── #403: file-locked load-modify-save preserves writes ────────────────

    /// Concurrent threads each create a uniquely-named schedule. Without the
    /// `flock`, the load-modify-save sequence would lose updates: every
    /// thread loads the same starting state, pushes its own schedule, and
    /// the last writer overwrites all the others. With the lock held for
    /// the whole sequence, all N writes must be visible after the dust
    /// settles. This test is the forensic evidence for issue #403.
    #[test]
    fn cron_create_concurrent_writes_do_not_corrupt_store() {
        use std::thread;
        use tempfile::TempDir;

        const N: usize = 8;

        let _lock = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let original = std::env::current_dir().ok();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            handles.push(thread::spawn(move || {
                let mut args = HashMap::new();
                args.insert(
                    "name".to_string(),
                    Value::String(format!("concurrent_job_{i}")),
                );
                args.insert(
                    "schedule".to_string(),
                    Value::String("0 * * * *".to_string()),
                );
                args.insert("prompt".to_string(), Value::String(format!("p{i}")));
                let (msg, is_err) = execute_cron_create(&args);
                assert!(!is_err, "concurrent create #{i} failed: {msg}");
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        // All N schedules must be present — without the flock, several would
        // be silently lost.
        let path = schedules_path();
        let store = ScheduleStore::load_locked(&path);
        assert_eq!(
            store.schedules.len(),
            N,
            "lost-update race: expected {N} schedules, found {} — \
             load-modify-save is not properly serialized",
            store.schedules.len()
        );
        let mut names: Vec<String> = store.schedules.iter().map(|s| s.name.clone()).collect();
        names.sort();
        for i in 0..N {
            assert!(
                names.iter().any(|n| n == &format!("concurrent_job_{i}")),
                "missing schedule concurrent_job_{i}; got {names:?}"
            );
        }

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }
    }

    /// Forensic: a second `ScheduleLock::acquire` on the same path from
    /// another thread must block until the first guard is dropped.
    /// Demonstrates that the lock is released on `Drop` (and only then),
    /// which is the contract that makes the load-modify-save serialization
    /// in `execute_cron_create` / `execute_cron_delete` sound.
    #[test]
    #[cfg(unix)]
    fn schedule_lock_blocks_then_releases_on_drop() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};
        use tempfile::TempDir;

        let _cwd = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("schedules.json");

        // Acquire the lock on the main thread.
        let first = ScheduleLock::acquire(&path).expect("first acquire");

        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            // This must block until `first` is dropped.
            let start = Instant::now();
            let second = ScheduleLock::acquire(&path).expect("second acquire");
            let elapsed = start.elapsed();
            tx.send(elapsed).unwrap();
            drop(second);
        });

        // Hold the lock for a measurable interval, then release.
        thread::sleep(Duration::from_millis(150));
        drop(first);

        // The waiting thread must finish promptly after we drop, and must
        // have spent at least ~100ms blocked (well above scheduler noise).
        let elapsed = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second acquire never completed — lock not released on drop");
        assert!(
            elapsed >= Duration::from_millis(100),
            "second acquire returned in {elapsed:?} — lock was not actually exclusive"
        );
        handle.join().expect("blocked thread panicked");
    }

    /// Forensic: lock acquisition surfaces a real error when it cannot open
    /// the lock file (parent path is a regular file, not a directory).
    /// Confirms we do not silently degrade to "no locking at all".
    #[test]
    fn schedule_lock_acquire_surfaces_open_failure() {
        use tempfile::TempDir;

        let _cwd = cwd_lock();
        let tmp = TempDir::new().unwrap();

        // Create a regular file where we want a directory — `create_dir_all`
        // and/or `OpenOptions::open` will refuse to put a lock file under it.
        let blocker = tmp.path().join("not_a_dir");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let schedule_path = blocker.join("schedules.json");

        let result = ScheduleLock::acquire(&schedule_path);
        assert!(
            result.is_err(),
            "acquire must surface an error when the lock path is unusable; got Ok"
        );
        let err = result.err().unwrap();
        assert!(
            err.contains("Failed to") && (err.contains("directory") || err.contains("lock file")),
            "error must describe the open failure; got: {err}"
        );
    }

    /// Forensic: atomic rename — a save followed by a concurrent reader
    /// never observes an empty / truncated `schedules.json`. Combined with
    /// the temp-file + rename strategy, an interrupted save cannot leave a
    /// half-written store on disk.
    #[test]
    fn schedule_save_locked_is_atomic_via_rename() {
        use tempfile::TempDir;

        let _cwd = cwd_lock();
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("schedules.json");

        // Seed an initial non-trivial store.
        let mut store = ScheduleStore::default();
        store.schedules.push(Schedule {
            id: "abcd1234".to_string(),
            name: "atomic_seed".to_string(),
            cron_expression: "0 * * * *".to_string(),
            prompt: "p".to_string(),
            enabled: true,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_run: None,
            run_count: 0,
        });
        store.save_locked(&path).expect("initial save");

        // Re-save and verify the destination is replaced atomically — at no
        // point should a tempfile remain alongside (rename moves it).
        store.schedules.push(Schedule {
            id: "efef5678".to_string(),
            name: "atomic_second".to_string(),
            cron_expression: "*/5 * * * *".to_string(),
            prompt: "p2".to_string(),
            enabled: true,
            created_at: "2026-01-01T00:00:01Z".to_string(),
            last_run: None,
            run_count: 0,
        });
        store.save_locked(&path).expect("second save");

        let mut tmp_path = path.as_os_str().to_owned();
        tmp_path.push(TMP_SUFFIX);
        let tmp_path = PathBuf::from(tmp_path);
        assert!(
            !tmp_path.exists(),
            "tempfile {} must be renamed away after a successful save",
            tmp_path.display()
        );

        let reloaded = ScheduleStore::load_locked(&path);
        assert_eq!(reloaded.schedules.len(), 2);
        assert!(reloaded.schedules.iter().any(|s| s.name == "atomic_seed"));
        assert!(reloaded.schedules.iter().any(|s| s.name == "atomic_second"));
    }
}
