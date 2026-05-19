//! Git worktree isolation for agent operations.
//!
//! Provides tools to create and manage isolated git worktrees so agents can
//! work on branches without affecting the main working tree.
//!
//! # Phase 1: no CWD mutation (crosslink #345)
//!
//! Earlier revisions called [`std::env::set_current_dir`] inside the enter
//! and exit handlers. That is process-wide global state: any other thread
//! (proxy, TUI, concurrent tool executor) doing a relative-path operation
//! races against the mutation and sees an inconsistent view of the working
//! directory. POSIX, Rust, and Go all document `chdir` as fundamentally
//! unsafe for concurrent processes.
//!
//! In Phase 1 of the fix:
//!
//! * `execute_enter_worktree` never mutates the process CWD. It creates the
//!   git worktree and returns the new path in its success message. The
//!   caller (REPL / session layer) is responsible for tracking the active
//!   worktree on the session.
//! * `execute_exit_worktree` no longer reads CWD to discover which worktree
//!   to clean up. It requires an explicit `path` argument naming the
//!   worktree to remove.
//! * All `git` invocations take an explicit `cwd` and pass it to
//!   `Command::current_dir`, so no `git` subprocess depends on the parent's
//!   CWD either.
//!
//! Phase 2 (passing the active worktree through `ToolContext` to bash /
//! file / lsp tool calls) is tracked separately — see the follow-up issue
//! filed against #345.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Maximum time to wait for a git command (seconds).
const GIT_TIMEOUT_SECS: u64 = 30;

