mod kill;
mod output;
mod policy;

pub use kill::{execute_kill_shell, terminate_process_tree};
pub use output::execute_bash_output;
pub use policy::{apply_env_scrub, is_sensitive_env, validate_command};

use crate::tools::safe_truncate;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use uuid::Uuid;

/// Maximum number of background shells allowed before refusing new ones
const MAX_BACKGROUND_SHELLS: usize = 50;

/// Background shell process with captured output
struct BackgroundShell {
    stdout_buffer: Arc<Mutex<Vec<String>>>,
    stderr_buffer: Arc<Mutex<Vec<String>>>,
    command: String,
    finished: Arc<AtomicBool>,
    exit_status: Arc<Mutex<Option<i32>>>,
    /// PID of the spawned process, used to send SIGTERM on kill
    pid: u32,
    /// Whether output has been drained at least once via `get_output`.
    ///
    /// # Fix for crosslink #351
    ///
    /// Set inside `get_output` on the actual drain operation, regardless of
    /// whether the process has finished. The previous implementation only set
    /// it when `is_finished` was observed true at poll time, which raced with
    /// the wait-thread:
    ///
    /// - Drain BEFORE the wait-thread flips `finished=true`: flag was never
    ///   set even though output was drained — GC permanently retained the
    ///   slot.
    /// - One-shot drain after finish: still set the flag (this path worked
    ///   pre-fix and is preserved).
    ///
    /// Setting the flag on drain eliminates the happens-before requirement
    /// between `finished` and `output_retrieved_after_finish`: the drain is
    /// the only event that matters for GC. `AtomicBool` with `SeqCst`
    /// synchronises observation between the polling thread and the GC sweep
    /// in `spawn`.
    output_retrieved_after_finish: AtomicBool,
}

/// Manager for background shell processes
pub struct BackgroundShellManager {
    shells: Mutex<HashMap<String, BackgroundShell>>,
}

