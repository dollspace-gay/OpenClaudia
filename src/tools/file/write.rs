use super::resolve_path;
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

    let p = match resolve_path(path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    // Resolve symlinks when possible; for new files use the path as-is
    let canonical = match std::fs::canonicalize(&p) {
        Ok(canon) => canon,
        Err(_) => {
            // File doesn't exist yet -- try to resolve the parent
            if let Some(parent) = p.parent() {
                std::fs::canonicalize(parent).map_or_else(
                    // Parent doesn't exist either -- allowed (write_file creates dirs)
                    |_| p.clone(),
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
        .map_or(0, |c| u32::try_from(c.lines().count()).unwrap_or(u32::MAX));
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_args(path: &str, content: &str) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("path".to_string(), serde_json::json!(path));
        m.insert("content".to_string(), serde_json::json!(content));
        m
    }

    // =========================================================================
    // Behavior 6: write creates parent directories when missing
    // =========================================================================

    #[test]
    fn write_creates_parent_directories_recursively() {
        // Behavior 6: OC calls create_dir_all before writing, matching CC's
        // mkdir-p semantics.
        let dir = TempDir::new().expect("tempdir");
        let deep = dir.path().join("a").join("b").join("c").join("file.txt");
        let args = make_args(&deep.to_string_lossy(), "hello");
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "deep path write must succeed: {msg}");
        assert!(
            std::fs::read_to_string(&deep).expect("read back") == "hello",
            "content correct"
        );
    }

    #[test]
    fn write_success_message_contains_byte_count_and_path() {
        // Behavior 6 output contract: "Successfully wrote {N} bytes to '{path}'"
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("out.txt");
        let content = "abc";
        let args = make_args(&path.to_string_lossy(), content);
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "write should succeed: {msg}");
        assert!(msg.contains("Successfully wrote"), "message: {msg}");
        assert!(msg.contains("3 bytes"), "byte count: {msg}");
    }

    #[test]
    fn write_parent_already_exists_is_idempotent() {
        // Behavior 6 edge: create_dir_all is idempotent — no error when parent exists
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("file.txt");
        // Write once
        let args = make_args(&path.to_string_lossy(), "first");
        let (_, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "first write must succeed");
        // Write again (same parent, same path)
        let args2 = make_args(&path.to_string_lossy(), "second");
        let (msg2, is_err2) = super::execute_write_file(&args2);
        assert!(!is_err2, "second write must succeed: {msg2}");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(content, "second", "content updated to second write");
    }

    #[test]
    fn write_overwrites_existing_file() {
        // Behavior 6: write is not append — existing content is replaced
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "old content").expect("setup");
        let args = make_args(&path.to_string_lossy(), "new content");
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "overwrite must succeed: {msg}");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(content, "new content");
    }

    #[test]
    fn write_empty_content_succeeds() {
        // Behavior 6 edge: empty string is valid content
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("empty.txt");
        let args = make_args(&path.to_string_lossy(), "");
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "empty content write must succeed: {msg}");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(content, "");
    }

    #[test]
    fn write_missing_content_arg_returns_error() {
        // Behavior 6 error path: missing required 'content' argument
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("x.txt");
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            serde_json::json!(path.to_string_lossy().as_ref()),
        );
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(is_err, "missing content must error: {msg}");
        assert!(msg.contains("Missing 'content'"), "message: {msg}");
    }

    #[test]
    fn write_missing_path_arg_returns_error() {
        // Behavior 6 error path: missing required 'path' argument
        let mut args = HashMap::new();
        args.insert("content".to_string(), serde_json::json!("data"));
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(is_err, "missing path must error: {msg}");
        assert!(msg.contains("Missing 'path'"), "message: {msg}");
    }
}
