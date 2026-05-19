use super::{resolve_open_path, resolve_path};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::Path;

/// Open a file for writing in a way that refuses to follow a symlink at the
/// leaf. This closes the TOCTOU window in which an attacker swaps the leaf
/// for a symlink between [`resolve_path`]'s `canonicalize` and the final
/// `fs::write` (crosslink #417 / dup #428).
///
/// `O_NOFOLLOW` applies to the **last** component of the path only — intermediate
/// path elements still resolve through symlinks. That is exactly what we want:
/// the jail check has already vetted the *resolved* path, and `O_NOFOLLOW`
/// ensures the kernel's `open(2)` call fails with `ELOOP` if the leaf became
/// a symlink in the meantime.
#[cfg(unix)]
fn open_for_write_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_for_write_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    // On non-Unix targets we fall back to the standard open. Windows
    // hardening (FILE_FLAG_OPEN_REPARSE_POINT) is tracked separately.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Write content to a file
pub fn execute_write_file(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(user_path) = args.get("path").and_then(|v| v.as_str()) else {
        return ("Missing 'path' argument".to_string(), true);
    };

    let p = match resolve_path(user_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    // Path passed to `open(2)`: canonical parent + original leaf name. A
    // fully canonicalized path has already resolved the leaf symlink, so
    // `O_NOFOLLOW` against it is useless. This leaf-preserving variant
    // makes `O_NOFOLLOW` reject a swapped leaf with `ELOOP`. See #417.
    let open_path = match resolve_open_path(user_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    let canonical = match std::fs::canonicalize(&p) {
        Ok(canon) => canon,
        Err(_) => {
            if let Some(parent) = p.parent() {
                std::fs::canonicalize(parent).map_or_else(
                    |_| p.clone(),
                    |canon_parent| canon_parent.join(p.file_name().unwrap_or_default()),
                )
            } else {
                return (format!("Invalid path: '{user_path}'"), true);
            }
        }
    };
    let path = canonical.to_string_lossy().to_string();
    let path = path.as_str();

    let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
        return ("Missing 'content' argument".to_string(), true);
    };

    if let Err(msg) = crate::guardrails::check_file_access(path) {
        return (msg, true);
    }

    let old_lines = fs::read_to_string(path)
        .map_or(0, |c| u32::try_from(c.lines().count()).unwrap_or(u32::MAX));
    let new_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);

    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                return (format!("Failed to create directories: {e}"), true);
            }
        }
    }

    // Open with O_NOFOLLOW against the LEAF-PRESERVING path. See #417.
    let mut file = match open_for_write_nofollow(&open_path) {
        Ok(f) => f,
        Err(e) => {
            return (
                format!("Failed to open file '{path}' for writing: {e}"),
                true,
            );
        }
    };

    match file.write_all(content.as_bytes()) {
        Ok(()) => {
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

    #[test]
    fn write_creates_parent_directories_recursively() {
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
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("file.txt");
        let args = make_args(&path.to_string_lossy(), "first");
        let (_, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "first write must succeed");
        let args2 = make_args(&path.to_string_lossy(), "second");
        let (msg2, is_err2) = super::execute_write_file(&args2);
        assert!(!is_err2, "second write must succeed: {msg2}");
        let content = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(content, "second");
    }

    #[test]
    fn write_overwrites_existing_file() {
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
        let mut args = HashMap::new();
        args.insert("content".to_string(), serde_json::json!("data"));
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(is_err, "missing path must error: {msg}");
        assert!(msg.contains("Missing 'path'"), "message: {msg}");
    }

    // ===== crosslink #417: TOCTOU symlink-swap rejected by O_NOFOLLOW =====

    #[cfg(unix)]
    #[test]
    fn fix417_write_rejects_symlink_at_target() {
        let dir = TempDir::new().expect("tempdir");
        let target = dir.path().join("attacker_secrets.txt");
        std::fs::write(&target, "DO NOT OVERWRITE").expect("setup target");
        let leaf = dir.path().join("leaf.txt");
        std::os::unix::fs::symlink(&target, &leaf).expect("create symlink");
        let args = make_args(&leaf.to_string_lossy(), "attacker would inject this");
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(
            is_err,
            "write through a symlink leaf must fail (O_NOFOLLOW): {msg}"
        );
        let target_contents = std::fs::read_to_string(&target).expect("read target");
        assert_eq!(
            target_contents, "DO NOT OVERWRITE",
            "symlink target must not be overwritten"
        );
    }

    #[test]
    fn fix417_write_legitimate_regular_file_still_works() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("real.txt");
        std::fs::write(&path, "old").expect("setup");
        let args = make_args(&path.to_string_lossy(), "new");
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "regular-file overwrite must succeed: {msg}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
    }

    #[test]
    fn fix417_write_create_new_file_works() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("brand_new.txt");
        assert!(!path.exists(), "precondition: file must not exist");
        let args = make_args(&path.to_string_lossy(), "fresh");
        let (msg, is_err) = super::execute_write_file(&args);
        assert!(!is_err, "create-new must succeed: {msg}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "fresh");
    }
}