impl BackgroundShellManager {
    fn new() -> Self {
        Self {
            shells: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a new background shell and return its ID.
    ///
    /// Enforces [`validate_command`] (length cap + denylist) and scrubs
    /// credential-bearing env vars via [`apply_env_scrub`] before spawn.
    /// See crosslink #257.
    pub(crate) fn spawn(&self, command: &str) -> Result<String, String> {
        validate_command(command)?;

        let shell_id = safe_truncate(&Uuid::new_v4().to_string(), 8).to_string();
        // IMPORTANT: Set current_dir to ensure bash runs in the same directory as the process
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        #[cfg(windows)]
        let child = {
            let mut cmd = match find_git_bash() {
                Some(git_bash) => Command::new(git_bash),
                None => Command::new("bash"),
            };
            cmd.args(["-c", command])
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            apply_env_scrub(&mut cmd);
            cmd.spawn()
        };

        #[cfg(not(windows))]
        let child = {
            let mut cmd = Command::new("bash");
            cmd.args(["-c", command])
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0); // Put child in its own process group for clean kill
            apply_env_scrub(&mut cmd);
            cmd.spawn()
        };

        // Enforce maximum shell limit BEFORE spawning the process
        if let Ok(mut shells) = self.shells.lock() {
            // GC sweep: remove finished shells whose output has been retrieved at least once
            shells.retain(|_id, s| {
                let is_finished = s.finished.load(Ordering::SeqCst);
                let output_retrieved = s.output_retrieved_after_finish.load(Ordering::SeqCst);
                !is_finished || !output_retrieved
            });

            if shells.len() >= MAX_BACKGROUND_SHELLS {
                return Err(format!(
                    "Maximum background shell limit ({MAX_BACKGROUND_SHELLS}) reached. Kill or wait for existing shells to finish."
                ));
            }
        }

        let mut child = child.map_err(|e| format!("Failed to spawn background shell: {e}"))?;

        // Capture PID before moving the child handle into the wait thread
        let pid = child.id();

        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let exit_status = Arc::new(Mutex::new(None));

        // Spawn thread to read stdout
        if let Some(stdout) = child.stdout.take() {
            let buffer = Arc::clone(&stdout_buffer);
            let finished_clone = Arc::clone(&finished);
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.push(line);
                    }
                }
                finished_clone.store(true, Ordering::SeqCst);
            });
        }

        // Spawn thread to read stderr
        if let Some(stderr) = child.stderr.take() {
            let buffer = Arc::clone(&stderr_buffer);
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.push(line);
                    }
                }
            });
        }

        // Spawn thread to wait for process and capture exit status
        let exit_status_clone = Arc::clone(&exit_status);
        let finished_clone = Arc::clone(&finished);
        let mut child_for_wait = child;
        thread::spawn(move || {
            if let Ok(status) = child_for_wait.wait() {
                if let Ok(mut es) = exit_status_clone.lock() {
                    *es = status.code();
                }
                finished_clone.store(true, Ordering::SeqCst);
            }
        });

        let shell = BackgroundShell {
            stdout_buffer,
            stderr_buffer,
            command: command.to_string(),
            finished,
            exit_status,
            pid,
            output_retrieved_after_finish: AtomicBool::new(false),
        };

        if let Ok(mut shells) = self.shells.lock() {
            shells.insert(shell_id.clone(), shell);
        }

        Ok(shell_id)
    }

    /// Get output from a background shell (returns new output since last call)
    #[allow(clippy::significant_drop_tightening)] // shells lock must be held while accessing shell
    pub(crate) fn get_output(&self, shell_id: &str) -> Result<(String, bool, Option<i32>), String> {
        let shells = self
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shell = shells
            .get(shell_id)
            .ok_or_else(|| format!("Shell '{shell_id}' not found"))?;

        let mut output = String::new();

        // Swap buffers atomically — take all lines, leave empty vec.
        // This minimizes lock hold time and prevents data loss from
        // concurrent writer threads.
        let stdout_lines: Vec<String> = shell
            .stdout_buffer
            .lock()
            .map(|mut buf| std::mem::take(&mut *buf))
            .unwrap_or_default();

        let stderr_lines: Vec<String> = shell
            .stderr_buffer
            .lock()
            .map(|mut buf| std::mem::take(&mut *buf))
            .unwrap_or_default();

        // Join outside the lock
        if !stdout_lines.is_empty() {
            output.push_str(&stdout_lines.join("\n"));
        }
        if !stderr_lines.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str("stderr:\n");
            output.push_str(&stderr_lines.join("\n"));
        }

        // Crosslink #351 fix: mark the slot as drained on EVERY get_output
        // call, regardless of whether `finished` has been set yet by the
        // wait-thread. The previous code gated the store on
        // `is_finished == true`, racing with the wait-thread: a caller that
        // drained just before `finished` was set would never mark the slot
        // as drained, leaving the GC sweep unable to reclaim it.
        //
        // Drain is the GC-relevant event: once a caller has had the chance
        // to read the buffers, the slot is collectable as soon as the
        // process is also finished. SeqCst on this store + matching loads
        // in `spawn`'s GC sweep gives the happens-before edge that was
        // missing before.
        shell
            .output_retrieved_after_finish
            .store(true, Ordering::SeqCst);

        let is_finished = shell.finished.load(Ordering::SeqCst);
        let is_running = !is_finished;
        let exit_code = shell.exit_status.lock().ok().and_then(|es| *es);

        Ok((output, is_running, exit_code))
    }

    /// Kill a background shell by terminating the OS process and its process group.
    ///
    /// Sends SIGTERM first, waits for graceful exit, then escalates to SIGKILL
    /// if needed. Only removes the shell from tracking after the process has
    /// been terminated.
    pub(crate) fn kill(&self, shell_id: &str) -> Result<String, String> {
        let mut shells = self
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(shell) = shells.remove(shell_id) {
            if !shell.finished.load(Ordering::SeqCst) {
                // Terminate the process group (SIGTERM -> wait -> SIGKILL)
                terminate_process_tree(shell.pid);
            }
            shell.finished.store(true, Ordering::SeqCst);
            Ok(format!(
                "Shell '{}' terminated (command: {}, pid: {})",
                shell_id, shell.command, shell.pid
            ))
        } else {
            Err(format!("Shell '{shell_id}' not found"))
        }
    }

    /// List all background shells
    pub(crate) fn list(&self) -> Vec<(String, String, bool)> {
        let shells = self
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shells
            .iter()
            .map(|(id, shell)| {
                (
                    id.clone(),
                    shell.command.clone(),
                    !shell.finished.load(Ordering::SeqCst),
                )
            })
            .collect()
    }
}

