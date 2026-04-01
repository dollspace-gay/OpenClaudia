//! Git operations and filesystem utilities for plugin installation.

use std::path::Path;

use super::PluginError;

/// Clone a git repository to a destination path.
///
/// # Errors
///
/// Returns an error if git is not available or the clone operation fails.
pub fn git_clone(url: &str, dest: &Path, git_ref: Option<&str>) -> Result<(), PluginError> {
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
    Ok(())
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
