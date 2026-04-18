//! Static analysis execution and Chainlink issue creation.
//!
//! Provides shell command execution with timeout for running static analysis
//! tools, and integration with Chainlink for creating issues from VDD findings.

use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;

use super::VddError;

// ==========================================================================
// StaticAnalysisResult
// ==========================================================================

/// Result of running a static analysis command
#[derive(Debug, Clone, Serialize)]
pub struct StaticAnalysisResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub passed: bool,
}

// ==========================================================================
// Shell Command Execution
// ==========================================================================

/// Run a shell command with timeout, returning structured result.
///
/// # Security
/// The command string is parsed with POSIX shlex into argv tokens and
/// executed via `Command::new(argv[0]).args(&argv[1..])` — **no shell is
/// invoked**. Previously this function routed through `sh -c` / `cmd /C`
/// with the raw string, allowing shell-metacharacter injection from any
/// config-sourced command (crosslink #277). Pipelines, redirections, and
/// `&&`/`||` are therefore no longer supported in this entry point; callers
/// that need them must compose subprocess invocations at the Rust level.
pub(crate) async fn run_shell_command(command: &str, timeout: Duration) -> StaticAnalysisResult {
    let tokens: Vec<String> = match shlex::split(command) {
        Some(t) if !t.is_empty() => t,
        Some(_) => {
            return StaticAnalysisResult {
                command: command.to_string(),
                exit_code: -1,
                stdout: String::new(),
                stderr: "Empty command".to_string(),
                passed: false,
            };
        }
        None => {
            return StaticAnalysisResult {
                command: command.to_string(),
                exit_code: -1,
                stdout: String::new(),
                stderr: "Could not parse command (unbalanced quotes or unsupported escape)"
                    .to_string(),
                passed: false,
            };
        }
    };

    let (program, argv_rest) = tokens.split_first().expect("non-empty by match above");
    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(program)
            .args(argv_rest)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            StaticAnalysisResult {
                command: command.to_string(),
                exit_code,
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                passed: exit_code == 0,
            }
        }
        Ok(Err(e)) => StaticAnalysisResult {
            command: command.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Command failed to execute: {e}"),
            passed: false,
        },
        Err(_) => StaticAnalysisResult {
            command: command.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Command timed out after {}s", timeout.as_secs()),
            passed: false,
        },
    }
}

// ==========================================================================
// Chainlink Integration
// ==========================================================================

/// Run `chainlink create`, then `label`, then `comment` via argv-level
/// dispatch. No shell is invoked at any stage — `title`, `label`, and
/// `comment` flow through as individual `Command::arg` values, so
/// backticks, `$()`, `;`, newlines, etc. are inert.
///
/// Closes crosslink #277 (shell injection via finding title).
pub(crate) async fn run_chainlink_create(
    title: &str,
    label: &str,
    comment: &str,
) -> Result<String, VddError> {
    // Create the issue: `chainlink create "<title>" -p high`
    let create_output = tokio::process::Command::new("chainlink")
        .arg("create")
        .arg(title)
        .arg("-p")
        .arg("high")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| VddError::ChainlinkError(format!("Failed to run chainlink: {e}")))?;

    let create_text = String::from_utf8_lossy(&create_output.stdout);

    // Extract issue ID from output like "Created issue #123"
    let issue_id = create_text
        .split('#')
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("unknown")
        .to_string();

    // Label it: `chainlink label <id> <label>`
    let _ = tokio::process::Command::new("chainlink")
        .arg("label")
        .arg(&issue_id)
        .arg(label)
        .output()
        .await;

    // Add comment: `chainlink comment <id> <text>`. Newlines inside the
    // comment body used to be replaced with spaces because they broke the
    // old shell quoting; argv dispatch preserves them correctly, but we
    // continue to collapse them so the resulting comment renders on one
    // logical line in the crosslink UI.
    let collapsed_comment = comment.replace('\n', " ");
    let _ = tokio::process::Command::new("chainlink")
        .arg("comment")
        .arg(&issue_id)
        .arg(&collapsed_comment)
        .output()
        .await;

    Ok(issue_id)
}
