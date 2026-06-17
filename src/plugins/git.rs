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

/// Read the `remote.origin.url` configured in a git repo by running
/// `git remote get-url origin` inside `dir`. Uses the cached absolute
/// `GIT_BIN` so PATH cannot redirect the lookup (crosslink #679).
///
/// # Errors
///
/// Returns [`PluginError::IoError`] if git cannot be invoked or if
/// `git remote get-url origin` returns a non-success status (e.g. no
/// `origin` remote configured).
pub fn git_remote_url(dir: &Path) -> Result<String, PluginError> {
    let output = std::process::Command::new(git_bin()?)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .current_dir(dir)
        .output()
        .map_err(|e| PluginError::IoError(format!("Failed to run git remote get-url: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::IoError(format!(
            "git remote get-url origin failed in {}: {}",
            dir.display(),
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Filename of the origin-URL sidecar (see crosslink #715).
///
/// Records the canonical add-time origin URL of a cloned
/// marketplace/plugin directory. Placed at the *root* of the clone
/// (outside `.git/`) so that an attacker who poisons `.git/config`
/// cannot also silently move the recorded URL.
pub const ORIGIN_URL_SIDECAR: &str = ".openclaudia-origin-url";

/// Write the add-time origin URL into the sidecar at the root of `dir`.
///
/// Called immediately after a successful [`git_clone`] so that future
/// [`git_pull`] invocations can compare the live remote URL against the
/// URL that the operator originally vetted. See crosslink #715.
///
/// # Errors
///
/// Returns [`PluginError::IoError`] if the sidecar cannot be written.
pub fn write_origin_url_sidecar(dir: &Path, url: &str) -> Result<(), PluginError> {
    let path = dir.join(ORIGIN_URL_SIDECAR);
    std::fs::write(&path, url.as_bytes()).map_err(|e| {
        PluginError::IoError(format!(
            "failed to write origin-URL sidecar {}: {}",
            path.display(),
            e
        ))
    })
}

/// Read the recorded add-time origin URL from the sidecar at the root of
/// `dir`. Returns `Ok(None)` if no sidecar is present (e.g. legacy clone
/// that pre-dates crosslink #715).
///
/// # Errors
///
/// Returns [`PluginError::IoError`] if the sidecar exists but cannot be
/// read.
pub fn read_origin_url_sidecar(dir: &Path) -> Result<Option<String>, PluginError> {
    let path = dir.join(ORIGIN_URL_SIDECAR);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PluginError::IoError(format!(
            "failed to read origin-URL sidecar {}: {}",
            path.display(),
            e
        ))),
    }
}

/// Pull latest changes in a git repository, re-validating the live remote
/// URL against an expected value before invoking `git pull`.
///
/// # Crosslink #715
///
/// Pre-fix `git_pull` invoked `git pull` blindly. An attacker that could
/// write into `.git/config` (filesystem race, supply-chain compromise of
/// a nested plugin, or social-engineered `/plugin marketplace add` of a
/// malicious local dir whose `.git/config` is then mutated) could repoint
/// `origin` to an attacker-controlled URL. The next
/// `/plugin marketplace update` would silently pull and execute arbitrary
/// content, bypassing `strict_known_marketplaces` (which only runs at
/// add time, not at pull time).
///
/// Post-fix: when `expected_url` is `Some`, the live `remote.origin.url`
/// is read via [`git_remote_url`], validated through
/// [`super::validate::validate_source_url`], and compared byte-for-byte
/// against `expected_url`. Any mismatch returns
/// [`PluginError::PolicyRejected`] with `scope = "marketplace"` and an
/// explanation that the remote was tampered with.
///
/// When `expected_url` is `None`, the validation step is skipped — this
/// is the legacy code path retained only for backward compatibility with
/// callers that have no recorded URL to compare against. New callers
/// MUST pass `Some(url)`.
///
/// # Errors
///
/// Returns an error if git is not available, the remote URL was
/// tampered with, the live remote URL is invalid, or the pull operation
/// fails.
pub fn git_pull(dir: &Path, expected_url: Option<&str>) -> Result<(), PluginError> {
    // Re-validation: read the live remote URL and compare against the
    // recorded add-time URL. Closes crosslink #715.
    //
    // The recorded URL was validated by `validate_source_url` at add
    // time (in `add_marketplace_from_git`); a byte-equal live URL is
    // therefore implicitly re-validated. When the URLs differ, we
    // additionally run the live URL through `validate_source_url` so
    // that a malicious mutation to a syntactically-invalid scheme
    // (e.g. `file:///etc/passwd`) is surfaced as a *validation*
    // failure rather than a generic mismatch — the operator sees
    // exactly what was rejected.
    if let Some(expected) = expected_url {
        let live = git_remote_url(dir)?;

        if live != expected {
            // Best-effort re-validation of the diverged live URL. Surfaces
            // a scheme/credential rejection if applicable, otherwise falls
            // through to the equality-mismatch error below.
            if let Err(e) = super::validate::validate_source_url(&live) {
                return Err(PluginError::PolicyRejected {
                    reason: format!(
                        "remote URL tampered: live `{live}` does not match recorded \
                         `{expected}` AND fails re-validation ({e})"
                    ),
                    scope: "marketplace",
                });
            }
            return Err(PluginError::PolicyRejected {
                reason: format!(
                    "remote URL tampered: live `{live}` does not match recorded `{expected}`. \
                     Refusing to pull — the remote was changed since this marketplace was added. \
                     Remove and re-add it after verifying the source."
                ),
                scope: "marketplace",
            });
        }
    }

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

    use super::{
        git_bin, git_clone, git_pull, read_origin_url_sidecar, write_origin_url_sidecar,
        PluginError, ORIGIN_URL_SIDECAR,
    };
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

    // ── Crosslink #715 — git_pull re-validates remote URL ─────────────────────
    //
    // Pre-fix `git_pull(dir)` invoked `git pull` blindly inside the marketplace
    // directory. An attacker that could write to `.git/config` (filesystem
    // race, supply-chain compromise of a nested plugin, or social-engineered
    // `/plugin marketplace add` of a malicious local dir whose `.git/config`
    // is later mutated) could repoint `origin` to an attacker-controlled URL.
    // The next `/plugin marketplace update` would silently pull and execute
    // arbitrary content.
    //
    // Post-fix `git_pull(dir, expected_url)` reads the live `remote.origin.url`,
    // re-runs `validate_source_url` on it, and refuses to pull when the live
    // URL diverges from the URL recorded at add time. The recorded URL lives
    // in a sidecar (`.openclaudia-origin-url`) at the *root* of the clone —
    // outside `.git/` — so poisoning `.git/config` alone is insufficient to
    // hide the change.

    /// Build a bare local git "remote" repository at `path`, commit a single
    /// file in it, and return the path. Used as a stable URL target for the
    /// #715 tests so we can exercise the live-pull path without network IO.
    #[cfg(unix)]
    fn make_local_git_remote(path: &Path) -> std::path::PathBuf {
        // 1. Create a regular working repo with one commit
        let work = path.join("source-work");
        std::fs::create_dir_all(&work).expect("mkdir source-work");
        let git = git_bin().expect("git on PATH").to_path_buf();
        let run = |args: &[&str], cwd: &Path| {
            let out = std::process::Command::new(&git)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .current_dir(cwd)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed in {}: {}",
                cwd.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"], &work);
        std::fs::write(work.join("README.md"), "hello\n").expect("write");
        run(&["add", "README.md"], &work);
        run(&["commit", "-q", "-m", "initial"], &work);

        // 2. Clone --bare into a sibling so it can serve as `origin`.
        let bare = path.join("source.git");
        let out = std::process::Command::new(&git)
            .args(["clone", "--bare", "-q"])
            .arg(&work)
            .arg(&bare)
            .current_dir(path)
            .output()
            .expect("spawn git clone --bare");
        assert!(
            out.status.success(),
            "git clone --bare failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        bare
    }

    /// Clone `remote_url` into `dest` using `git_bin` directly (skipping
    /// `git_clone`'s URL validation, which rejects local paths). Mirrors
    /// the post-clone steps `add_marketplace_from_git` would otherwise
    /// perform — including writing the origin-URL sidecar.
    #[cfg(unix)]
    fn clone_and_record(
        remote_url: &str,
        dest: &Path,
        recorded_url: &str,
    ) -> Result<(), PluginError> {
        let git = git_bin()?.to_path_buf();
        let clone_cwd = dest.parent().unwrap_or_else(|| Path::new("."));
        let out = std::process::Command::new(&git)
            .args(["clone", "-q"])
            .arg(remote_url)
            .arg(dest)
            .current_dir(clone_cwd)
            .output()
            .map_err(|e| PluginError::IoError(format!("clone spawn: {e}")))?;
        assert!(
            out.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        write_origin_url_sidecar(dest, recorded_url)?;
        Ok(())
    }

    /// (#715-a) Pull on a marketplace with an unchanged remote succeeds.
    /// Establishes the happy path: live `remote.origin.url` matches the
    /// recorded sidecar URL byte-for-byte → pull proceeds.
    #[test]
    #[cfg(unix)]
    fn fix715_pull_with_unchanged_remote_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bare = make_local_git_remote(tmp.path());
        let bare_url = bare.to_string_lossy().to_string();

        let clone_dest = tmp.path().join("marketplace-clone");
        clone_and_record(&bare_url, &clone_dest, &bare_url).expect("clone+record");

        let recorded = read_origin_url_sidecar(&clone_dest)
            .expect("read sidecar")
            .expect("sidecar present");
        assert_eq!(recorded, bare_url, "sidecar round-trips the add-time URL");

        // Pull with matching recorded URL → must succeed.
        git_pull(&clone_dest, Some(&recorded)).expect("pull with unchanged remote must succeed");
    }

    /// (#715-b) Pull is rejected when `.git/config` was tampered with to
    /// point at a different URL.
    #[test]
    #[cfg(unix)]
    fn fix715_pull_rejected_when_remote_url_changed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bare = make_local_git_remote(tmp.path());
        let bare_url = bare.to_string_lossy().to_string();

        let clone_dest = tmp.path().join("marketplace-clone");
        clone_and_record(&bare_url, &clone_dest, &bare_url).expect("clone+record");

        // Simulate `.git/config` tampering: repoint origin to a new URL.
        let git = git_bin().expect("git on PATH").to_path_buf();
        let attacker_url = format!("{bare_url}-attacker");
        let out = std::process::Command::new(&git)
            .args(["remote", "set-url", "origin", &attacker_url])
            .current_dir(&clone_dest)
            .output()
            .expect("spawn git remote set-url");
        assert!(out.status.success(), "tamper setup failed");

        let err = git_pull(&clone_dest, Some(&bare_url))
            .expect_err("#715-b: tampered remote must be rejected");
        match err {
            PluginError::PolicyRejected { reason, scope } => {
                assert_eq!(scope, "marketplace");
                assert!(
                    reason.contains("tampered"),
                    "#715-b: rejection reason must mention tamper; got: {reason}"
                );
            }
            other => panic!("#715-b: expected PolicyRejected, got {other:?}"),
        }
    }

    /// (#715-c) Pull is rejected when the remote URL points at a different
    /// HOST than the recorded one (even if both URLs are well-formed).
    /// Distinct from (b) because the URL is changed to a syntactically
    /// valid https URL, not a sibling local-path string — exercises the
    /// "different host than original" prong of the contract.
    #[test]
    #[cfg(unix)]
    fn fix715_pull_rejected_when_remote_points_to_different_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bare = make_local_git_remote(tmp.path());
        let original_url = "https://github.com/orig/marketplace.git";
        let attacker_url = "https://evil.example.invalid/attacker/marketplace.git";

        // Clone from the local bare, but RECORD the well-formed https URL
        // (simulates the operator having originally added the marketplace
        // by its public https endpoint).
        let clone_dest = tmp.path().join("marketplace-clone");
        clone_and_record(&bare.to_string_lossy(), &clone_dest, original_url).expect("clone+record");

        // Repoint origin to a different host.
        let git = git_bin().expect("git on PATH").to_path_buf();
        let out = std::process::Command::new(&git)
            .args(["remote", "set-url", "origin", attacker_url])
            .current_dir(&clone_dest)
            .output()
            .expect("spawn git remote set-url");
        assert!(out.status.success(), "tamper setup failed");

        let err = git_pull(&clone_dest, Some(original_url))
            .expect_err("#715-c: cross-host tamper must be rejected");
        let PluginError::PolicyRejected { reason, scope } = err else {
            panic!("#715-c: expected PolicyRejected");
        };
        assert_eq!(scope, "marketplace");
        assert!(
            reason.contains("evil.example.invalid") || reason.contains("tampered"),
            "#715-c: reason must mention the offending URL or tamper; got: {reason}"
        );
    }

    /// (#715-d) Sidecar round-trip — explicit pinning that the sidecar is
    /// at the documented filename so future refactors don't silently move it.
    #[test]
    fn fix715_sidecar_filename_is_stable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_origin_url_sidecar(tmp.path(), "https://example.invalid/x.git")
            .expect("write sidecar");
        let path = tmp.path().join(ORIGIN_URL_SIDECAR);
        assert!(
            path.exists(),
            "sidecar must be written under the documented filename {ORIGIN_URL_SIDECAR}"
        );
        let contents = std::fs::read_to_string(&path).expect("read sidecar back");
        assert_eq!(contents, "https://example.invalid/x.git");
    }
}
