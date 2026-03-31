use super::BACKGROUND_SHELLS;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

/// Kill a background shell
pub(crate) fn execute_kill_shell(args: &HashMap<String, Value>) -> (String, bool) {
    let shell_id = match args.get("shell_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ("Missing 'shell_id' argument".to_string(), true),
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
pub(crate) fn terminate_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        use std::time::{Duration, Instant};

        let pgid = pid.to_string();
        let neg_pgid = format!("-{}", pid);

        // Step 1: Send SIGTERM to the entire process group
        let _ = Command::new("kill").args(["-TERM", &neg_pgid]).output();

        // Step 2: Wait up to 2 seconds for the process to exit
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut exited = false;
        while Instant::now() < deadline {
            // `kill -0` checks if process exists without sending a signal
            let check = Command::new("kill").args(["-0", &pgid]).output();
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
            let _ = Command::new("kill").args(["-KILL", &neg_pgid]).output();

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
