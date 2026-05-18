use super::BACKGROUND_SHELLS;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

/// Kill a background shell
pub fn execute_kill_shell(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(shell_id) = args.get("shell_id").and_then(|v| v.as_str()) else {
        return ("Missing 'shell_id' argument".to_string(), true);
    };

    match BACKGROUND_SHELLS.kill(shell_id) {
        Ok(msg) => (msg, false),
        Err(e) => (e, true),
    }
}

/// Terminate a process and its entire process group.
///
/// On Unix, sends SIGTERM to the process group (negative PID), waits up to
/// 2 seconds for the process to exit, then escalates to SIGKILL if needed.
/// The process must have been spawned with `process_group(0)` for group
/// killing to work correctly.
///
/// On Windows, uses `taskkill /T` which terminates the process tree.
pub fn terminate_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        use std::time::{Duration, Instant};

        let pid_str = pid.to_string();
        let neg_pid_str = format!("-{pid}");

        // Step 1: Send SIGTERM to the entire process group
        let _ = Command::new("kill").args(["-TERM", &neg_pid_str]).output();

        // Step 2: Wait up to 2 seconds for the process to exit
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut exited = false;
        while Instant::now() < deadline {
            // `kill -0` checks if process exists without sending a signal
            let check = Command::new("kill").args(["-0", &pid_str]).output();
            match check {
                Ok(output) if !output.status.success() => {
                    // Process no longer exists
                    exited = true;
                    break;
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }

        // Step 3: If still alive, send SIGKILL to the process group
        if !exited {
            let _ = Command::new("kill").args(["-KILL", &neg_pid_str]).output();

            // Brief wait for SIGKILL to take effect
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(not(unix))]
    {
        // /T kills the process tree, /F forces termination
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── Phase 2 pinning tests (crosslink #541) ────────────────────────────────
    // Pins OC's CURRENT kill_shell contracts per spec crosslink #526 §B2.

    /// B2-kill-a: missing shell_id arg → is_error=true, message contains "Missing".
    ///
    /// OC source: kill.rs:8-10 — arg check fires before any BACKGROUND_SHELLS call.
    #[test]
    fn b2_kill_missing_shell_id_arg() {
        let args: HashMap<String, serde_json::Value> = HashMap::new();
        let (msg, is_error) = execute_kill_shell(&args);
        assert!(is_error, "b2_kill_missing_arg: must be is_error=true");
        assert!(
            msg.contains("Missing"),
            "b2_kill_missing_arg: message must contain 'Missing'; got: {msg}"
        );
    }

    /// B2-kill-b: unknown shell_id → is_error=true, message contains "not found".
    ///
    /// OC source: kill.rs:13-15 via BackgroundShellManager::kill (mod.rs:246-248).
    #[test]
    fn b2_kill_unknown_shell_id() {
        let mut args = HashMap::new();
        args.insert(
            "shell_id".to_string(),
            serde_json::Value::String("deadbeef".to_string()),
        );
        let (msg, is_error) = execute_kill_shell(&args);
        assert!(is_error, "b2_kill_unknown_id: must be is_error=true");
        assert!(
            msg.contains("not found"),
            "b2_kill_unknown_id: message must contain 'not found'; got: {msg}"
        );
    }

    /// B2-kill-c: kill of a running shell returns is_error=false and a
    /// confirmation message containing "terminated" and the shell_id.
    ///
    /// OC source: kill.rs:12-14 (Ok branch), mod.rs:242-245.
    /// Uses BACKGROUND_SHELLS.spawn to create a real process.
    #[test]
    #[cfg(unix)]
    fn b2_kill_running_shell_returns_terminated_message() {
        // Spawn a long-running background shell via the manager
        let shell_id = super::super::BACKGROUND_SHELLS
            .spawn("sleep 30")
            .expect("b2_kill_running: spawn must succeed");

        let mut args = HashMap::new();
        args.insert(
            "shell_id".to_string(),
            serde_json::Value::String(shell_id.clone()),
        );
        let (msg, is_error) = execute_kill_shell(&args);

        assert!(
            !is_error,
            "b2_kill_running: must be is_error=false; got: {msg}"
        );
        assert!(
            msg.contains("terminated"),
            "b2_kill_running: message must contain 'terminated'; got: {msg}"
        );
        assert!(
            msg.contains(&shell_id),
            "b2_kill_running: message must contain the shell_id; got: {msg}"
        );
    }

    /// B2-kill-d: killing the same shell_id twice — second call must return
    /// is_error=true ("not found"), because the entry is removed on first kill.
    ///
    /// OC source: mod.rs:236 — shells.remove(shell_id) evicts the entry.
    #[test]
    #[cfg(unix)]
    fn b2_kill_same_shell_twice_second_is_not_found() {
        let shell_id = super::super::BACKGROUND_SHELLS
            .spawn("sleep 30")
            .expect("b2_kill_twice: spawn must succeed");

        let make_args = |id: &str| {
            let mut args = HashMap::new();
            args.insert(
                "shell_id".to_string(),
                serde_json::Value::String(id.to_string()),
            );
            args
        };

        let (_, first_err) = execute_kill_shell(&make_args(&shell_id));
        assert!(!first_err, "b2_kill_twice: first kill must succeed");

        let (msg2, second_err) = execute_kill_shell(&make_args(&shell_id));
        assert!(
            second_err,
            "b2_kill_twice: second kill must be is_error=true (entry removed)"
        );
        assert!(
            msg2.contains("not found"),
            "b2_kill_twice: second kill must say 'not found'; got: {msg2}"
        );
    }
}
