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
pub fn git_clone(
    url: &str,
    dest: &Path,
    git_ref: Option<&str>,
) -> Result<String, PluginError> {
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

/// Recursively copy a directory.
///
/// # Errors
///
/// Returns an error if any directory creation or file copy operation fails.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
