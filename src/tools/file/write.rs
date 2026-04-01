use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Write content to a file
pub fn execute_write_file(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return ("Missing 'path' argument".to_string(), true);
    };

    // Reject path traversal attempts (relative paths with ..)
    let p = Path::new(path);
    if !p.is_absolute() {
        return (
            format!("Path must be absolute, got relative path: '{path}'"),
            true,
        );
    }

    // Reject path traversal attempts (../ in path)
    if p.components().any(|c| c == std::path::Component::ParentDir) {
        return (format!("Path traversal not allowed: '{path}'"), true);
    }

    // Resolve symlinks when possible; for new files use the path as-is
    let canonical = match std::fs::canonicalize(p) {
        Ok(canon) => canon,
        Err(_) => {
            // File doesn't exist yet -- try to resolve the parent
            if let Some(parent) = p.parent() {
                std::fs::canonicalize(parent).map_or_else(
                    // Parent doesn't exist either -- allowed (write_file creates dirs)
                    // but only if path is absolute (no relative traversal)
                    |_| std::path::PathBuf::from(path),
                    |canon_parent| canon_parent.join(p.file_name().unwrap_or_default()),
                )
            } else {
                return (format!("Invalid path: '{path}'"), true);
            }
        }
    };
    let path = canonical.to_string_lossy().to_string();
    let path = path.as_str();

    let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
        return ("Missing 'content' argument".to_string(), true);
    };

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(path) {
        return (msg, true);
    }

    // Read existing content for diff tracking
    let old_lines = fs::read_to_string(path)
        .map(|c| u32::try_from(c.lines().count()).unwrap_or(u32::MAX))
        .unwrap_or(0);
    let new_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);

    // Create parent directories if needed
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                return (format!("Failed to create directories: {e}"), true);
            }
        }
    }

    match fs::write(path, content) {
        Ok(()) => {
            // Record diff stats
            crate::guardrails::record_file_modification(path, new_lines, old_lines);

            let mut result = format!("Successfully wrote {} bytes to '{}'", content.len(), path);
            if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                let _ = write!(result, "\n\nWarning: {}", warning.message);
            }
            (result, false)
        }
        Err(e) => (format!("Failed to write file '{path}': {e}"), true),
    }
}
