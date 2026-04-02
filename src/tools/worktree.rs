//! Git worktree isolation for agent operations.
//!
//! Provides tools to create and manage isolated git worktrees
//! so agents can work on branches without affecting the main working tree.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Maximum time to wait for a git command (seconds).
const GIT_TIMEOUT_SECS: u64 = 30;

/// Run a git command with a timeout. Returns the output or an error.
fn git_with_timeout(args: &[&str]) -> Result<std::process::Output, String> {
    let mut child = Command::new("git")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn git: {e}"))?;

    // Poll for completion with timeout
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(GIT_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| format!("Git wait failed: {e}")),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // reap
                    return Err(format!(
                        "Git command timed out after {GIT_TIMEOUT_SECS}s: git {}",
                        args.join(" ")
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Git wait error: {e}")),
        }
    }
}

/// State of an active worktree
#[derive(Debug, Clone)]
pub struct WorktreeState {
    pub path: PathBuf,
    pub branch: String,
    pub original_cwd: PathBuf,
}

/// Create a new git worktree for isolated agent work.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn execute_enter_worktree(args: &HashMap<String, Value>) -> (String, bool) {
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if branch.is_empty() {
        return ("Error: branch name is required".to_string(), true);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Check if we're in a git repo
    match git_with_timeout(&["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) if output.status.success() => {}
        _ => return ("Error: not inside a git repository".to_string(), true),
    }

    // Get git root
    let git_root = git_with_timeout(&["rev-parse", "--show-toplevel"])
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| cwd.clone(), |s| PathBuf::from(s.trim()));

    let worktree_dir = git_root.join(".worktrees").join(&branch);

    // Create the worktree
    let base_branch = get_current_branch().unwrap_or_else(|| "HEAD".to_string());

    // Try to create a new branch, or use existing
    let result = git_with_timeout(&[
        "worktree",
        "add",
        "-b",
        &branch,
        worktree_dir.to_str().unwrap_or(""),
        &base_branch,
    ]);

    match result {
        Ok(output) if output.status.success() => {
            // Change to the worktree directory
            if std::env::set_current_dir(&worktree_dir).is_err() {
                return (
                    format!(
                        "Created worktree but failed to change directory to {}",
                        worktree_dir.display()
                    ),
                    true,
                );
            }
            (
                format!(
                    "Entered worktree at {} on branch '{}' (based on '{}')\nOriginal directory: {}",
                    worktree_dir.display(),
                    branch,
                    base_branch,
                    cwd.display()
                ),
                false,
            )
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Branch might already exist -- try without -b
            if stderr.contains("already exists") {
                let retry = git_with_timeout(&[
                    "worktree",
                    "add",
                    worktree_dir.to_str().unwrap_or(""),
                    &branch,
                ]);
                match retry {
                    Ok(o) if o.status.success() => {
                        let _ = std::env::set_current_dir(&worktree_dir);
                        (
                            format!(
                                "Entered existing worktree at {} on branch '{}'",
                                worktree_dir.display(),
                                branch
                            ),
                            false,
                        )
                    }
                    _ => (
                        format!("Failed to create worktree: {}", stderr.trim()),
                        true,
                    ),
                }
            } else {
                (
                    format!("Failed to create worktree: {}", stderr.trim()),
                    true,
                )
            }
        }
        Err(e) => (format!("Failed to run git: {e}"), true),
    }
}

