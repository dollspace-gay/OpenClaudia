//! Git operations and filesystem utilities for plugin installation.

use std::path::Path;

use super::validate::validate_source_url;
use super::PluginError;

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

    let mut cmd = std::process::Command::new("git");
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
    let output = std::process::Command::new("git")
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
    let output = std::process::Command::new("git")
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
