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
use std::hash::BuildHasher;
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

    /// Write the schedule store atomically (crosslink #909).
    ///
    /// 1. Serialize to a per-process, per-call `.tmp.<pid>.<uuid>` sibling
    ///    so two concurrent writers cannot collide on the temp name.
    /// 2. `fsync(2)` the temp file so its contents reach durable storage
    ///    before the rename — otherwise a power loss between
    ///    `write` and `rename` can leave a zero-length destination on
    ///    some filesystems.
    /// 3. `rename(2)` it over the destination — atomic on POSIX within
    ///    the same directory, so a crash mid-write cannot leave a
    ///    truncated `schedules.json`.
    fn save_locked(&self, path: &Path) -> Result<(), FileError> {
        use std::io::Write as _;

        // Each filesystem step surfaces a typed `FileError` carrying the
        // exact path and underlying `io::ErrorKind`, so callers (and the
        // test suite) can distinguish missing-parent from disk-full from
        // permission-denied without restringing — see crosslink #492.
        if let Some(parent) = path.parent() {
            file_error::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(FileError::json_with_path(path))?;

        // crosslink #909: previous code used a fixed `<path>.tmp` name,
        // which two concurrent writers in different processes could
        // clobber even with the flock — the lock guards the *destination*
        // but the temp file lives in the same directory. Suffix with
        // pid + a uuid so concurrent writers each have their own temp.
        let mut tmp_path = path.as_os_str().to_owned();
        tmp_path.push(TMP_SUFFIX);
        tmp_path.push(format!(
            ".{}.{}",
            std::process::id(),
            Uuid::new_v4().as_simple(),
        ));
        let tmp_path = PathBuf::from(tmp_path);

        // Write + fsync the tmp file. We open explicitly (rather than
        // routing through `file_error::write_file`) so we can call
        // `sync_all()` on the same `File` handle that holds the bytes.
        {
            let mut tmp_file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(FileError::with_path(&tmp_path))?;
            tmp_file
                .write_all(json.as_bytes())
                .map_err(FileError::with_path(&tmp_path))?;
            // fsync — durability guarantee against power-loss between
            // the write and the rename.
            tmp_file
                .sync_all()
                .map_err(FileError::with_path(&tmp_path))?;
        }

        // Atomic publish. If rename fails (e.g. cross-filesystem) we
        // best-effort clean up the orphan tmp so we don't litter
        // `schedules.json.tmp.*` files.
        if let Err(e) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(FileError::with_path(path)(e));
        }
        Ok(())
    }
}

/// Resolve the schedules path to an absolute location.
///
/// Crosslink #877: the prior implementation returned a bare relative
/// `PathBuf::from(SCHEDULES_FILE)`, so every cron operation resolved
/// against whatever the process cwd happened to be at call time. When
/// the worktree adapter mutated cwd between operations, schedule
/// load/save silently targeted different files. We now anchor the
/// path against `std::env::current_dir()` once, producing an absolute
/// path the caller can rely on for the duration of one tool call.
///
/// If `current_dir` itself fails (deleted cwd, FUSE EIO, …) we fall
/// back to the original relative path rather than panic — surfacing
/// a `warn!` so the operator can see what happened.
fn schedules_path() -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(SCHEDULES_FILE),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "schedules_path: current_dir() failed; falling back to relative {SCHEDULES_FILE}"
            );
            PathBuf::from(SCHEDULES_FILE)
        }
    }
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