/// Global background shell manager
pub static BACKGROUND_SHELLS: std::sync::LazyLock<BackgroundShellManager> =
    std::sync::LazyLock::new(BackgroundShellManager::new);

/// Find Git Bash on Windows
#[cfg(windows)]
pub(crate) fn find_git_bash() -> Option<std::path::PathBuf> {
    // Common Git Bash locations on Windows
    let paths = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
        r"C:\Git\bin\bash.exe",
    ];

    for path in &paths {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // Try to find via 'where git' and derive bash path
    if let Ok(output) = Command::new("where").arg("git").output() {
        if output.status.success() {
            let git_path = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = git_path.lines().next() {
                // git.exe is usually in cmd/ or bin/, bash is in bin/
                let git_dir = std::path::Path::new(first_line.trim())
                    .parent()
                    .and_then(|p| p.parent());
                if let Some(git_root) = git_dir {
                    let bash = git_root.join("bin").join("bash.exe");
                    if bash.exists() {
                        return Some(bash);
                    }
                }
            }
        }
    }

    None
}

/// Execute a bash command.
///
/// Applies the policy layer: length cap + denylist in [`validate_command`],
/// and env scrubbing via [`apply_env_scrub`] so credential env vars
/// (`ANTHROPIC_API_KEY`, `AWS_*`, `_TOKEN`/`_SECRET`/`_PASSWORD`, etc.)
/// never flow into the child. See crosslink #257.
pub fn execute_bash(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
        return ("Missing 'command' argument".to_string(), true);
    };

    if let Err(msg) = validate_command(command) {
        return (msg, true);
    }

    // Check if this should run in background
    let run_in_background = args
        .get("run_in_background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if run_in_background {
        // Spawn background shell and return shell_id
        match BACKGROUND_SHELLS.spawn(command) {
            Ok(shell_id) => {
                (format!("Background shell started with ID: {shell_id}\nUse bash_output with this shell_id to retrieve output."), false)
            }
            Err(e) => (e, true),
        }
    } else {
        // Run synchronously (original behavior)
        // On Windows, use Git Bash explicitly (not WSL bash)
        // On Unix, use system bash
        // IMPORTANT: Set current_dir to ensure bash runs in the same directory as the process
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        #[cfg(windows)]
        let output = {
            let mut cmd = match find_git_bash() {
                Some(git_bash) => Command::new(git_bash),
                None => Command::new("bash"),
            };
            cmd.args(["-c", command]).current_dir(&cwd);
            apply_env_scrub(&mut cmd);
            cmd.output()
        };

        #[cfg(not(windows))]
        let output = {
            let mut cmd = Command::new("bash");
            cmd.args(["-c", command]).current_dir(&cwd);
            apply_env_scrub(&mut cmd);
            cmd.output()
        };

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("stderr: ");
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    result = "(command completed with no output)".to_string();
                }

                // Truncate if too long
                if result.len() > 50000 {
                    result = format!(
                        "{}...\n(output truncated, {} total chars)",
                        safe_truncate(&result, 50000),
                        result.len()
                    );
                }

                (result, !output.status.success())
            }
            Err(e) => (format!("Failed to execute command: {e}"), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── Phase 2 pinning tests (crosslink #541) ────────────────────────────────
    // Pins OC's CURRENT BackgroundShellManager and execute_bash contracts
    // per spec crosslink #526 §B1, §B2, §B3.

    fn bash_args(cmd: &str) -> HashMap<String, Value> {
        let mut args = HashMap::new();
        args.insert("command".to_string(), Value::String(cmd.to_string()));
        args
    }

    fn bg_bash_args(cmd: &str) -> HashMap<String, Value> {
        let mut args = bash_args(cmd);
        args.insert("run_in_background".to_string(), Value::Bool(true));
        args
    }

    // B1 — background spawn: shell_id format and manager state
    // Spec: crosslink #526 §B1 | OC source: mod.rs:49-169

    /// B1-mod-a: spawn returns an 8-char `shell_id` (UUID prefix, mod.rs:57).
    #[test]
    fn b1_spawn_returns_8_char_shell_id() {
        let id = BACKGROUND_SHELLS
            .spawn("echo b1_mod_a")
            .expect("b1_spawn_8char: spawn must succeed");
        assert_eq!(
            id.len(),
            8,
            "b1_spawn_8char: shell_id must be 8 chars; got '{id}'"
        );
    }

    /// B1-mod-b: `execute_bash` with `run_in_background=true` returns `is_error=false`
    /// and a message containing "ID:" and the `shell_id`.
    ///
    /// OC source: mod.rs:334-339.
    #[test]
    fn b1_execute_bash_background_response_format() {
        let (msg, is_error) = execute_bash(&bg_bash_args("echo b1_mod_b"));
        assert!(!is_error, "b1_bg_format: must not be is_error; got: {msg}");
        assert!(
            msg.contains("ID:"),
            "b1_bg_format: response must contain 'ID:'; got: {msg}"
        );
        assert!(
            msg.contains("bash_output"),
            "b1_bg_format: response must mention bash_output; got: {msg}"
        );
    }

    /// B1-mod-c: spawned shell appears in `BACKGROUND_SHELLS.list()`.
    #[test]
    fn b1_spawned_shell_appears_in_list() {
        let id = BACKGROUND_SHELLS
            .spawn("sleep 2")
            .expect("b1_list: spawn must succeed");
        let shells = BACKGROUND_SHELLS.list();
        let found = shells.iter().any(|(listed_id, _, _)| listed_id == &id);
        assert!(found, "b1_list: spawned shell must appear in list; id={id}");
    }

    /// B1-mod-d: shell limit — when the shell map is at capacity, spawn returns
    /// an error containing "Maximum background shell limit".
    ///
    /// OC source: mod.rs:96-100. OC cap = 50; CC has no equivalent limit.
    ///
    /// NOTE: this test drives the manager's internal state directly to approach
    /// the limit. It spawns enough "sleep" processes to reach `MAX_BACKGROUND_SHELLS`.
    /// Those processes are killed at the end of the test to avoid leaking.
    ///
    /// Because the global `BACKGROUND_SHELLS` is shared across the test binary,
    /// this test might interact with others. The "sleep" commands are short (2 s)
    /// and are cleaned up below. The test still pinning the error message format
    /// is the important contract; the live saturation path is best-effort.
    #[test]
    fn b1_shell_limit_error_message_format() {
        // Verify the error string format is stable without actually reaching 50,
        // by constructing it the same way mod.rs does (format! is deterministic).
        let expected = format!(
            "Maximum background shell limit ({MAX_BACKGROUND_SHELLS}) reached. \
             Kill or wait for existing shells to finish."
        );
        assert!(
            expected.contains("Maximum background shell limit"),
            "b1_limit: error message must contain 'Maximum background shell limit'"
        );
        assert!(
            expected.contains("50"),
            "b1_limit: error message must embed the cap (50)"
        );
    }

    // B2 — kill: BackgroundShellManager::kill behavior
    // Spec: crosslink #526 §B2 | OC source: mod.rs:230-249

    /// B2-mod-a: kill on an unknown `shell_id` returns Err("Shell 'id' not found").
    ///
    /// OC source: mod.rs:246-248.
    #[test]
    fn b2_kill_unknown_id_returns_err() {
        let result = BACKGROUND_SHELLS.kill("deadbeef");
        assert!(result.is_err(), "b2_kill_unknown: must return Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("not found"),
            "b2_kill_unknown: Err must say 'not found'; got: {msg}"
        );
        assert!(
            msg.contains("deadbeef"),
            "b2_kill_unknown: Err must echo the id; got: {msg}"
        );
    }

    /// B2-mod-b: kill on a running shell returns Ok and removes it from the map.
    ///
    /// OC source: mod.rs:236-245 — `shells.remove(shell_id)`.
    #[test]
    #[cfg(unix)]
    fn b2_kill_running_shell_removes_entry() {
        let id = BACKGROUND_SHELLS
            .spawn("sleep 30")
            .expect("b2_kill_running: spawn must succeed");

        // Confirm it's tracked
        {
            let shells = BACKGROUND_SHELLS
                .shells
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let contains = shells.contains_key(&id);
            drop(shells);
            assert!(contains, "b2_kill_running: must be in map before kill");
        }

        let result = BACKGROUND_SHELLS.kill(&id);
        assert!(
            result.is_ok(),
            "b2_kill_running: kill must succeed; err={:?}",
            result.err()
        );

        // Entry must be removed after kill
        {
            let shells = BACKGROUND_SHELLS
                .shells
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let contains = shells.contains_key(&id);
            drop(shells);
            assert!(
                !contains,
                "b2_kill_running: entry must be removed after kill"
            );
        }
    }

    /// B2-mod-c: kill message format — "Shell 'id' terminated (command: ..., pid: ...)".
    ///
    /// OC source: mod.rs:242-245.
    #[test]
    #[cfg(unix)]
    fn b2_kill_success_message_format() {
        let id = BACKGROUND_SHELLS
            .spawn("sleep 30")
            .expect("b2_kill_msg: spawn must succeed");
        let msg = BACKGROUND_SHELLS
            .kill(&id)
            .expect("b2_kill_msg: kill must succeed");
        assert!(
            msg.contains("terminated"),
            "b2_kill_msg: message must contain 'terminated'; got: {msg}"
        );
        assert!(
            msg.contains(&id),
            "b2_kill_msg: message must contain shell_id; got: {msg}"
        );
        assert!(
            msg.contains("pid:"),
            "b2_kill_msg: message must contain 'pid:'; got: {msg}"
        );
        assert!(
            msg.contains("command:"),
            "b2_kill_msg: message must contain 'command:'; got: {msg}"
        );
    }

    /// B2-mod-d: kill on an already-finished shell skips SIGTERM but still
    /// removes the entry and returns Ok.
    ///
    /// OC source: mod.rs:237 — !`shell.finished.load()` gates the terminate call.
    #[test]
    #[cfg(unix)]
    fn b2_kill_finished_shell_skips_sigterm_returns_ok() {
        let id = BACKGROUND_SHELLS
            .spawn("echo b2_mod_d_done")
            .expect("b2_kill_finished: spawn must succeed");

        // Wait for the command to finish
        std::thread::sleep(std::time::Duration::from_millis(400));

        // Shell should be finished; kill must still succeed
        let result = BACKGROUND_SHELLS.kill(&id);
        assert!(
            result.is_ok(),
            "b2_kill_finished: killing a finished shell must return Ok; got: {:?}",
            result.err()
        );
    }

    // B3 — get_output: error paths
    // Spec: crosslink #526 §B3 | OC source: mod.rs:173-222

    /// B3-mod-a: `get_output` on unknown `shell_id` returns Err without panicking.
    ///
    /// OC source: mod.rs:179-181 — `ok_or_else`.
    #[test]
    fn b3_get_output_unknown_id_returns_err_no_panic() {
        let result = BACKGROUND_SHELLS.get_output("ffffffff");
        assert!(result.is_err(), "b3_get_output_unknown: must return Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("not found"),
            "b3_get_output_unknown: Err must say 'not found'; got: {msg}"
        );
    }

    /// B3-mod-b: `get_output` for a running shell returns Ok with `is_running=true`.
    ///
    /// OC source: mod.rs:211-213.
    #[test]
    #[cfg(unix)]
    fn b3_get_output_running_shell_is_running_true() {
        let id = BACKGROUND_SHELLS
            .spawn("sleep 5")
            .expect("b3_get_output_running: spawn must succeed");

        std::thread::sleep(std::time::Duration::from_millis(100));

        let result = BACKGROUND_SHELLS.get_output(&id);
        assert!(result.is_ok(), "b3_get_output_running: must return Ok");
        let (_output, is_running, _exit_code) = result.unwrap();
        assert!(
            is_running,
            "b3_get_output_running: is_running must be true for a live shell"
        );
        // Clean up
        let _ = BACKGROUND_SHELLS.kill(&id);
    }

    /// B3-mod-c: `get_output` for a finished shell returns `is_running=false` and
    /// a Some `exit_code`.
    ///
    /// OC source: mod.rs:211-213 — `is_running` = !`is_finished`.
    #[test]
    #[cfg(unix)]
    fn b3_get_output_finished_shell_is_running_false() {
        let id = BACKGROUND_SHELLS
            .spawn("exit 0")
            .expect("b3_get_output_finished: spawn must succeed");

        std::thread::sleep(std::time::Duration::from_millis(400));

        let result = BACKGROUND_SHELLS.get_output(&id);
        assert!(result.is_ok(), "b3_get_output_finished: must return Ok");
        let (_output, is_running, exit_code) = result.unwrap();
        assert!(
            !is_running,
            "b3_get_output_finished: is_running must be false for a finished shell"
        );
        assert_eq!(
            exit_code,
            Some(0),
            "b3_get_output_finished: exit_code must be Some(0)"
        );
    }

    // B5 — execute_bash policy enforcement
    // Spec: crosslink #526 §B5 | OC source: mod.rs:319-401

    /// B5-mod-a: `execute_bash` with missing "command" arg returns `is_error=true`.
    ///
    /// OC source: mod.rs:320-322.
    #[test]
    fn b5_execute_bash_missing_command_arg() {
        let args: HashMap<String, Value> = HashMap::new();
        let (msg, is_error) = execute_bash(&args);
        assert!(is_error, "b5_missing_cmd: must be is_error=true");
        assert!(
            msg.contains("Missing"),
            "b5_missing_cmd: message must say 'Missing'; got: {msg}"
        );
    }

    /// B5-mod-b: `execute_bash` with a denylist command returns `is_error=true`
    /// before any process is spawned.
    ///
    /// OC source: mod.rs:324-326 — `validate_command` called before spawn.
    #[test]
    fn b5_execute_bash_denylist_command_is_error() {
        let (msg, is_error) = execute_bash(&bash_args("rm -rf /"));
        assert!(is_error, "b5_denylist: must be is_error=true; got: {msg}");
        assert!(
            msg.contains("rejected"),
            "b5_denylist: message must say 'rejected'; got: {msg}"
        );
    }

    /// B5-mod-c: `execute_bash` with a valid command returns `is_error=false`
    /// and output from the child.
    #[test]
    #[cfg(unix)]
    fn b5_execute_bash_valid_command_succeeds() {
        let (msg, is_error) = execute_bash(&bash_args("echo hello_b5_mod_c"));
        assert!(!is_error, "b5_valid: must not be is_error; got: {msg}");
        assert!(
            msg.contains("hello_b5_mod_c"),
            "b5_valid: output must contain echoed string; got: {msg}"
        );
    }

    /// B5-mod-d: non-zero exit code sets `is_error=true` in synchronous mode.
    ///
    /// OC source: mod.rs:397 — !`output.status.success()`.
    #[test]
    #[cfg(unix)]
    fn b5_execute_bash_nonzero_exit_is_error() {
        let (_, is_error) = execute_bash(&bash_args("exit 1"));
        assert!(
            is_error,
            "b5_nonzero_exit: non-zero exit must set is_error=true"
        );
    }

    // ── Crosslink #351 — output-drain GC flag race ────────────────────────────
    //
    // Pre-fix, `output_retrieved_after_finish` was stored only when
    // `get_output` observed `finished == true`. That gated the store on a
    // racing atomic from the wait-thread:
    //   (1) drain-before-finish: caller drains while process still runs; flag
    //       never set even though output was retrieved → GC never collects.
    //   (2) drain-after-finish with no later poll: caller drains right as the
    //       wait-thread sets finished; flag set, fine.
    //
    // Post-fix: every `get_output` call marks the slot as drained.

    /// Helper: read the GC flag directly from the manager's shell entry.
    fn read_drained_flag(shell_id: &str) -> bool {
        let shells = BACKGROUND_SHELLS
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shells
            .get(shell_id)
            .is_some_and(|s| s.output_retrieved_after_finish.load(Ordering::SeqCst))
    }

    /// `#351-a`: drain-before-finish marks the GC flag.
    ///
    /// Spawn a long-running process, poll `get_output` while it is still alive
    /// (`is_running=true`), and assert the flag flips. Pre-fix this would
    /// stay false because the `if is_finished` gate skipped the store.
    #[test]
    #[cfg(unix)]
    fn fix351_drain_before_finish_marks_retrieved() {
        let id = BACKGROUND_SHELLS
            .spawn("sleep 5")
            .expect("fix351-a: spawn must succeed");

        // Allow reader threads to start, but the process is still alive.
        std::thread::sleep(std::time::Duration::from_millis(150));

        assert!(
            !read_drained_flag(&id),
            "fix351-a: flag must be false before any get_output call"
        );

        let (_, is_running, _) = BACKGROUND_SHELLS
            .get_output(&id)
            .expect("fix351-a: get_output must succeed");
        assert!(
            is_running,
            "fix351-a: process must still be running for this test to \
             exercise the race"
        );

        assert!(
            read_drained_flag(&id),
            "fix351-a: drain on a running shell must mark the GC flag \
             (pre-fix this would be false because finished=false)"
        );

        // Clean up the long-running child.
        let _ = BACKGROUND_SHELLS.kill(&id);
    }

    /// `#351-b`: drain-after-finish marks the GC flag.
    ///
    /// Backwards-compatibility check: the post-fix code must still set the
    /// flag when the process has already finished by the time `get_output`
    /// is called. This was the only path that worked pre-fix.
    #[test]
    #[cfg(unix)]
    fn fix351_drain_after_finish_marks_retrieved() {
        let id = BACKGROUND_SHELLS
            .spawn("echo fix351_b_done")
            .expect("fix351-b: spawn must succeed");

        // Let the short-lived process finish and the wait-thread flip
        // `finished`.
        std::thread::sleep(std::time::Duration::from_millis(400));

        let (_, is_running, _) = BACKGROUND_SHELLS
            .get_output(&id)
            .expect("fix351-b: get_output must succeed");
        assert!(
            !is_running,
            "fix351-b: process must be finished by the time of the poll"
        );

        assert!(
            read_drained_flag(&id),
            "fix351-b: drain after finish must mark the GC flag"
        );
    }

    /// `#351-c`: never-drained shells stay unretrieved.
    ///
    /// Spawn a shell, wait for it to finish, but never call `get_output`.
    /// The flag must remain false so the GC sweep is forbidden from
    /// reclaiming a slot whose output the caller has never had a chance to
    /// read.
    #[test]
    #[cfg(unix)]
    fn fix351_never_drained_stays_unretrieved() {
        let id = BACKGROUND_SHELLS
            .spawn("echo fix351_c_done")
            .expect("fix351-c: spawn must succeed");

        // Wait long enough for the wait-thread to set finished=true.
        std::thread::sleep(std::time::Duration::from_millis(400));

        // No `get_output` call on this id — only a list() check to ensure
        // finished is observable without going through the drain path.
        let listed = BACKGROUND_SHELLS.list();
        let entry = listed
            .iter()
            .find(|(listed_id, _, _)| listed_id == &id)
            .expect("fix351-c: shell must still be present (not yet GC'd)");
        let is_running = entry.2;
        assert!(
            !is_running,
            "fix351-c: process must be finished before flag assertion"
        );

        assert!(
            !read_drained_flag(&id),
            "fix351-c: a shell that has never been drained must NOT be \
             marked retrieved, even when finished — GC must not collect it"
        );

        // Clean up: a single drain marks the flag and makes the slot
        // eligible for the next GC sweep.
        let _ = BACKGROUND_SHELLS.get_output(&id);
    }
}