/// Exit a worktree and return to the original directory.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn execute_exit_worktree(args: &HashMap<String, Value>) -> (String, bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let apply_changes = args
        .get("apply_changes")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Check if we're in a worktree
    let wt_check = git_with_timeout(&["rev-parse", "--git-common-dir"]);

    let common_dir = match wt_check {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => return ("Error: not in a git worktree".to_string(), true),
    };

    let git_dir = git_with_timeout(&["rev-parse", "--git-dir"])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // If git-dir equals git-common-dir, we're in the main worktree, not an isolated one
    if git_dir == common_dir || git_dir == ".git" {
        return (
            "Not in an isolated worktree. Use this tool only inside a worktree created by enter_worktree.".to_string(),
            true,
        );
    }

    let current_branch = get_current_branch().unwrap_or_default();

    // Find the main worktree path
    let main_path = Path::new(&common_dir)
        .parent()
        .unwrap_or_else(|| Path::new("."));

    if apply_changes {
        // Commit any uncommitted changes
        let _ = git_with_timeout(&["add", "-A"]);
        let commit = git_with_timeout(&[
            "commit",
            "-m",
            &format!("Worktree changes from branch '{current_branch}'"),
        ]);

        let committed = commit.map(|o| o.status.success()).unwrap_or(false);

        // Switch to main worktree
        let _ = std::env::set_current_dir(main_path);

        // Merge the branch
        if committed {
            let merge = git_with_timeout(&["merge", &current_branch, "--no-edit"]);

            let merge_result = match merge {
                Ok(o) if o.status.success() => {
                    format!("Merged branch '{current_branch}' into main worktree.")
                }
                Ok(o) => format!(
                    "Merge had conflicts: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                Err(e) => format!("Merge failed: {e}"),
            };

            // Clean up worktree
            let _ = git_with_timeout(&["worktree", "remove", cwd.to_str().unwrap_or(""), "--force"]);

            (
                format!(
                    "Exited worktree. {}\nReturned to: {}",
                    merge_result,
                    main_path.display()
                ),
                false,
            )
        } else {
            let _ = git_with_timeout(&["worktree", "remove", cwd.to_str().unwrap_or(""), "--force"]);
            (
                format!(
                    "No changes to commit. Removed worktree.\nReturned to: {}",
                    main_path.display()
                ),
                false,
            )
        }
    } else {
        // Discard and return
        let _ = std::env::set_current_dir(main_path);
        let _ = git_with_timeout(&["worktree", "remove", cwd.to_str().unwrap_or(""), "--force"]);
        (
            format!(
                "Discarded worktree on branch '{}'. Returned to: {}",
                current_branch,
                main_path.display()
            ),
            false,
        )
    }
}

/// List active worktrees
#[must_use]
pub fn execute_list_worktrees() -> (String, bool) {
    let output = git_with_timeout(&["worktree", "list", "--porcelain"]);

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let mut worktrees = Vec::new();
            let mut current: HashMap<String, String> = HashMap::new();

            for line in stdout.lines() {
                if line.is_empty() {
                    if !current.is_empty() {
                        let path = current.get("worktree").cloned().unwrap_or_default();
                        let branch = current
                            .get("branch")
                            .cloned()
                            .unwrap_or_else(|| "detached".to_string());
                        let branch = branch
                            .strip_prefix("refs/heads/")
                            .unwrap_or(&branch)
                            .to_string();
                        worktrees.push(format!("  {path} ({branch})"));
                        current.clear();
                    }
                } else if let Some((key, value)) = line.split_once(' ') {
                    current.insert(key.to_string(), value.to_string());
                } else {
                    current.insert(line.to_string(), String::new());
                }
            }
            if !current.is_empty() {
                let path = current.get("worktree").cloned().unwrap_or_default();
                let branch = current
                    .get("branch")
                    .cloned()
                    .unwrap_or_else(|| "detached".to_string());
                let branch = branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&branch)
                    .to_string();
                worktrees.push(format!("  {path} ({branch})"));
            }

            if worktrees.is_empty() {
                ("No active worktrees.".to_string(), false)
            } else {
                (
                    format!("Active worktrees:\n{}", worktrees.join("\n")),
                    false,
                )
            }
        }
        Ok(o) => (
            format!(
                "git worktree list failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ),
            true,
        ),
        Err(e) => (format!("Failed to run git: {e}"), true),
    }
}

fn get_current_branch() -> Option<String> {
    git_with_timeout(&["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_current_branch() {
        // Should work in the test environment (we're in a git repo)
        let branch = get_current_branch();
        assert!(branch.is_some());
    }

    #[test]
    fn test_enter_worktree_requires_branch() {
        let args = HashMap::new();
        let (msg, is_err) = execute_enter_worktree(&args);
        assert!(is_err);
        assert!(msg.contains("branch name is required"));
    }

    #[test]
    fn test_list_worktrees() {
        let (msg, is_err) = execute_list_worktrees();
        // Should work in any git repo
        assert!(!is_err);
        assert!(msg.contains("worktree") || msg.contains("Active"));
    }
}