/// Run a git command in a specified working directory with a timeout.
///
/// `cwd` is mandatory: every call site must say *where* the git command runs.
/// This is the contract that lets us remove `set_current_dir` from this
/// module entirely (crosslink #345).
fn git_in(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn git: {e}"))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(GIT_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("Git wait failed: {e}"))
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
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

/// State of an active worktree.
///
/// `original_cwd` records the CWD at the moment the worktree was created so
/// callers can use it as the path against which the not-yet-implemented
/// `ToolContext.cwd` would resolve relative paths once Phase 2 lands.
#[derive(Debug, Clone)]
pub struct WorktreeState {
    pub path: PathBuf,
    pub branch: String,
    pub original_cwd: PathBuf,
}

/// Create a new git worktree for isolated agent work.
///
/// **Phase 1 (#345) behavior**: this function does NOT change the process
/// CWD. It only invokes git to create the worktree directory and returns the
/// resulting path in its success message. The caller is responsible for
/// recording the active worktree on the session and threading the path into
/// subsequent tool calls (Phase 2).
#[must_use]
pub fn execute_enter_worktree<S: std::hash::BuildHasher>(
    args: &HashMap<String, Value, S>,
) -> (String, bool) {
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if branch.is_empty() {
        return ("Error: branch name is required".to_string(), true);
    }

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return (format!("Error: cannot read current directory: {e}"), true),
    };

    match git_in(&cwd, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) if output.status.success() => {}
        _ => return ("Error: not inside a git repository".to_string(), true),
    }

    let git_root = git_in(&cwd, &["rev-parse", "--show-toplevel"])
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| cwd.clone(), |s| PathBuf::from(s.trim()));

    let worktree_dir = git_root.join(".worktrees").join(&branch);

    let base_branch = get_current_branch_at(&cwd).unwrap_or_else(|| "HEAD".to_string());

    let result = git_in(
        &cwd,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree_dir.to_str().unwrap_or(""),
            &base_branch,
        ],
    );

    match result {
        Ok(output) if output.status.success() => (
            format!(
                "Created worktree at {} on branch '{}' (based on '{}').\n\
                 The process CWD has NOT been changed. Pass path={} to exit_worktree, \
                 or use `bash` with explicit working directories when running commands \
                 inside the worktree.\nOriginal directory: {}",
                worktree_dir.display(),
                branch,
                base_branch,
                worktree_dir.display(),
                cwd.display()
            ),
            false,
        ),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("already exists") {
                let retry = git_in(
                    &cwd,
                    &[
                        "worktree",
                        "add",
                        worktree_dir.to_str().unwrap_or(""),
                        &branch,
                    ],
                );
                match retry {
                    Ok(o) if o.status.success() => (
                        format!(
                            "Created worktree (existing branch) at {} on branch '{}'.\n\
                             The process CWD has NOT been changed. Pass path={} to exit_worktree.",
                            worktree_dir.display(),
                            branch,
                            worktree_dir.display()
                        ),
                        false,
                    ),
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

/// Resolved geometry of a worktree-exit request.
///
/// Computed by [`validate_exit_request`] before any mutating git command runs
/// so the orchestration in [`execute_exit_worktree`] stays a flat sequence of
/// helpers rather than a deeply nested function.
struct ExitContext {
    worktree_path: PathBuf,
    main_path: PathBuf,
    current_branch: String,
    apply_changes: bool,
}

/// Parse and validate the arguments to `exit_worktree`. Returns either a
/// resolved [`ExitContext`] or an error tuple ready to bubble back to the
/// caller.
fn validate_exit_request<S: std::hash::BuildHasher>(
    args: &HashMap<String, Value, S>,
) -> Result<ExitContext, (String, bool)> {
    let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path_arg.is_empty() {
        return Err((
            "Error: 'path' is required — exit_worktree no longer reads the \
             process CWD. Pass the path returned by enter_worktree."
                .to_string(),
            true,
        ));
    }
    let worktree_path = PathBuf::from(path_arg);
    if !worktree_path.exists() {
        return Err((
            format!(
                "Error: worktree path does not exist: {}",
                worktree_path.display()
            ),
            true,
        ));
    }

    let apply_changes = args
        .get("apply_changes")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let common_dir = match git_in(&worktree_path, &["rev-parse", "--git-common-dir"]) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            return Err((
                format!(
                    "Error: path is not inside a git worktree: {}",
                    worktree_path.display()
                ),
                true,
            ));
        }
    };

    let git_dir = git_in(&worktree_path, &["rev-parse", "--git-dir"])
        .ok()
        .map_or_else(String::new, |o| {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        });

    if git_dir == common_dir || git_dir == ".git" {
        return Err((
            "Not in an isolated worktree. Use this tool only on a worktree \
             created by enter_worktree."
                .to_string(),
            true,
        ));
    }

    let current_branch = get_current_branch_at(&worktree_path).unwrap_or_default();
    let main_path = Path::new(&common_dir)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    Ok(ExitContext {
        worktree_path,
        main_path,
        current_branch,
        apply_changes,
    })
}

/// Render a git error from `git_in`'s `Result<Output, String>`.
fn render_git_failure(res: &Result<std::process::Output, String>) -> String {
    match res {
        Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
        Err(e) => e.clone(),
    }
}

/// Stage + commit + merge the worktree branch into the main worktree.
fn merge_into_main(ctx: &ExitContext) -> String {
    let _stage = git_in(&ctx.worktree_path, &["add", "-A"]);
    let commit = git_in(
        &ctx.worktree_path,
        &[
            "commit",
            "-m",
            &format!("Worktree changes from branch '{}'", ctx.current_branch),
        ],
    );
    let committed = commit.is_ok_and(|o| o.status.success());

    if !committed {
        return "No changes to commit.".to_string();
    }

    match git_in(&ctx.main_path, &["merge", &ctx.current_branch, "--no-edit"]) {
        Ok(o) if o.status.success() => {
            format!("Merged branch '{}' into main worktree.", ctx.current_branch)
        }
        Ok(o) => format!(
            "Merge had conflicts: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => format!("Merge failed: {e}"),
    }
}

/// Issue `git worktree remove --force` from the main worktree and return
/// `(removed_ok, detail_string)`.
fn remove_worktree(ctx: &ExitContext) -> (bool, String) {
    let removed = git_in(
        &ctx.main_path,
        &[
            "worktree",
            "remove",
            ctx.worktree_path.to_str().unwrap_or(""),
            "--force",
        ],
    );
    let ok = removed.as_ref().is_ok_and(|o| o.status.success());
    (ok, render_git_failure(&removed))
}

/// Exit (remove) an isolated git worktree.
///
/// **Phase 1 (#345) behavior**: this function does NOT read or mutate the
/// process CWD. The caller must pass `path` naming the worktree to remove.
/// All git commands run against that path explicitly via `current_dir`.
///
/// Arguments:
/// * `path` (string, required) — absolute path to the worktree that
///   [`execute_enter_worktree`] previously created.
/// * `apply_changes` (bool, optional, default `false`) — if true, commit
///   uncommitted changes inside the worktree and merge the worktree branch
///   into the main worktree before removing the worktree.
#[must_use]
pub fn execute_exit_worktree<S: std::hash::BuildHasher>(
    args: &HashMap<String, Value, S>,
) -> (String, bool) {
    let ctx = match validate_exit_request(args) {
        Ok(ctx) => ctx,
        Err(err) => return err,
    };

    if ctx.apply_changes {
        let merge_result = merge_into_main(&ctx);
        let (removed_ok, detail) = remove_worktree(&ctx);
        let warning = if removed_ok {
            String::new()
        } else {
            format!(" WARNING: worktree removal failed: {detail}")
        };
        return (
            format!(
                "Exited worktree at {}. {}{}\nMain worktree: {}",
                ctx.worktree_path.display(),
                merge_result,
                warning,
                ctx.main_path.display()
            ),
            !removed_ok,
        );
    }

    let (removed_ok, detail) = remove_worktree(&ctx);
    if removed_ok {
        (
            format!(
                "Discarded worktree on branch '{}' at {}. Main worktree: {}",
                ctx.current_branch,
                ctx.worktree_path.display(),
                ctx.main_path.display()
            ),
            false,
        )
    } else {
        (
            format!(
                "Failed to remove worktree at {}: {}",
                ctx.worktree_path.display(),
                detail
            ),
            true,
        )
    }
}

/// List active worktrees.
///
/// Runs `git worktree list` from the process CWD (read-only) — this queries
/// git but does not mutate any state.
#[must_use]
pub fn execute_list_worktrees() -> (String, bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let output = git_in(&cwd, &["worktree", "list", "--porcelain"]);

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

fn get_current_branch_at(cwd: &Path) -> Option<String> {
    git_in(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Some tests still rely on observing the process CWD. The lock keeps
    /// them sequential — but note that the production functions in this
    /// module no longer touch `set_current_dir` at all (crosslink #345).
    fn cwd_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn test_get_current_branch_at_cwd() {
        let _lock = cwd_lock();
        let cwd = std::env::current_dir().unwrap();
        let branch = get_current_branch_at(&cwd);
        assert!(branch.is_some());
    }

    #[test]
    fn test_enter_worktree_requires_branch() {
        let _lock = cwd_lock();
        let args = HashMap::new();
        let (msg, is_err) = execute_enter_worktree(&args);
        assert!(is_err);
        assert!(msg.contains("branch name is required"));
    }

    #[test]
    fn test_list_worktrees() {
        let _lock = cwd_lock();
        let (msg, is_err) = execute_list_worktrees();
        assert!(!is_err);
        assert!(msg.contains("worktree") || msg.contains("Active"));
    }

    // ─── Spec §5: Worktree enter/exit updates session working directory ────────

    #[test]
    fn enter_worktree_empty_branch_is_error() {
        let _lock = cwd_lock();
        let mut args = HashMap::new();
        args.insert(
            "branch".to_string(),
            serde_json::Value::String(String::new()),
        );
        let (msg, is_err) = execute_enter_worktree(&args);
        assert!(is_err, "empty branch must produce is_error=true");
        assert!(
            msg.contains("branch name is required"),
            "error message must mention branch; got: {msg}"
        );
    }

    /// Contract: `enter_worktree` outside a git repo returns `is_error=true`
    /// with a repo-not-found message.
    ///
    /// Because the production function no longer mutates CWD, the only way
    /// to exercise the "not a git repo" path is to set CWD in the test
    /// itself. We hold `cwd_lock` to serialise with other tests, and restore
    /// CWD on the way out.
    #[test]
    fn enter_worktree_outside_git_repo_is_error() {
        let _lock = cwd_lock();
        let tmp = tempfile::tempdir().expect("temp dir");
        let original = std::env::current_dir().ok();

        let _ = std::env::set_current_dir(tmp.path());

        let mut args = HashMap::new();
        args.insert(
            "branch".to_string(),
            serde_json::Value::String("test-branch".to_string()),
        );
        let (msg, is_err) = execute_enter_worktree(&args);

        if let Some(orig) = original {
            let _ = std::env::set_current_dir(orig);
        }

        assert!(is_err, "must error outside a git repo");
        assert!(
            msg.contains("not inside a git repository"),
            "error must say 'not inside a git repository'; got: {msg}"
        );
    }

    /// Contract: `exit_worktree` with no `path` arg returns `is_error=true`.
    /// The old behavior — falling back to the process CWD — is exactly the
    /// global-state bug fixed by #345.
    #[test]
    fn exit_worktree_without_path_is_error() {
        let _lock = cwd_lock();
        let args = HashMap::new();
        let (msg, is_err) = execute_exit_worktree(&args);
        assert!(is_err, "missing path must produce is_error=true");
        assert!(
            msg.contains("'path' is required"),
            "error message must mention required path; got: {msg}"
        );
    }

    /// Contract: `exit_worktree` called with a path pointing at the main
    /// worktree (not an isolated one) returns `is_error=true` with a clear
    /// message — regardless of process CWD.
    #[test]
    fn exit_worktree_with_main_tree_path_is_error() {
        let _lock = cwd_lock();
        let main = std::env::current_dir().unwrap();
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            serde_json::Value::String(main.display().to_string()),
        );
        let (msg, is_err) = execute_exit_worktree(&args);
        assert!(is_err, "exit on main worktree must produce is_error=true");
        assert!(
            msg.contains("Not in an isolated worktree")
                || msg.contains("not inside a git worktree"),
            "error must indicate we are not in an isolated worktree; got: {msg}"
        );
    }

    /// Pin gap #624: OC does NOT check for an already-active worktree session
    /// before creating another. Verified by calling with a valid branch and
    /// confirming no "session already active" guard fires.
    #[test]
    fn enter_worktree_has_no_duplicate_session_guard_gap624() {
        let _lock = cwd_lock();
        let mut args = HashMap::new();
        args.insert(
            "branch".to_string(),
            serde_json::Value::String("probe-gap624".to_string()),
        );
        let (msg, _) = execute_enter_worktree(&args);
        assert!(
            !msg.contains("already in a worktree"),
            "gap #624: OC must NOT emit 'already in a worktree' guard; got: {msg}"
        );
    }

    /// Pin gap #623: `exit_worktree` with `apply_changes=false` runs
    /// `git worktree remove --force` without checking for uncommitted work.
    #[test]
    fn exit_worktree_discard_path_has_no_safety_guard_gap623() {
        let _lock = cwd_lock();
        let main = std::env::current_dir().unwrap();
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            serde_json::Value::String(main.display().to_string()),
        );
        args.insert("apply_changes".to_string(), serde_json::Value::Bool(false));
        let (msg, _) = execute_exit_worktree(&args);
        assert!(
            !msg.contains("uncommitted changes") && !msg.contains("discard_changes"),
            "gap #623: OC must NOT emit a safety-guard message; got: {msg}"
        );
    }

    // ─── #345 regression tests: CWD must not be mutated ───────────────────────

    /// `execute_enter_worktree` must NOT change the process CWD, even on the
    /// happy path that creates a worktree. This is the core invariant of
    /// crosslink #345 — every other thread doing relative-path work must
    /// continue to see the same CWD.
    #[test]
    fn enter_worktree_does_not_mutate_process_cwd() {
        let _lock = cwd_lock();
        let before = std::env::current_dir().expect("cwd before");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let branch = format!("test-345-{nanos}");

        let mut args = HashMap::new();
        args.insert(
            "branch".to_string(),
            serde_json::Value::String(branch.clone()),
        );
        let (msg, _is_err) = execute_enter_worktree(&args);

        let after = std::env::current_dir().expect("cwd after");
        assert_eq!(
            before, after,
            "execute_enter_worktree must not mutate process CWD; \
             before={before:?} after={after:?} msg={msg}"
        );

        // Best-effort cleanup if we did succeed.
        if !msg.contains("Failed") && !msg.contains("Error") {
            let wt = before.join(".worktrees").join(&branch);
            let _ = git_in(
                &before,
                &["worktree", "remove", wt.to_str().unwrap_or(""), "--force"],
            );
            let _ = git_in(&before, &["branch", "-D", &branch]);
        }
    }

    /// `execute_exit_worktree` must NOT change the process CWD on any error
    /// path — including the new "missing path" error introduced in #345.
    #[test]
    fn exit_worktree_does_not_mutate_process_cwd_on_error() {
        let _lock = cwd_lock();
        let before = std::env::current_dir().expect("cwd before");

        let (_, is_err) = execute_exit_worktree(&HashMap::new());
        assert!(is_err);
        let after_missing = std::env::current_dir().expect("cwd after missing-path");
        assert_eq!(before, after_missing, "CWD changed on missing-path error");

        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            serde_json::Value::String("/nonexistent/path/for/345".to_string()),
        );
        let (_, is_err) = execute_exit_worktree(&args);
        assert!(is_err);
        let after_nonexistent = std::env::current_dir().expect("cwd after nonexistent");
        assert_eq!(
            before, after_nonexistent,
            "CWD changed on nonexistent-path error"
        );

        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            serde_json::Value::String(before.display().to_string()),
        );
        let (_, is_err) = execute_exit_worktree(&args);
        assert!(is_err);
        let after_main = std::env::current_dir().expect("cwd after main");
        assert_eq!(before, after_main, "CWD changed on main-worktree error");
    }

    /// Forensic anti-regression: this module must not *call*
    /// `set_current_dir` in any production function. Test code is allowed
    /// to call it (the "outside git repo" test deliberately sets CWD to a
    /// temp dir to simulate that environment), so the assertion is scoped
    /// to the production region of the file — everything before
    /// `#[cfg(test)]`.
    ///
    /// We grep for the call-site pattern `set_current_dir(` to ignore
    /// docstring mentions of the symbol, then strip line comments so that
    /// a `// set_current_dir(...)` comment never trips the regression.
    #[test]
    fn production_code_contains_no_set_current_dir_calls_345() {
        let src = include_str!("worktree.rs");
        let cfg_test = src
            .find("#[cfg(test)]")
            .expect("test module marker must be present");
        let production = &src[..cfg_test];

        for (idx, raw_line) in production.lines().enumerate() {
            // Drop everything after `//` so a line like
            // `// don't call set_current_dir(...)` does not trigger.
            let code = raw_line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("set_current_dir("),
                "crosslink #345: production code in src/tools/worktree.rs must \
                 not call set_current_dir (process-wide global mutation); \
                 line {n}: {raw_line}",
                n = idx + 1,
            );
        }
    }
}
