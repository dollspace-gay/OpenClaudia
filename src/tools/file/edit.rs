use super::READ_TRACKER;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Edit a file by replacing text
pub(crate) fn execute_edit_file(args: &HashMap<String, Value>) -> (String, bool) {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'path' argument".to_string(), true),
    };

    // Reject path traversal attempts (relative paths with ..)
    let p = Path::new(path);
    if !p.is_absolute() {
        return (
            format!("Path must be absolute, got relative path: '{}'", path),
            true,
        );
    }

    // Resolve symlinks to prevent symlink-based path traversal.
    let canonical = match std::fs::canonicalize(p) {
        Ok(canon) => canon,
        Err(_) => {
            // File doesn't exist — try to resolve the parent directory
            if let Some(parent) = p.parent() {
                match std::fs::canonicalize(parent) {
                    Ok(canon_parent) => canon_parent.join(p.file_name().unwrap_or_default()),
                    Err(_) => {
                        return (
                            format!("Cannot resolve path '{}': parent directory does not exist", path),
                            true,
                        );
                    }
                }
            } else {
                return (format!("Invalid path: '{}'", path), true);
            }
        }
    };
    let path = canonical.to_string_lossy().to_string();
    let path = path.as_str();

    // ENFORCE: Must read file before editing
    // This prevents the model from making edits based on hallucinated file contents
    if !READ_TRACKER.has_been_read(Path::new(path)) {
        return (
            format!(
                "You must read '{}' before editing it. Use read_file first to see the actual contents.",
                path
            ),
            true,
        );
    }

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(path) {
        return (msg, true);
    }

    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'old_string' argument".to_string(), true),
    };

    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'new_string' argument".to_string(), true),
    };

    // Read the file
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("Failed to read file '{}': {}", path, e), true),
    };

    // Check if old_string exists
    if !content.contains(old_string) {
        return (
            format!(
                "Could not find the specified text in '{}'. Make sure old_string matches exactly.",
                path
            ),
            true,
        );
    }

    // Count occurrences
    let count = content.matches(old_string).count();
    if count > 1 {
        return (format!("Found {} occurrences of the text. Please provide a more specific old_string that matches uniquely.", count), true);
    }

    // Track diff: lines removed vs added
    let lines_removed = old_string.lines().count() as u32;
    let lines_added = new_string.lines().count() as u32;

    // Make the replacement
    let new_content = content.replacen(old_string, new_string, 1);

    // Write back
    match fs::write(path, &new_content) {
        Ok(()) => {
            // Record diff stats
            crate::guardrails::record_file_modification(path, lines_added, lines_removed);

            // Build diff data for color rendering in the CLI
            let diff_json = serde_json::json!({
                "path": path,
                "old": old_string,
                "new": new_string,
            });
            let mut result = format!(
                "Successfully edited '{}'. Replaced {} chars with {} chars.\n@@DIFF_START@@\n{}\n@@DIFF_END@@",
                path,
                old_string.len(),
                new_string.len(),
                diff_json,
            );
            if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                result.push_str(&format!("\n\nWarning: {}", warning.message));
            }
            (result, false)
        }
        Err(e) => (format!("Failed to write file '{}': {}", path, e), true),
    }
}
