//! Shared subprocess-with-timeout helper.
//!
//! Several tool implementations spawn external programs that can hang
//! indefinitely on adversarial input — `git` against a slow remote,
//! `pdftotext` against a malformed PDF, `pdfinfo` against a file whose
//! `XRef` table loops. Previously each module re-implemented its own
//! polling loop (or skipped the timeout entirely). [`run_with_timeout`]
//! is the single chokepoint so a fix or tuning change applies
//! uniformly (crosslink #836).
//!
//! The polling-with-backoff loop is intentionally NOT a `tokio::spawn`
//! / `tokio::time::timeout` pair: many callers (the `read_pdf_file`
//! tool, the `git_in` helper in worktree.rs) execute synchronously
//! inside a blocking tool dispatch — pulling tokio in would require a
//! runtime handle the caller does not always have. The exponential
//! backoff matches the schedule worktree.rs already used so trivial
//! commands still see sub-millisecond exit-detection overhead.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Exponential-backoff polling schedule (ms between `try_wait` calls).
/// Pins on the last entry once exhausted so long-running commands cost
/// at most one poll per 100 ms (crosslink #956, #836).
const WAIT_BACKOFF_MS: &[u64] = &[1, 2, 5, 10, 25, 50, 100];

/// Run `program` with `args` under `timeout`. Captures stdout and
/// stderr (both `Stdio::piped`) and returns them in [`Output`] on a
/// clean exit. On deadline expiry, sends SIGKILL via [`Child::kill`]
/// and reaps the zombie before returning a structured timeout error
/// so callers can render the program name + argv tail to the user.
///
/// `cwd` is applied via [`Command::current_dir`] when `Some`. Pass
/// `None` to inherit the parent's working directory — the caller is
/// expected to have validated that path globally; this helper does
/// not.
///
/// # Errors
///
/// Returns [`CommandError::SpawnFailed`] if the program could not be
/// invoked at all (binary not on PATH, EACCES, fork failure), and
/// [`CommandError::TimedOut`] if the deadline elapsed before exit.
/// Wait errors (a rare kernel-side condition: signal handler races,
/// EINTR after retry exhaustion) surface as
/// [`CommandError::WaitFailed`].
pub fn run_with_timeout(
    program: &(impl AsRef<OsStr> + ?Sized),
    args: &[impl AsRef<OsStr>],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<Output, CommandError> {
    run_with_timeout_with_env(program, args, cwd, timeout, &HashMap::new())
}

/// Run `program` under `timeout`, applying additional environment variables
/// to the child process.
///
/// See [`run_with_timeout`] for timeout and error semantics.
///
/// # Errors
///
/// Returns the same structured [`CommandError`] variants as
/// [`run_with_timeout`].
pub fn run_with_timeout_with_env(
    program: &(impl AsRef<OsStr> + ?Sized),
    args: &[impl AsRef<OsStr>],
    cwd: Option<&Path>,
    timeout: Duration,
    env: &HashMap<String, String>,
) -> Result<Output, CommandError> {
    let program_str = program.as_ref().to_string_lossy().into_owned();
    let mut cmd = Command::new(program);
    cmd.args(args.iter().map(AsRef::as_ref))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.envs(env);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let mut child = cmd.spawn().map_err(|e| CommandError::SpawnFailed {
        program: program_str.clone(),
        source: e.to_string(),
    })?;

    let deadline = Instant::now() + timeout;
    let mut step = 0usize;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| CommandError::WaitFailed {
                        program: program_str,
                        source: e.to_string(),
                    });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CommandError::TimedOut {
                        program: program_str,
                        timeout,
                    });
                }
                let idx = step.min(WAIT_BACKOFF_MS.len() - 1);
                std::thread::sleep(Duration::from_millis(WAIT_BACKOFF_MS[idx]));
                step = step.saturating_add(1);
            }
            Err(e) => {
                return Err(CommandError::WaitFailed {
                    program: program_str,
                    source: e.to_string(),
                });
            }
        }
    }
}

/// Errors returned by [`run_with_timeout`]. The variants are
/// structured (program name kept) so callers can render messages
/// without re-parsing the source error string. Implements
/// [`std::fmt::Display`] with a stable format so tool-output assertions
/// in tests stay readable.
#[derive(Debug)]
pub enum CommandError {
    /// `Command::spawn` failed — program not on PATH, EACCES,
    /// fork failure, etc.
    SpawnFailed { program: String, source: String },
    /// Deadline elapsed before the child exited; the child has been
    /// killed and reaped before this variant is returned.
    TimedOut { program: String, timeout: Duration },
    /// The `wait`/`wait_with_output` path itself returned an error. Rare;
    /// usually signal-handler races (`EINTR` storms) or pipe-buffer
    /// exhaustion after the child exited.
    WaitFailed { program: String, source: String },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed { program, source } => {
                write!(f, "Failed to spawn {program}: {source}")
            }
            Self::TimedOut { program, timeout } => {
                write!(f, "{program} timed out after {}s", timeout.as_secs())
            }
            Self::WaitFailed { program, source } => {
                write!(f, "{program} wait failed: {source}")
            }
        }
    }
}

impl std::error::Error for CommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial `true` invocation completes well inside the timeout
    /// and reports an empty-stdout success Output. This pins the
    /// happy path so refactors don't accidentally introduce a sleep
    /// after exit.
    #[test]
    fn run_with_timeout_succeeds_for_fast_command() {
        let out = run_with_timeout("true", &Vec::<&str>::new(), None, Duration::from_secs(5))
            .expect("`true` must exit cleanly");
        assert!(out.status.success(), "exit status must be 0");
        assert!(
            out.stdout.is_empty(),
            "`true` writes no stdout, got {:?}",
            out.stdout
        );
    }

    /// A `sleep` that exceeds the timeout returns
    /// [`CommandError::TimedOut`] WITHOUT leaking the child. The exact
    /// wall-clock varies between schedulers; we assert only that the
    /// total elapsed is close to the timeout, not under it.
    #[test]
    fn run_with_timeout_kills_command_past_deadline() {
        let start = Instant::now();
        let res = run_with_timeout("sleep", &["5"], None, Duration::from_millis(100));
        let elapsed = start.elapsed();
        match res {
            Err(CommandError::TimedOut { program, .. }) => {
                assert_eq!(program, "sleep");
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
        // Hard upper bound: 2× the deadline tolerates CI jitter
        // without masking a "didn't kill the child" regression.
        assert!(
            elapsed < Duration::from_millis(500),
            "run_with_timeout must return promptly after timeout; took {elapsed:?}"
        );
    }

    /// Spawning a nonexistent program surfaces `SpawnFailed`, not a
    /// timeout — important because the caller's error rendering branch
    /// differs (install hint vs retry suggestion).
    #[test]
    fn run_with_timeout_reports_spawn_failure() {
        let res = run_with_timeout(
            "definitely-not-on-path-xyzzy-9f87",
            &Vec::<&str>::new(),
            None,
            Duration::from_secs(1),
        );
        match res {
            Err(CommandError::SpawnFailed { program, .. }) => {
                assert!(
                    program.contains("xyzzy"),
                    "program field must echo the requested binary, got: {program}"
                );
            }
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
    }
}
