use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// Track if we've shown the chainlink install message (only show once per session)
static CHAINLINK_INSTALL_SHOWN: AtomicBool = AtomicBool::new(false);

/// Execute chainlink command for task management
/// Uses Git Bash on Windows (which has access to Windows PATH)
pub(crate) fn execute_chainlink(args: &HashMap<String, Value>) -> (String, bool) {
    let cmd_args = match args.get("args").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return ("Missing 'args' argument".to_string(), true),
    };

    // Use Git Bash to run chainlink (same approach as execute_bash)
    #[cfg(windows)]
    let output = {
        match super::bash::find_git_bash() {
            Some(git_bash) => Command::new(git_bash)
                .args(["-c", &format!("chainlink {}", cmd_args)])
                .output(),
            None => Command::new("bash")
                .args(["-c", &format!("chainlink {}", cmd_args)])
                .output(),
        }
    };

    #[cfg(not(windows))]
    let output = Command::new("bash")
        .args(["-c", &format!("chainlink {}", cmd_args)])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            // Check if chainlink wasn't found
            if !output.status.success()
                && (stderr.contains("command not found") || stderr.contains("not recognized"))
            {
                let show_install_help = !CHAINLINK_INSTALL_SHOWN.swap(true, Ordering::Relaxed);
                if show_install_help {
                    return (
                        "Chainlink not found. Chainlink is a lightweight issue tracking tool designed to integrate with AI agents.\n\n\
                        Install from: https://github.com/dollspace-gay/chainlink".to_string(),
                        true
                    );
                } else {
                    return ("Chainlink not available.".to_string(), true);
                }
            }

            let mut result = stdout.to_string();
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                if !output.status.success() {
                    result.push_str("Error: ");
                }
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = "(chainlink command completed)".to_string();
            }

            (result.trim().to_string(), !output.status.success())
        }
        Err(e) => (format!("Failed to execute chainlink: {}", e), true),
    }
}
