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
