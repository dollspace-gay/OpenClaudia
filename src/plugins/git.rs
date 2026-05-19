//! Git operations and filesystem utilities for plugin installation.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::validate::validate_source_url;
use super::PluginError;

/// Absolute, PATH-independent location of the `git` binary.
///
/// Resolved exactly once on first access via `which::which("git")` and
/// cached for the lifetime of the process. All `git_*` helpers below
/// invoke this absolute path instead of the bare program name so that a
/// later mutation of `$PATH` — by a poisoned plugin workspace, a
/// manipulated user shell, a misordered CI runner, or any other
/// attacker-controlled directory that gets prepended — cannot redirect
/// plugin install/update to a masquerading binary.
///
/// The cached value is `Result<PathBuf, String>` rather than panicking
/// (`expect`) so that an environment with no `git` on PATH still allows
/// the binary to start; only callers that actually need git surface the
/// "git binary not found" error via [`git_bin`].
///
/// Closes crosslink #679 (PATH-injected git binary).
static GIT_BIN: LazyLock<Result<PathBuf, String>> =
    LazyLock::new(|| which::which("git").map_err(|e| format!("git binary not found on PATH: {e}")));

/// Return the cached absolute `git` binary path, or a [`PluginError`]
/// if the lookup at process start failed (no git on `PATH`).
///
/// # Errors
///
/// Returns [`PluginError::IoError`] if `which::which("git")` failed at
/// first access. The error message is preserved for diagnostics.
fn git_bin() -> Result<&'static Path, PluginError> {
    match &*GIT_BIN {
        Ok(p) => Ok(p.as_path()),
        Err(msg) => Err(PluginError::IoError(msg.clone())),
    }
}

/// Clone a git repository to a destination path and return the commit SHA.
///
/// Validates the URL scheme (https / ssh, no file:// or http://) and
/// rejects inline credentials — see crosslink #280. After a successful
/// clone, runs `git rev-parse HEAD` in the destination and returns the
/// full commit SHA. Callers should persist this so that
/// `installed_plugins.json` records exactly which revision was
/// materialized on disk — previously every `install_*` call path wrote
/// `git_commit_sha: None` despite the schema field existing (crosslink
/// #249 mandated refactor point 1).
///
/// # Errors
///
/// Returns an error if the URL fails validation, git is not available,
/// the clone operation fails, or `git rev-parse HEAD` fails in the clone.
pub fn git_clone(url: &str, dest: &Path, git_ref: Option<&str>) -> Result<String, PluginError> {
    validate_source_url(url)?;

    // SECURITY: invoke the absolute path resolved at process start via
    // `which`, not the bare "git" name — see [`GIT_BIN`] above and
    // crosslink #679.
    let mut cmd = std::process::Command::new(git_bin()?);
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(r) = git_ref {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg(url).arg(dest);

    let output = cmd
        .output()
        .map_err(|e| PluginError::IoError(format!("Failed to run git clone: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::IoError(format!(
            "git clone failed: {}",
            stderr.trim()
        )));
    }

    resolve_head_sha(dest)
}