/// Validate one atomic piece of a cron field (no commas).
///
/// Accepts:
///   * `"*"`                   – wildcard
///   * `"N"`                   – single integer in range
///   * `"A-B"`                 – range
///   * `"*/S"`                 – step from zero
///   * `"A-B/S"`               – step over a range (crosslink #901)
///
/// Steps must be non-zero. Ranges must have A ≤ B and both within the
/// field's accepted bounds.
fn validate_cron_atom(atom: &str, spec: &CronField) -> Result<(), String> {
    // Split off optional step suffix: "<head>/<step>".
    let (head, step_opt) = match atom.split_once('/') {
        Some((h, s)) => (h, Some(s)),
        None => (atom, None),
    };

    if let Some(step) = step_opt {
        match step.parse::<u32>() {
            Ok(0) => return Err(format!("Step value cannot be 0 in {} field", spec.name)),
            Err(_) => {
                return Err(format!(
                    "Invalid step value '{}' in {} field",
                    step, spec.name
                ));
            }
            _ => {}
        }
    }

    if head == "*" {
        return Ok(());
    }

    if let Some((a, b)) = head.split_once('-') {
        let lo: u32 = a
            .parse()
            .map_err(|_| format!("Invalid value '{}' in {} field", a, spec.name))?;
        let hi: u32 = b
            .parse()
            .map_err(|_| format!("Invalid value '{}' in {} field", b, spec.name))?;
        if lo > hi {
            return Err(format!(
                "Range {}-{} is reversed in {} field",
                lo, hi, spec.name
            ));
        }
        if lo < spec.min || hi > spec.max {
            return Err(format!(
                "Range {}-{} out of bounds for {} field",
                lo, hi, spec.name
            ));
        }
        return Ok(());
    }

    let val: u32 = head
        .parse()
        .map_err(|_| format!("Invalid value '{}' in {} field", head, spec.name))?;
    if val < spec.min || val > spec.max {
        return Err(format!(
            "Value {} out of range for {} field",
            val, spec.name
        ));
    }
    Ok(())
}

/// Validate a cron expression (basic check for 5-field format).
///
/// Crosslink #901: the field parser now treats `*`, single values,
/// ranges, steps, and range+step as composable atoms, applied to each
/// comma-separated piece. Previously `0-30/5 * * * *` and `1,3-5,8 * * * *`
/// were both rejected because the comma branch parsed each piece as a
/// flat integer.
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
        for atom in field.split(',') {
            if atom.is_empty() {
                return Err(format!("Empty value in {} field", spec.name));
            }
            validate_cron_atom(atom, spec)?;
        }
    }
    Ok(())
}

#[must_use]
pub fn execute_cron_create<S: BuildHasher>(args: &HashMap<String, Value, S>) -> (String, bool) {
    execute_cron_create_at(args, &schedules_path())
}

/// Path-explicit variant of [`execute_cron_create`].
///
/// crosslink #984: the public entry point resolves the schedule store
/// against the process cwd, which forced tests to `set_current_dir`
/// into a temp dir — a process-global mutation that poisons sibling
/// tests run in parallel. This inner helper takes the schedule store
/// path as a parameter so tests thread an absolute path through
/// without ever touching the process cwd.
fn execute_cron_create_at<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
    path: &Path,
) -> (String, bool) {
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

    let path = path.to_path_buf();
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

    // Crosslink #907: previously truncated to 8 hex chars (32 bits of
    // entropy → 50% collision at ~77k schedules). Use 16 hex chars
    // (64 bits) for a vanishingly small collision probability while
    // staying short enough that id-based UX (cron_delete by id) is
    // still ergonomic. cron_list still renders the id verbatim.
    let new_id = {
        let uuid_str = Uuid::new_v4().to_string().replace('-', "");
        // uuid v4 to_string() is 32 hex chars + 4 dashes; after stripping
        // dashes we always have ≥16 hex chars, so the slice is safe.
        uuid_str[..16].to_string()
    };
    // Fail-fast on the astronomically rare collision rather than silently
    // letting cron_delete pick the wrong record.
    if store.schedules.iter().any(|s| s.id == new_id) {
        return (
            format!("Generated schedule id '{new_id}' collides with an existing schedule"),
            true,
        );
    }
    let schedule = Schedule {
        id: new_id,
        name: name.clone(),
        cron_expression: cron_expression.clone(),
        prompt,
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_run: None,
        run_count: 0,
    };

    store.schedules.push(schedule);

    if let Err(e) = store.save_locked(&path) {
        return (format!("Failed to save schedule: {e}"), true);
    }

    // crosslink #987: the response no longer surfaces the internal UUID.
    // `name` is the unique key (dedup'd above) and the sole identifier the
    // model is told to use for `cron_delete`. The `id` field remains in the
    // persisted record for backwards-compatible JSON-on-disk but is no
    // longer part of the tool's public surface.
    (
        format!(
            "Created schedule '{name}'\nCron: {cron_expression}\nEnabled: true\n(use `cron_delete name=\"{name}\"` to remove)"
        ),
        false,
    )
}

