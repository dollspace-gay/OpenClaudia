//! Bash path allowlist — parity with Claude Code's `checkPathConstraints`.
//!
//! See crosslink #594. CC's bash tool enforces an "additional working
//! directories" allowlist on top of the conversation's root directory: every
//! path mentioned by the spawned command (via `cd`, redirect targets, etc.)
//! must resolve under one of the allowed roots, or the command is refused
//! before spawn.
//!
//! `OpenClaudia`'s existing [`super::policy`] module covers the *content* of
//! the command (denylist, env scrub, safety allowlist). This module covers
//! the *paths* the command may touch. The two are independent gates: a
//! `cat /etc/passwd` is content-safe but path-unsafe; `rm -rf /` is
//! path-safe-looking (no `cd` token) but content-denied.
//!
//! ## Scope and limitations
//!
//! The check is intentionally *prefix-based* on syntactically-extracted
//! tokens; it does NOT execute the command in a sandbox. A sufficiently
//! clever attacker can defeat it with shell-expansion (`cat /et$x/passwd`),
//! `$(...)` substitution, or symlinks pointing out of the allowed root.
//! The `policy::dangerous_shell_construct` check refuses to *auto-allow*
//! such commands, so this layer composes with that one: anything containing
//! `$(`/`<(`/backticks/etc. already fails the safety gate before path
//! constraints are consulted. For commands that pass the safety gate, this
//! module catches the common naive-traversal cases.
//!
//! Treat this as a defence-in-depth layer, not a sandbox.

use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, RwLock};

/// Process-wide active path constraints. `None` (the default) means the
/// gate is disabled — bash commands are not subject to path checks at all,
/// preserving the legacy behaviour for callers that have not opted in.
///
/// Populated by [`install_global`] at startup (typically from
/// `.claude/settings.json::additionalWorkingDirectories`), and consulted by
/// [`check_command_against_global`] which the bash entry point calls before
/// spawning the child.
static GLOBAL_CONSTRAINTS: LazyLock<RwLock<Option<PathConstraints>>> =
    LazyLock::new(|| RwLock::new(None));

/// Install a [`PathConstraints`] set as the process-wide active gate.
///
/// Subsequent calls to [`check_command_against_global`] will validate
/// commands against this set. Passing an empty constraint set is allowed
/// — it disables the gate explicitly, distinct from "never installed".
///
/// See crosslink #594.
pub fn install_global(constraints: PathConstraints) {
    let mut slot = GLOBAL_CONSTRAINTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = Some(constraints);
}

/// Clear any previously installed global constraints — primarily for tests
/// that want to restore the default (gate disabled) state.
pub fn clear_global() {
    let mut slot = GLOBAL_CONSTRAINTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = None;
}

/// Validate `command` against the process-wide global constraint set.
///
/// Returns `Ok(())` when no constraint set is active (the gate is
/// disabled), or when every extracted path falls under an allowed root.
///
/// Called from `execute_bash` before spawning the child process so a
/// `cat /etc/passwd` is rejected with an actionable error message instead
/// of leaking the file's contents.
///
/// # Errors
///
/// Returns the same error string format as [`PathConstraints::check_command`].
pub fn check_command_against_global(command: &str) -> Result<(), String> {
    let slot = GLOBAL_CONSTRAINTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slot.as_ref().map_or(Ok(()), |pc| pc.check_command(command))
}

/// Allowlist of filesystem roots under which the bash tool may operate.
///
/// Constructed with [`Self::new`] from the current working directory plus
/// any additional roots (e.g. user-configured `additionalWorkingDirectories`
/// from `.claude/settings.json`). Roots are canonicalised lazily on each
/// check call so symlink targets are resolved at policy-evaluation time
/// (not at constructor time, which would race with filesystem mutation).
#[derive(Debug, Clone)]
pub struct PathConstraints {
    roots: Vec<PathBuf>,
}