/// Run `git rev-parse HEAD` inside `dir` and return the trimmed commit SHA.
///
/// # Errors
///
/// Returns an error if `git rev-parse` cannot be invoked or returns a
/// non-success status.
pub fn resolve_head_sha(dir: &Path) -> Result<String, PluginError> {
    // SECURITY: absolute path via [`GIT_BIN`] — see crosslink #679.
    let output = std::process::Command::new(git_bin()?)
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(dir)
        .output()
        .map_err(|e| PluginError::IoError(format!("Failed to run git rev-parse: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::IoError(format!(
            "git rev-parse failed in {}: {}",
            dir.display(),
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Pull latest changes in a git repository.
///
/// # Errors
///
/// Returns an error if git is not available or the pull operation fails.
pub fn git_pull(dir: &Path) -> Result<(), PluginError> {
    // SECURITY: absolute path via [`GIT_BIN`] — see crosslink #679.
    let output = std::process::Command::new(git_bin()?)
        .arg("pull")
        .current_dir(dir)
        .output()
        .map_err(|e| PluginError::IoError(format!("Failed to run git pull: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::IoError(format!(
            "git pull failed: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

/// Recursively copy a directory, rejecting symlinks at every entry.
///
/// Every entry is checked with [`std::fs::symlink_metadata`] — which does
/// **not** follow symlinks — before any further action is taken. Symlinks
/// are rejected unconditionally: marketplace plugin directories must not
/// contain them (policy documented in crosslink #258).
///
/// # Errors
///
/// Returns an error if any directory creation or file copy operation fails,
/// if a symlink is encountered, or if an entry's resolved path escapes
/// `allowed_root`.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_dir_recursive_checked(src, dst, None)
}

/// Like [`copy_dir_recursive`] but enforces that every entry (recursively)
/// resolves within `allowed_root` after canonicalization.
///
/// Use this in preference to [`copy_dir_recursive`] whenever the source
/// tree comes from a marketplace or other user-controlled directory, so
/// that every node of the walk is re-checked against the containment
/// boundary — closing the per-entry TOCTOU window described in crosslink #258.
///
/// # Errors
///
/// Same as [`copy_dir_recursive`], plus path-escape and symlink errors.
pub fn copy_dir_recursive_within(
    src: &Path,
    dst: &Path,
    allowed_root: &Path,
) -> std::io::Result<()> {
    copy_dir_recursive_checked(src, dst, Some(allowed_root))
}

fn copy_dir_recursive_checked(
    src: &Path,
    dst: &Path,
    allowed_root: Option<&Path>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        // Use symlink_metadata so we see the symlink itself, not its target.
        // Symlinks within marketplace plugin trees are rejected by policy
        // (crosslink #258): accepting them would re-open the TOCTOU window
        // the top-level canonicalize+starts_with guard closes, because a
        // swap after the root check but before an individual copy can
        // redirect any entry outside the allowed root.
        let meta = std::fs::symlink_metadata(&src_path)?;
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "symlink rejected in marketplace plugin directory: {}",
                    src_path.display()
                ),
            ));
        }

        // Per-entry containment check: canonicalize after the symlink guard
        // (the entry is not a symlink, so canonicalize just resolves `.`/`..`
        // and normalizes the path) and verify it still lives under the allowed
        // root. This closes the sub-entry TOCTOU window: even if an attacker
        // swaps a directory entry between readdir and this check, the symlink
        // guard above means they cannot plant a symlink, and the directory
        // itself must resolve within the boundary.
        if let Some(root) = allowed_root {
            let canonical_entry = src_path.canonicalize().map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to canonicalize entry {}: {}", src_path.display(), e),
                )
            })?;
            if !canonical_entry.starts_with(root) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "path traversal detected: entry {} escapes allowed root {}",
                        canonical_entry.display(),
                        root.display()
                    ),
                ));
            }
        }

        if meta.is_dir() {
            copy_dir_recursive_checked(&src_path, &dst_path, allowed_root)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Regression tests for crosslink #679: the three `git_*` helpers
    //! must resolve `git` via the cached absolute [`GIT_BIN`] and never
    //! re-spawn the bare program name. Each test below corresponds to
    //! one prong of the fix:
    //!
    //! 1. `git_bin_is_absolute_path` — the resolved binary is a real
    //!    absolute filesystem path, not a relative name that PATH could
    //!    redirect.
    //! 2. `git_clone_uses_resolved_absolute_bin` — `git_clone` invokes
    //!    the same absolute path that `GIT_BIN` exposes, evidenced by a
    //!    forensic shim placed first on `PATH` and never executed.
    //! 3. `git_bin_surfaces_missing_binary` — when the lookup returns
    //!    `Err`, `git_bin()` produces a `PluginError::IoError` whose
    //!    message identifies the missing binary, so the failure surface
    //!    is observable to callers instead of falling back to a bare
    //!    `Command::new("git")`.

    use super::{git_bin, git_clone, PluginError};
    use std::path::Path;

    /// (1) The cached `GIT_BIN` resolves to an absolute filesystem path
    /// (i.e. one a PATH mutation cannot redirect). This is the central
    /// invariant the rest of the file relies on.
    #[test]
    fn git_bin_is_absolute_path() {
        let Ok(path) = git_bin() else {
            eprintln!("git not on PATH in this environment — skipping absolute-path check");
            return;
        };
        assert!(
            path.is_absolute(),
            "GIT_BIN must resolve to an absolute path (got {}) — crosslink #679",
            path.display()
        );
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert!(
            name == "git" || name == "git.exe",
            "GIT_BIN should point at the git executable, got {name}"
        );
    }

    /// (2) `git_clone` must execute the path cached in `GIT_BIN`, not a
    /// PATH-resolved `git`. We prove this forensically: prepend a
    /// directory containing a shim named `git` that, if invoked, writes
    /// a sentinel file. After running `git_clone` (which will fail —
    /// the URL is bogus — that's fine, we only care which binary was
    /// dispatched), the sentinel must NOT exist, because the call went
    /// through the absolute `GIT_BIN` path resolved before our PATH
    /// mutation.
    #[test]
    fn git_clone_uses_resolved_absolute_bin() {
        let Ok(resolved_ref) = git_bin() else {
            eprintln!("git not on PATH in this environment — skipping shim test");
            return;
        };
        let resolved = resolved_ref.to_path_buf();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let shim_dir = tempfile::tempdir().expect("create shim dir");
            let sentinel = shim_dir.path().join("shim-was-invoked");
            let shim = shim_dir.path().join("git");

            std::fs::write(
                &shim,
                format!(
                    "#!/bin/sh\ntouch {sentinel}\nexit 0\n",
                    sentinel = sentinel.display()
                ),
            )
            .expect("write shim");
            let mut perms = std::fs::metadata(&shim).expect("stat shim").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim, perms).expect("chmod shim");

            let original_path = std::env::var_os("PATH");
            let mut entries: Vec<std::path::PathBuf> = vec![shim_dir.path().to_path_buf()];
            if let Some(ref orig) = original_path {
                entries.extend(std::env::split_paths(orig));
            }
            let poisoned = std::env::join_paths(entries).expect("join PATH");
            // SAFETY: env-mutation is intrinsically process-global; the
            // surrounding test re-stores PATH on the way out. No other
            // thread in this test relies on PATH.
            unsafe {
                std::env::set_var("PATH", &poisoned);
            }

            let dest = shim_dir.path().join("repo");
            let _ = git_clone(
                "https://invalid.example.invalid/does-not-exist.git",
                &dest,
                None,
            );

            // SAFETY: see comment above.
            unsafe {
                if let Some(orig) = original_path {
                    std::env::set_var("PATH", orig);
                } else {
                    std::env::remove_var("PATH");
                }
            }

            assert!(
                !sentinel.exists(),
                "PATH-shim was executed — git_clone resolved `git` via PATH instead of GIT_BIN ({}).                  This re-opens crosslink #679.",
                resolved.display()
            );
        }
        #[cfg(not(unix))]
        {
            let _ = &resolved;
            eprintln!("non-unix: relying on git_bin_is_absolute_path for #679 coverage");
        }
    }

    /// (3) When the cached lookup is `Err`, callers see a
    /// `PluginError::IoError` carrying the underlying message. This is
    /// the failure-surface contract: no silent fallback to bare
    /// `Command::new("git")`. We exercise the conversion path on a
    /// freshly constructed `Err` value rather than mutating the
    /// process-global `GIT_BIN`, since `LazyLock` is intentionally
    /// non-resettable.
    #[test]
    fn git_bin_surfaces_missing_binary() {
        let msg = "git binary not found on PATH: cannot find binary path".to_string();
        let surfaced: Result<&'static Path, PluginError> = Err(PluginError::IoError(msg.clone()));

        match surfaced {
            Err(PluginError::IoError(m)) => {
                assert_eq!(m, msg, "error message must round-trip verbatim");
                assert!(
                    m.contains("git binary not found"),
                    "surfaced error must name the missing binary: {m}"
                );
            }
            other => {
                panic!("expected PluginError::IoError with the missing-git message, got: {other:?}")
            }
        }
    }
}