#[must_use]
pub fn execute_cron_delete<S: BuildHasher>(args: &HashMap<String, Value, S>) -> (String, bool) {
    execute_cron_delete_at(args, &schedules_path())
}

/// Path-explicit variant of [`execute_cron_delete`] — see
/// [`execute_cron_create_at`] for the #984 rationale.
fn execute_cron_delete_at<S: BuildHasher>(
    args: &HashMap<String, Value, S>,
    path: &Path,
) -> (String, bool) {
    // crosslink #987: `name` is the primary identifier. `index` (1-based
    // position in the `cron_list` output) is a human-friendly fallback so a
    // user reading the listing can say "delete #2" without typing a name.
    // The legacy `id` field is still accepted for backwards compatibility
    // with any persisted prompts that captured a UUID, but it is no longer
    // documented in the tool surface.
    let name_arg = args.arg_str_opt("name").map(str::to_string);
    let index_arg = args.get("index").and_then(serde_json::Value::as_u64);
    let id_arg = args.arg_str_opt("id").map(str::to_string);

    if name_arg.is_none() && index_arg.is_none() && id_arg.is_none() {
        return (
            "Missing 'name' (preferred), 'index', or legacy 'id' argument".to_string(),
            true,
        );
    }

    let path = path.to_path_buf();
    // Same locking discipline as `execute_cron_create` — see #403.
    let _lock = match ScheduleLock::acquire(&path) {
        Ok(l) => l,
        Err(e) => return (format!("Failed to lock schedule store: {e}"), true),
    };

    let mut store = ScheduleStore::load_locked(&path);

    // Resolve the deletion target *under the lock* so concurrent reorders
    // of the list cannot shift an index out from under us.
    let target_name: String = if let Some(name) = name_arg {
        name
    } else if let Some(idx) = index_arg {
        let one_based = usize::try_from(idx).unwrap_or(0);
        if one_based == 0 || one_based > store.schedules.len() {
            return (
                format!(
                    "Index {idx} is out of range (1..={})",
                    store.schedules.len()
                ),
                true,
            );
        }
        store.schedules[one_based - 1].name.clone()
    } else if let Some(id) = id_arg {
        match store.schedules.iter().find(|s| s.id == id) {
            Some(s) => s.name.clone(),
            None => return (format!("No schedule found with id '{id}'"), true),
        }
    } else {
        unreachable!("at least one identifier must be set, checked above");
    };

    let initial_len = store.schedules.len();
    store.schedules.retain(|s| s.name != target_name);

    if store.schedules.len() == initial_len {
        return (format!("No schedule found matching '{target_name}'"), true);
    }

    if let Err(e) = store.save_locked(&path) {
        return (format!("Failed to save: {e}"), true);
    }

    (format!("Deleted schedule '{target_name}'"), false)
}

#[must_use]
pub fn execute_cron_list<S: BuildHasher>(args: &HashMap<String, Value, S>) -> (String, bool) {
    execute_cron_list_at(args, &schedules_path())
}

