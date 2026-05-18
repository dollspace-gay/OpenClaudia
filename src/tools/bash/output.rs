use super::BACKGROUND_SHELLS;
use crate::tools::safe_truncate;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Retrieve output from a background shell
pub fn execute_bash_output(args: &HashMap<String, Value>) -> (String, bool) {
    // If no shell_id provided, list all background shells
    let Some(shell_id) = args.get("shell_id").and_then(|v| v.as_str()) else {
        let shells = BACKGROUND_SHELLS.list();
        if shells.is_empty() {
            return ("No background shells running.".to_string(), false);
        }
        let mut result = format!("Background shells ({}):\n", shells.len());
        for (id, command, is_running) in shells {
            let status = if is_running { "running" } else { "finished" };
            let cmd_preview = if command.len() > 50 {
                format!("{}...", safe_truncate(&command, 50))
            } else {
                command
            };
            let _ = writeln!(result, "  {id} [{status}]: {cmd_preview}");
        }
        return (result, false);
    };

    match BACKGROUND_SHELLS.get_output(shell_id) {
        Ok((output, is_running, exit_code)) => {
            let status = if is_running {
                "running".to_string()
            } else {
                exit_code.map_or_else(
                    || "finished".to_string(),
                    |code| format!("finished (exit code: {code})"),
                )
            };

            let result = if output.is_empty() {
                format!("Status: {status}\n(no new output)")
            } else {
                format!("Status: {status}\n\n{output}")
            };

            (result, false)
        }
        Err(e) => (e, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── Phase 2 pinning tests (crosslink #541) ────────────────────────────────
    // Pins OC's CURRENT bash_output contracts per spec crosslink #526 §B1 + §B3.

    /// B3-output-a: unknown shell_id → is_error=true, message contains "not found".
    ///
    /// OC source: output.rs:47 — Err branch returns (e, true).
    /// No panic on unknown ID (mod.rs:178-181 uses ok_or_else).
    /// CC has no bash_output RPC; this path is OC-specific.
    #[test]
    fn b3_output_unknown_shell_id_is_error() {
        let mut args = HashMap::new();
        args.insert(
            "shell_id".to_string(),
            serde_json::Value::String("00000000".to_string()),
        );
        let (msg, is_error) = execute_bash_output(&args);
        assert!(
            is_error,
            "b3_output_unknown_id: must be is_error=true; got: {msg}"
        );
        assert!(
            msg.contains("not found"),
            "b3_output_unknown_id: message must contain 'not found'; got: {msg}"
        );
    }

    /// B3-output-b: the supplied shell_id is echoed verbatim in the error message.
    ///
    /// OC source: mod.rs:180 — format!("Shell '{{shell_id}}' not found").
    #[test]
    fn b3_output_error_echoes_shell_id() {
        let bogus = "cafebabe";
        let mut args = HashMap::new();
        args.insert(
            "shell_id".to_string(),
            serde_json::Value::String(bogus.to_string()),
        );
        let (msg, is_error) = execute_bash_output(&args);
        assert!(is_error, "b3_output_echo_id: must be is_error=true");
        assert!(
            msg.contains(bogus),
            "b3_output_echo_id: message must contain '{bogus}'; got: {msg}"
        );
    }

    /// B1-output-a: no shell_id arg → listing path, is_error=false.
    ///
    /// OC source: output.rs:10-26 — absent shell_id triggers BACKGROUND_SHELLS.list().
    /// Result is either "No background shells running." or "Background shells (N):...".
    #[test]
    fn b1_output_no_arg_returns_listing_not_error() {
        let args: HashMap<String, serde_json::Value> = HashMap::new();
        let (msg, is_error) = execute_bash_output(&args);
        assert!(
            !is_error,
            "b1_output_list: listing must not be is_error=true; got: {msg}"
        );
        assert!(
            msg.contains("Background shells") || msg.contains("No background shells"),
            "b1_output_list: must describe shell list state; got: {msg}"
        );
    }

    /// B1-output-b: poll of a running shell starts with "Status:" (format pin).
    ///
    /// OC source: output.rs:39-44 — format!("Status: {{status}}\n\n{{output}}")
    /// or format!("Status: {{status}}\n(no new output)").
    #[test]
    #[cfg(unix)]
    fn b1_output_status_line_format() {
        let shell_id = super::super::BACKGROUND_SHELLS
            .spawn("sleep 5")
            .expect("b1_output_status: spawn must succeed");

        std::thread::sleep(std::time::Duration::from_millis(100));

        let mut args = HashMap::new();
        args.insert("shell_id".to_string(), serde_json::Value::String(shell_id));
        let (msg, is_error) = execute_bash_output(&args);
        assert!(!is_error, "b1_output_status: poll must succeed; got: {msg}");
        assert!(
            msg.starts_with("Status:"),
            "b1_output_status: response must begin with 'Status:'; got: {msg}"
        );
    }

    /// B1-output-c: running shell reports "running" in the status line.
    #[test]
    #[cfg(unix)]
    fn b1_output_running_shell_status_is_running() {
        let shell_id = super::super::BACKGROUND_SHELLS
            .spawn("sleep 5")
            .expect("b1_output_running: spawn must succeed");

        std::thread::sleep(std::time::Duration::from_millis(100));

        let mut args = HashMap::new();
        args.insert("shell_id".to_string(), serde_json::Value::String(shell_id));
        let (msg, _) = execute_bash_output(&args);
        assert!(
            msg.contains("running"),
            "b1_output_running: running shell must report 'running'; got: {msg}"
        );
    }

    /// B1-output-d: incremental drain — second poll does not re-emit first poll's output.
    ///
    /// OC source: mod.rs:187-197 — std::mem::take swaps the buffer on each call.
    /// GAP: CC output is file-based (append-only); OC draining is OC-specific.
    #[test]
    #[cfg(unix)]
    fn b1_output_buffers_drained_on_each_poll() {
        let shell_id = super::super::BACKGROUND_SHELLS
            .spawn("echo sentinel_b1d; sleep 3")
            .expect("b1_output_drain: spawn must succeed");

        std::thread::sleep(std::time::Duration::from_millis(300));

        let mut args = HashMap::new();
        args.insert("shell_id".to_string(), serde_json::Value::String(shell_id));

        // First poll: should contain the echoed line
        let (first, _) = execute_bash_output(&args.clone());
        assert!(
            first.contains("sentinel_b1d"),
            "b1_output_drain: first poll must see buffered output; got: {first}"
        );

        // Second poll: buffer was swapped; should NOT re-emit the same line
        let (second, _) = execute_bash_output(&args);
        assert!(
            !second.contains("sentinel_b1d"),
            "b1_output_drain: second poll must NOT re-emit drained output; got: {second}"
        );
    }

    /// B1-output-e: finished shell shows "finished" in status, not "running".
    ///
    /// OC source: output.rs:31-36 — exit_code.map_or_else formats "finished (exit code: N)".
    #[test]
    #[cfg(unix)]
    fn b1_output_finished_shell_status_says_finished() {
        let shell_id = super::super::BACKGROUND_SHELLS
            .spawn("echo done_b1e")
            .expect("b1_output_finished: spawn must succeed");

        // Wait for the shell to finish
        std::thread::sleep(std::time::Duration::from_millis(400));

        let mut args = HashMap::new();
        args.insert("shell_id".to_string(), serde_json::Value::String(shell_id));
        let (msg, is_error) = execute_bash_output(&args);
        assert!(
            !is_error,
            "b1_output_finished: poll must succeed; got: {msg}"
        );
        assert!(
            msg.contains("finished"),
            "b1_output_finished: finished shell must report 'finished'; got: {msg}"
        );
        assert!(
            !msg.contains("Status: running"),
            "b1_output_finished: finished shell must NOT say 'running'; got: {msg}"
        );
    }
}
