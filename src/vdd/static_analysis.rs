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
pub(crate) async fn run_shell_command(command: &str, timeout: Duration) -> StaticAnalysisResult {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };

    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(shell)
            .arg(flag)
            .arg(command)
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

/// Run `chainlink create` and `chainlink label` to create an issue.
pub(crate) async fn run_chainlink_create(
    title: &str,
    label: &str,
    comment: &str,
) -> Result<String, VddError> {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };

    // Create the issue
    let create_output = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(format!(
            "chainlink create \"{}\" -p high",
            title.replace('"', "\\\"")
        ))
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

    // Label it
    let _ = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(format!("chainlink label {issue_id} {label}"))
        .output()
        .await;

    // Add comment with details
    let _ = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(format!(
            "chainlink comment {} \"{}\"",
            issue_id,
            comment.replace('"', "\\\"").replace('\n', " ")
        ))
        .output()
        .await;

    Ok(issue_id)
}