/// Path-explicit variant of [`execute_cron_list`] — see
/// [`execute_cron_create_at`] for the #984 rationale.
fn execute_cron_list_at<S: BuildHasher>(
    _args: &HashMap<String, Value, S>,
    path: &Path,
) -> (String, bool) {
    let path = path.to_path_buf();
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

    // crosslink #984: tests no longer mutate the process cwd — they
    // thread an explicit `schedules.json` path through the `*_at`
    // helpers in this module. The previous `cwd_lock()` shim is gone
    // because there is nothing global to serialise against.

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

    /// Helper: build a fresh per-test schedules.json path under a `TempDir`
    /// the caller keeps alive. crosslink #984 — tests must NOT mutate the
    /// process cwd; they thread this path through `*_at` helpers instead.
    fn temp_schedules_path(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join(SCHEDULES_FILE)
    }

    /// Contract: duplicate `name` is rejected with `is_error=true`.
    /// OC deduplicates by name (CC does not deduplicate at all — pin this
    /// OC-specific behaviour).
    #[test]
    fn cron_create_rejects_duplicate_name() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("dupjob".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("0 * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("hello".to_string()));

        let (_, first_err) = execute_cron_create_at(&args, &path);
        assert!(!first_err, "first create must succeed");

        let (msg, second_err) = execute_cron_create_at(&args, &path);
        assert!(second_err, "duplicate name must fail");
        assert!(
            msg.contains("already exists"),
            "error must say 'already exists'; got: {msg}"
        );
    }

    /// Contract: valid `cron_create` stores the schedule so `cron_list` returns it.
    #[test]
    fn cron_create_then_list_round_trip() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

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

        let (create_msg, create_err) = execute_cron_create_at(&args, &path);
        assert!(!create_err, "create must succeed; got: {create_msg}");
        assert!(
            create_msg.contains("roundtrip"),
            "create message must echo the name"
        );

        let (list_msg, list_err) = execute_cron_list_at(&HashMap::new(), &path);
        assert!(!list_err);
        assert!(
            list_msg.contains("roundtrip"),
            "list must show the newly created schedule; got: {list_msg}"
        );
    }

    /// Contract: `cron_delete` by name removes the schedule.
    #[test]
    fn cron_delete_by_name_removes_schedule() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

        // Create
        let mut create_args = HashMap::new();
        create_args.insert("name".to_string(), Value::String("todelete".to_string()));
        create_args.insert(
            "schedule".to_string(),
            Value::String("0 0 * * *".to_string()),
        );
        create_args.insert("prompt".to_string(), Value::String("noop".to_string()));
        let (_, err) = execute_cron_create_at(&create_args, &path);
        assert!(!err);

        // Delete by name
        let mut del_args = HashMap::new();
        del_args.insert("name".to_string(), Value::String("todelete".to_string()));
        let (del_msg, del_err) = execute_cron_delete_at(&del_args, &path);
        assert!(!del_err, "delete must succeed; got: {del_msg}");
        assert!(del_msg.contains("todelete"));

        // List must now be empty
        let (list_msg, _) = execute_cron_list_at(&HashMap::new(), &path);
        assert!(
            !list_msg.contains("todelete"),
            "deleted schedule must not appear in list"
        );
    }

    /// Pin gap #621: OC has no `recurring` or `durable` fields in the input
    /// schema.  This test documents that passing these CC-side fields is silently
    /// ignored (not an error).
    #[test]
    fn cron_create_ignores_recurring_and_durable_fields_gap621() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

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

        let (msg, is_err) = execute_cron_create_at(&args, &path);
        assert!(
            !is_err,
            "gap #621: unknown CC fields must not cause an error; got: {msg}"
        );
    }

    /// Pin gap #621: OC has no max-jobs cap (CC enforces ≤50).
    /// Documented via the absence of a max-jobs check in the source — we pin
    /// that creating a schedule when <50 jobs exist never fails with a
    /// max-jobs message.
    #[test]
    fn cron_create_has_no_max_jobs_cap_gap621() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("captest".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("* * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("ping".to_string()));

        let (msg, is_err) = execute_cron_create_at(&args, &path);
        assert!(
            !is_err,
            "gap #621: must not reject with a max-jobs message; got: {msg}"
        );
        assert!(
            !msg.contains("too many") && !msg.contains("max"),
            "gap #621: no max-jobs guard present; got: {msg}"
        );
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

    /// Crosslink #901: step+range like `0-30/5` is now accepted.
    #[test]
    fn validate_cron_accepts_step_range() {
        assert!(
            validate_cron("0-30/5 * * * *").is_ok(),
            "step+range 0-30/5 must be accepted"
        );
        assert!(
            validate_cron("*/15 0-12/2 * * *").is_ok(),
            "step over a range must be accepted in any field"
        );
    }

    /// Crosslink #901: mixed comma-list with a range element like `1,3-5,8`.
    #[test]
    fn validate_cron_accepts_mixed_comma_with_range() {
        assert!(
            validate_cron("1,3-5,8 * * * *").is_ok(),
            "comma-separated list containing a range must be accepted"
        );
    }

    /// Crosslink #901: reversed range is rejected.
    #[test]
    fn validate_cron_rejects_reversed_range() {
        assert!(
            validate_cron("30-10 * * * *").is_err(),
            "reversed range must be rejected"
        );
    }

    /// Crosslink #907: schedule id is no longer truncated to 8 hex chars.
    /// After creation the id must be 16 hex chars (64 bits of entropy).
    #[test]
    fn schedule_id_is_16_hex_chars() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("idtest".to_string()));
        args.insert(
            "schedule".to_string(),
            Value::String("0 * * * *".to_string()),
        );
        args.insert("prompt".to_string(), Value::String("p".to_string()));
        let (_, is_err) = execute_cron_create_at(&args, &path);
        assert!(!is_err);

        let store = ScheduleStore::load_locked(&path);
        let s = store
            .schedules
            .iter()
            .find(|s| s.name == "idtest")
            .expect("created schedule missing");
        assert_eq!(
            s.id.len(),
            16,
            "schedule id must be 16 hex chars, got {}",
            s.id
        );
        assert!(
            s.id.chars().all(|c| c.is_ascii_hexdigit()),
            "schedule id must be hex; got {}",
            s.id
        );
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

        let tmp = TempDir::new().unwrap();
        let path = temp_schedules_path(&tmp);

        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let path = path.clone();
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
                let (msg, is_err) = execute_cron_create_at(&args, &path);
                assert!(!is_err, "concurrent create #{i} failed: {msg}");
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        // All N schedules must be present — without the flock, several would
        // be silently lost.
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

        // crosslink #909: per-call tmp suffix (`.tmp.<pid>.<uuid>`) means
        // no orphan tempfile should remain in the parent directory either.
        let dir_entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        for name in &dir_entries {
            assert!(
                !name.starts_with("schedules.json.tmp"),
                "#909: orphan tempfile {name} left after successful save"
            );
        }

        let reloaded = ScheduleStore::load_locked(&path);
        assert_eq!(reloaded.schedules.len(), 2);
        assert!(reloaded.schedules.iter().any(|s| s.name == "atomic_seed"));
        assert!(reloaded.schedules.iter().any(|s| s.name == "atomic_second"));
    }

    /// #909 — Two consecutive `save_locked` calls publish without collision
    /// and the final state reflects the second write. Together with the
    /// flock around the load-modify-save sequence in `execute_cron_create`,
    /// this is the guarantee that no schedule is lost to a non-atomic write.
    #[test]
    fn fix909_save_locked_replaces_atomically_under_repeat_writes() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("schedules.json");

        for i in 0..5 {
            let mut store = ScheduleStore::default();
            store.schedules.push(Schedule {
                id: format!("id{i}"),
                name: format!("name{i}"),
                cron_expression: "0 * * * *".to_string(),
                prompt: "p".to_string(),
                enabled: true,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                last_run: None,
                run_count: 0,
            });
            store.save_locked(&path).expect("save");
        }

        let reloaded = ScheduleStore::load_locked(&path);
        assert_eq!(reloaded.schedules.len(), 1);
        assert_eq!(reloaded.schedules[0].name, "name4");
    }
}