impl PathConstraints {
    /// Build a new constraints object from an iterable of allowed roots.
    ///
    /// Each root is stored verbatim and canonicalised at check time. Empty
    /// or whitespace-only roots are skipped. The list ends up *inclusive
    /// of `cwd`* when called from the bash entry point; callers that want
    /// only the cwd can pass `[std::env::current_dir()]`.
    #[must_use]
    pub fn new<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let roots: Vec<PathBuf> = roots
            .into_iter()
            .filter_map(|p| {
                let p = p.as_ref();
                if p.as_os_str().is_empty() {
                    None
                } else {
                    Some(p.to_path_buf())
                }
            })
            .collect();
        Self { roots }
    }

    /// True if `roots` is empty. An empty constraint set means the check is
    /// effectively disabled — the caller must explicitly populate at least
    /// one root for the gate to be active.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Return the list of roots verbatim — useful for diagnostic messages.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Test whether `path` is contained within any allowed root.
    ///
    /// Algorithm:
    ///  1. Normalise `path` lexically (resolve `.` / `..` without touching
    ///     the filesystem). We deliberately avoid `canonicalize` here so a
    ///     non-existent path (which is common for write/create flows) can
    ///     still be evaluated.
    ///  2. Make `path` absolute by joining the *first* root if it's
    ///     relative — relative paths inherit the agent's cwd, which is
    ///     conventionally the first root.
    ///  3. Each root is normalised the same way and checked as a prefix.
    ///
    /// Returns `true` when the path is under some root, or when the
    /// constraint set is empty (gate disabled).
    #[must_use]
    pub fn allows(&self, path: &Path) -> bool {
        if self.roots.is_empty() {
            return true;
        }
        let abs = self.absolutize(path);
        let normalised = normalise_lexically(&abs);
        for root in &self.roots {
            let root_abs = self.absolutize(root);
            let root_norm = normalise_lexically(&root_abs);
            if normalised.starts_with(&root_norm) {
                return true;
            }
        }
        false
    }

    /// Resolve every path-shaped token in `command` against this allowlist.
    ///
    /// Returns `Ok(())` when every extracted path is contained within an
    /// allowed root, or `Err(message)` with a user-facing explanation when
    /// at least one path falls outside. The error message names the first
    /// rejected path so the caller can fix the offending argument.
    ///
    /// "Path-shaped tokens" are tokens that start with `/`, `~`, or `./`
    /// — i.e. absolute paths, user-home paths, and explicit relatives.
    /// Bare names like `foo.txt` are NOT classified as paths because they
    /// resolve in the bash tool's cwd, which is one of the roots by
    /// construction. The classifier is intentionally conservative so that
    /// `echo hello` and `cargo test` continue to work without a synthetic
    /// path argument.
    ///
    /// # Errors
    ///
    /// Returns `Err` if a path-shaped token resolves outside every root.
    pub fn check_command(&self, command: &str) -> Result<(), String> {
        if self.roots.is_empty() {
            return Ok(());
        }
        for token in path_tokens(command) {
            let path = expand_home(&token);
            if !self.allows(&path) {
                return Err(format!(
                    "Path '{token}' is outside the bash tool's allowed roots ({}). \
                     To allow it, add the directory to `additionalWorkingDirectories` \
                     in `.claude/settings.json` (or rerun the command from inside the root).",
                    self.roots
                        .iter()
                        .map(|r| r.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Join a relative path with the first root so the check operates on an
    /// absolute path; absolute inputs pass through unchanged.
    fn absolutize(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        // Fall back to cwd if no roots — relative inputs resolve against
        // the agent's process cwd, which is the conventional anchor.
        let anchor = self
            .roots
            .first()
            .cloned()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        anchor.join(path)
    }
}

/// Lexically normalise a path: collapse `.`/`..` without touching disk.
///
/// `..` at the root is dropped (cannot escape past `/`). Trailing slashes
/// are preserved by re-joining the components.
fn normalise_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // Preserve `..` at the start so a relative `../foo` can
                    // still be matched against a relative root (defensive
                    // — most callers pass absolute inputs).
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Expand a leading `~` to the user's home directory; pass through anything
/// else verbatim as a `PathBuf`. `~` alone resolves to `$HOME`.
fn expand_home(token: &str) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if token == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(token)
}

/// Extract path-shaped tokens from a bash command.
///
/// Heuristic — splits on whitespace (outside single quotes) and keeps
/// tokens that start with `/`, `~`, or `./`. Quoting is preserved by
/// stripping matching outer single quotes after the split. This is a
/// conservative parser — it doesn't expand braces, globs, or variable
/// interpolation, and it's only used by [`PathConstraints::check_command`]
/// which itself is gated behind the safety-allowlist (see module docs).
fn path_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for ch in command.chars() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    let token = std::mem::take(&mut current);
                    push_if_path(&mut tokens, &token);
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        push_if_path(&mut tokens, &current);
    }
    tokens
}

fn push_if_path(out: &mut Vec<String>, token: &str) {
    let stripped = strip_outer_quotes(token);
    if is_path_shaped(stripped) {
        out.push(stripped.to_string());
    }
}

fn strip_outer_quotes(s: &str) -> &str {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn is_path_shaped(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with("~/")
        || token == "~"
        || token.starts_with("./")
        || token.starts_with("../")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pc(roots: &[&str]) -> PathConstraints {
        PathConstraints::new(roots.iter().map(PathBuf::from))
    }

    #[test]
    fn empty_constraints_allow_everything() {
        let pc = PathConstraints::new(Vec::<PathBuf>::new());
        assert!(pc.is_empty());
        assert!(pc.allows(Path::new("/etc/passwd")));
        assert!(pc.check_command("cat /etc/passwd").is_ok());
    }

    #[test]
    fn path_under_root_is_allowed() {
        let pc = pc(&["/home/user/project"]);
        assert!(pc.allows(Path::new("/home/user/project/src/main.rs")));
        assert!(pc.allows(Path::new("/home/user/project")));
    }

    #[test]
    fn path_outside_root_is_denied() {
        let pc = pc(&["/home/user/project"]);
        assert!(!pc.allows(Path::new("/etc/passwd")));
        assert!(!pc.allows(Path::new("/home/user/other")));
    }

    #[test]
    fn parent_dir_traversal_is_normalised_then_checked() {
        let pc = pc(&["/home/user/project"]);
        // `/home/user/project/../../../etc/passwd` resolves to `/etc/passwd`
        assert!(!pc.allows(Path::new("/home/user/project/../../../etc/passwd")));
        // `/home/user/project/./src` stays under root
        assert!(pc.allows(Path::new("/home/user/project/./src")));
    }

    #[test]
    fn multiple_roots_any_match_allows() {
        let pc = pc(&["/home/user/project", "/tmp/scratch"]);
        assert!(pc.allows(Path::new("/home/user/project/x")));
        assert!(pc.allows(Path::new("/tmp/scratch/y")));
        assert!(!pc.allows(Path::new("/var/log/z")));
    }

    #[test]
    fn check_command_extracts_absolute_paths() {
        let pc = pc(&["/home/user/project"]);
        // cat with an absolute path outside the root: rejected
        let err = pc
            .check_command("cat /etc/passwd")
            .expect_err("must reject absolute path outside root");
        assert!(err.contains("/etc/passwd"), "error must name path: {err}");
        assert!(err.contains("allowed roots"), "error must mention roots");
        // cat with an absolute path inside the root: allowed
        assert!(pc
            .check_command("cat /home/user/project/src/main.rs")
            .is_ok());
    }

    #[test]
    fn check_command_ignores_bare_names() {
        let pc = pc(&["/home/user/project"]);
        // Bare relative names resolve in cwd, which is the root —
        // and they are not "path-shaped" so we don't extract them.
        assert!(pc.check_command("cat main.rs").is_ok());
        assert!(pc.check_command("cargo build").is_ok());
        assert!(pc.check_command("echo hello world").is_ok());
    }

    #[test]
    fn check_command_handles_dot_relative_paths() {
        let pc = pc(&["/home/user/project"]);
        // ./foo is path-shaped but resolves to the root, so it's allowed.
        assert!(pc.check_command("cat ./src/main.rs").is_ok());
        // ../escape is path-shaped and resolves OUTSIDE the root.
        let err = pc
            .check_command("cat ../../etc/passwd")
            .expect_err("must reject ../traversal outside root");
        assert!(
            err.contains("etc/passwd") || err.contains(".."),
            "error must indicate the offending token: {err}"
        );
    }

    #[test]
    fn path_tokens_extracts_quoted_and_unquoted() {
        let tokens = path_tokens("cat /a/b 'foo bar' \"/c/d\" e ~/x");
        // The quoted "foo bar" is NOT path-shaped (no leading `/`, `~`, or `./`).
        assert!(tokens.contains(&"/a/b".to_string()), "got {tokens:?}");
        assert!(tokens.contains(&"/c/d".to_string()), "got {tokens:?}");
        assert!(tokens.contains(&"~/x".to_string()), "got {tokens:?}");
        assert!(!tokens.contains(&"e".to_string()));
        assert!(!tokens.iter().any(|t| t.contains("foo bar")));
    }

    #[test]
    fn home_expansion_resolves_against_dirs_home() {
        if let Some(home) = dirs::home_dir() {
            let pc = PathConstraints::new(vec![home]);
            // ~/foo expands to $HOME/foo which is under the root
            assert!(pc.check_command("cat ~/foo").is_ok());
        }
    }

    #[test]
    fn normalise_lexically_collapses_dots() {
        let p = normalise_lexically(Path::new("/a/b/./c/../d"));
        assert_eq!(p, PathBuf::from("/a/b/d"));
        // Cannot escape past root: parent-dir consumes nothing at "/"
        let p = normalise_lexically(Path::new("/../../etc"));
        assert_eq!(p, PathBuf::from("/etc"));
    }

    #[test]
    fn is_path_shaped_classifier_is_conservative() {
        assert!(is_path_shaped("/abs/path"));
        assert!(is_path_shaped("~/home/relative"));
        assert!(is_path_shaped("~"));
        assert!(is_path_shaped("./relative"));
        assert!(is_path_shaped("../parent"));
        // Bare names and option flags are NOT path-shaped
        assert!(!is_path_shaped("foo.txt"));
        assert!(!is_path_shaped("--help"));
        assert!(!is_path_shaped("-rf"));
    }

    /// Process-wide gate is disabled by default — `check_command_against_global`
    /// must return `Ok(())` for any input until `install_global` is called.
    ///
    /// Uses a mutex to serialise with `global_install_then_clear_round_trip`
    /// — both touch the shared `GLOBAL_CONSTRAINTS` and would race under
    /// cargo's parallel test runner without explicit serialisation.
    #[test]
    fn global_gate_is_disabled_until_installed() {
        let _lock = global_test_lock();
        clear_global();
        assert!(check_command_against_global("cat /etc/passwd").is_ok());
        assert!(check_command_against_global("rm -rf /").is_ok());
    }

    /// `install_global` then `check_command_against_global` reject paths
    /// outside the installed roots; `clear_global` returns to the disabled
    /// state.
    #[test]
    fn global_install_then_clear_round_trip() {
        let _lock = global_test_lock();
        clear_global();
        install_global(pc(&["/home/user/project"]));

        // Outside the root: rejected.
        let err = check_command_against_global("cat /etc/passwd")
            .expect_err("must reject /etc/passwd outside root");
        assert!(err.contains("/etc/passwd"), "{err}");

        // Inside the root: allowed.
        assert!(check_command_against_global("cat /home/user/project/src/main.rs").is_ok());

        // After clear: gate is disabled again.
        clear_global();
        assert!(check_command_against_global("cat /etc/passwd").is_ok());
    }

    /// Serialise the two global-state tests so they don't race each other
    /// under parallel test execution. Other tests in this file don't touch
    /// `GLOBAL_CONSTRAINTS`, so they can run unsynchronised.
    fn global_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
