//! Static analysis execution and Crosslink issue creation.
//!
//! Provides shell command execution with timeout for running static analysis
//! tools, and integration with the `crosslink` library for creating issues
//! from VDD findings (library-backed, no subprocess).

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
// Crosslink Integration (library-backed)
// ==========================================================================

/// Create a crosslink issue, label it, and attach a comment via the
/// `crosslink::db` library — no subprocess fork, no `chainlink` /
/// `crosslink` binary required on `$PATH`. The previous
/// `tokio::process::Command::new("chainlink")` triple has been
/// replaced by direct calls into the same SQLite-backed
/// `Database` the `crosslink` tool uses, so VDD findings land in
/// the project's `.crosslink/issues.db` exactly like agent-driven
/// `crosslink create` calls do.
///
/// Hopped onto `spawn_blocking` because `rusqlite::Connection` is
/// blocking I/O — keeps the async caller's runtime free.
///
/// Closes crosslink #277 (shell injection via finding title) by
/// removing the shell layer entirely; `title` / `label` / `comment`
/// are never interpreted by any shell.
pub(crate) async fn create_crosslink_issue(
    title: &str,
    label: &str,
    comment: &str,
) -> Result<String, VddError> {
    let title = title.to_string();
    let label = label.to_string();
    // Collapse newlines so the comment renders on one logical line in
    // the crosslink UI — matches the pre-port behaviour.
    let collapsed_comment = comment.replace('\n', " ");

    tokio::task::spawn_blocking(move || -> Result<String, VddError> {
        let cwd = std::env::current_dir().map_err(|e| {
            VddError::CrosslinkError(format!("Failed to read current directory: {e}"))
        })?;
        let dir = cwd.join(".crosslink");
        std::fs::create_dir_all(&dir)
            .map_err(|e| VddError::CrosslinkError(format!("Failed to create .crosslink/: {e}")))?;
        let db_path = dir.join("issues.db");
        let db = crosslink::db::Database::open(&db_path)
            .map_err(|e| VddError::CrosslinkError(format!("Failed to open crosslink DB: {e}")))?;
        let id = db
            .create_issue(&title, None, "high")
            .map_err(|e| VddError::CrosslinkError(format!("create_issue failed: {e}")))?;
        // Label + comment are best-effort. A label that fails to insert
        // (e.g. trips the per-row PK because the issue already had one)
        // shouldn't roll back the issue itself.
        if let Err(e) = db.add_label(id, &label) {
            tracing::warn!(issue_id = id, label = %label, "VDD: add_label failed: {e}");
        }
        if let Err(e) = db.add_comment(id, &collapsed_comment, "note") {
            tracing::warn!(issue_id = id, "VDD: add_comment failed: {e}");
        }
        Ok(id.to_string())
    })
    .await
    .map_err(|e| VddError::CrosslinkError(format!("blocking task panicked: {e}")))?
}
