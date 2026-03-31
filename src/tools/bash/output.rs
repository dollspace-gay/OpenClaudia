use super::BACKGROUND_SHELLS;
use crate::tools::safe_truncate;
use serde_json::Value;
use std::collections::HashMap;

/// Retrieve output from a background shell
pub(crate) fn execute_bash_output(args: &HashMap<String, Value>) -> (String, bool) {
    // If no shell_id provided, list all background shells
    let shell_id = match args.get("shell_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
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
                result.push_str(&format!("  {} [{}]: {}\n", id, status, cmd_preview));
            }
            return (result, false);
        }
    };

    match BACKGROUND_SHELLS.get_output(shell_id) {
        Ok((output, is_running, exit_code)) => {
            let status = if is_running {
                "running".to_string()
            } else {
                match exit_code {
                    Some(code) => format!("finished (exit code: {})", code),
                    None => "finished".to_string(),
                }
            };

            let result = if output.is_empty() {
                format!("Status: {}\n(no new output)", status)
            } else {
                format!("Status: {}\n\n{}", status, output)
            };

            (result, false)
        }
        Err(e) => (e, true),
    }
}
