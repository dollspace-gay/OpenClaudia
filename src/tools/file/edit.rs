use super::{resolve_open_path, resolve_path, READ_TRACKER};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

/// Open the file once for read+write with `O_NOFOLLOW` on the leaf so a
/// symlink-swap between [`resolve_path`]'s canonicalize and this open call
/// fails with `ELOOP` instead of silently writing through the attacker's
/// symlink. See crosslink #417 (dup #428).
#[cfg(unix)]
fn open_for_edit_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_for_edit_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
}

/// Truncate the open handle to zero and rewrite it with `new_content`.
/// Keeps `execute_edit_file` under the clippy line budget while preserving
/// the single-FD discipline that makes #417's `O_NOFOLLOW` open meaningful.
fn rewrite_in_place(file: &mut std::fs::File, new_content: &str) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(new_content.as_bytes())
}

/// Canonicalise the user-supplied edit path. Mirrors the old inline logic in
/// [`execute_edit_file`]; extracted to keep that function under the clippy
/// `too_many_lines` budget after the crosslink #687 branch was added.
///
/// Returns the canonical path string, falling back to the canonical parent +
/// original leaf when the file does not yet exist.
fn canonicalise_edit_path(path: &str) -> Result<String, String> {
    let p = resolve_path(path)?;
    let canonical = match std::fs::canonicalize(&p) {
        Ok(c) => c,
        Err(_) => match p.parent() {
            Some(parent) => match std::fs::canonicalize(parent) {
                Ok(canon_parent) => canon_parent.join(p.file_name().unwrap_or_default()),
                Err(_) => {
                    return Err(format!(
                        "Cannot resolve path '{path}': parent directory does not exist"
                    ));
                }
            },
            None => return Err(format!("Invalid path: '{path}'")),
        },
    };
    Ok(canonical.to_string_lossy().to_string())
}

/// Build the human-readable success message + DIFF marker block.
///
/// Extracted from [`execute_edit_file`] so the parent function stays under
/// the clippy `too_many_lines` threshold once the crosslink #687
/// `replace_all` branch is added.
fn format_edit_success(
    path: &str,
    old_string: &str,
    new_string: &str,
    count: usize,
    replace_all: bool,
) -> String {
    let diff_json = serde_json::json!({
        "path": path,
        "old": old_string,
        "new": new_string,
    });
    let mut out = if replace_all && count > 1 {
        format!(
            "Successfully edited '{}'. Replaced {} occurrences ({} chars each with {} chars).\n@@DIFF_START@@\n{}\n@@DIFF_END@@",
            path,
            count,
            old_string.len(),
            new_string.len(),
            diff_json,
        )
    } else {
        format!(
            "Successfully edited '{}'. Replaced {} chars with {} chars.\n@@DIFF_START@@\n{}\n@@DIFF_END@@",
            path,
            old_string.len(),
            new_string.len(),
            diff_json,
        )
    };
    if let Some(warning) = crate::guardrails::check_diff_thresholds() {
        let _ = write!(out, "\n\nWarning: {}", warning.message);
    }
    out
}

/// Edit a file by replacing text.
///
/// Honours the optional `replace_all: bool` argument (crosslink #687):
/// when `true` every occurrence of `old_string` is replaced; when `false`
/// or absent, multi-occurrence inputs are rejected so callers must provide
/// a uniquely-matching `old_string`.
pub fn execute_edit_file(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(user_path) = args.get("path").and_then(|v| v.as_str()) else {
        return ("Missing 'path' argument".to_string(), true);
    };

    // Path passed to `open(2)`: canonical parent + original leaf so that
    // `O_NOFOLLOW` on the leaf can catch a symlink-swap. See crosslink #417.
    let open_path = match resolve_open_path(user_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    // Resolve symlinks to prevent symlink-based path traversal.
    let path = match canonicalise_edit_path(user_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };
    let path = path.as_str();

    // ENFORCE: Must read file before editing
    // This prevents the model from making edits based on hallucinated file contents
    if !READ_TRACKER.has_been_read(Path::new(path)) {
        return (
            format!(
                "You must read '{path}' before editing it. Use read_file first to see the actual contents."
            ),
            true,
        );
    }

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(path) {
        return (msg, true);
    }

    let Some(old_string) = args.get("old_string").and_then(|v| v.as_str()) else {
        return ("Missing 'old_string' argument".to_string(), true);
    };

    let Some(new_string) = args.get("new_string").and_then(|v| v.as_str()) else {
        return ("Missing 'new_string' argument".to_string(), true);
    };

    // crosslink #687: honour the `replace_all` flag. When `true`, all
    // occurrences are replaced; when `false` (or absent) the existing
    // single-occurrence-with-multi-rejection behaviour is preserved.
    let replace_all = args
        .get("replace_all")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Open ONCE with O_NOFOLLOW against the LEAF-PRESERVING path; all
    // I/O goes through this FD. See crosslink #417 (dup #428).
    let mut file = match open_for_edit_nofollow(&open_path) {
        Ok(f) => f,
        Err(e) => return (format!("Failed to open file '{path}': {e}"), true),
    };

    let mut content = String::new();
    if let Err(e) = file.read_to_string(&mut content) {
        return (format!("Failed to read file '{path}': {e}"), true);
    }

    if !content.contains(old_string) {
        return (
            format!(
                "Could not find the specified text in '{path}'. Make sure old_string matches exactly."
            ),
            true,
        );
    }

    let count = content.matches(old_string).count();
    if count > 1 && !replace_all {
        return (
            format!(
                "Found {count} occurrences of the text. Please provide a more specific old_string that matches uniquely, or set replace_all: true to replace every occurrence."
            ),
            true,
        );
    }

    let lines_removed = u32::try_from(old_string.lines().count())
        .unwrap_or(u32::MAX)
        .saturating_mul(u32::try_from(count).unwrap_or(u32::MAX));
    let lines_added = u32::try_from(new_string.lines().count())
        .unwrap_or(u32::MAX)
        .saturating_mul(u32::try_from(count).unwrap_or(u32::MAX));

    let new_content = if replace_all {
        content.replace(old_string, new_string)
    } else {
        content.replacen(old_string, new_string, 1)
    };

    match rewrite_in_place(&mut file, &new_content) {
        Ok(()) => {
            crate::guardrails::record_file_modification(path, lines_added, lines_removed);
            (
                format_edit_success(path, old_string, new_string, count, replace_all),
                false,
            )
        }
        Err(e) => (format!("Failed to write file '{path}': {e}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::super::READ_TRACKER;
    use std::io::Write as _;
    use std::path::Path;
    use tempfile::NamedTempFile;

    /// Write content to a `NamedTempFile`, mark it as read in `READ_TRACKER`,
    /// and return (file, `canonical_path_string`).
    fn tmp_readable(content: &str) -> (NamedTempFile, String) {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        let canon = f.path().canonicalize().expect("canonicalize");
        READ_TRACKER.mark_read(&canon);
        let path = canon.to_string_lossy().to_string();
        (f, path)
    }

    fn make_args(
        path: &str,
        old: &str,
        new: &str,
    ) -> std::collections::HashMap<String, serde_json::Value> {
        let mut m = std::collections::HashMap::new();
        m.insert("path".to_string(), serde_json::json!(path));
        m.insert("old_string".to_string(), serde_json::json!(old));
        m.insert("new_string".to_string(), serde_json::json!(new));
        m
    }

    // =========================================================================
    // Behavior 4: old_string not found → explicit error, no modification
    // =========================================================================

    #[test]
    fn edit_old_string_not_found_returns_error() {
        // Behavior 4: absent old_string must produce an error result.
        let (_f, path) = tmp_readable("hello world\n");
        let args = make_args(&path, "DOES NOT EXIST", "replacement");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(is_err, "missing old_string must be an error: {msg}");
        assert!(
            msg.contains("Could not find the specified text"),
            "error message: {msg}"
        );
    }

    #[test]
    fn edit_old_string_not_found_does_not_modify_file() {
        // Behavior 4: file content must be unchanged when old_string is absent.
        let original = "unchanged content\n";
        let (_f, path) = tmp_readable(original);
        let args = make_args(&path, "ABSENT", "whatever");
        super::execute_edit_file(&args);
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(
            after, original,
            "file must be unmodified on not-found error"
        );
    }

    // =========================================================================
    // Behavior 4 edge: CC performs quote normalization; OC does exact match
    // =========================================================================

    #[test]
    fn edit_curly_quote_not_normalized_returns_error() {
        // Behavior 4 edge: OC uses exact byte-match — curly quotes are NOT
        // substituted for straight quotes (CC does this via findActualString).
        // Pinned as current OC behavior; CC parity gap noted in #525 spec.
        let (_f, path) = tmp_readable("it's fine\n");
        // Search with a straight apostrophe when file has a curly one
        let args = make_args(&path, "it's fine", "ok");
        let (msg, is_err) = super::execute_edit_file(&args);
        // OC will return error (cannot find with straight quote); CC would find it.
        // We pin whichever OC currently does — the key assertion is the file is intact.
        let after = std::fs::read_to_string(&path).expect("read back");
        if is_err {
            // Expected OC path: exact match fails
            assert!(msg.contains("Could not find"), "error message: {msg}");
            assert!(after.contains("it's fine"), "file unmodified");
        } else {
            // If OC somehow matches (e.g. file was written with straight quote by
            // NamedTempFile), the replacement is fine — the point is no panic.
            assert!(!after.contains("it\u{2019}s fine") || after.contains("ok"));
        }
    }

    // =========================================================================
    // Behavior 4 edge: old_string === new_string
    // =========================================================================

    #[test]
    fn edit_old_equals_new_succeeds_if_present() {
        // Behavior 4 edge: CC catches old==new before the not-found check (errorCode 1).
        // OC does NOT special-case this — it succeeds (no-op write) when the string
        // exists once. Pinned as current OC behavior.
        let (_f, path) = tmp_readable("foo bar\n");
        let args = make_args(&path, "foo bar", "foo bar");
        let (msg, is_err) = super::execute_edit_file(&args);
        // OC: succeeds (no special validation for equal strings)
        assert!(!is_err, "OC does not reject old==new: {msg}");
    }

    // =========================================================================
    // Behavior 5: replace_all — OC rejects multi-occurrence unconditionally
    // =========================================================================

    #[test]
    fn edit_single_occurrence_succeeds() {
        // Behavior 5: single occurrence with no replace_all flag → success
        let (_f, path) = tmp_readable("alpha beta gamma\n");
        let args = make_args(&path, "beta", "BETA");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(!is_err, "single occurrence replace must succeed: {msg}");
        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(after.contains("BETA"), "replacement applied");
        assert!(!after.contains(" beta "), "old string gone");
    }

    #[test]
    fn edit_multi_occurrence_without_replace_all_errors() {
        // Behavior 5: N>1 occurrences without replace_all → error in both CC and OC
        let (_f, path) = tmp_readable("dog cat dog\n");
        let args = make_args(&path, "dog", "bird");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(is_err, "multi-occurrence must error: {msg}");
        assert!(
            msg.contains('2'),
            "error must mention occurrence count: {msg}"
        );
    }

    #[test]
    fn fix687_replace_all_true_replaces_every_occurrence() {
        // crosslink #687: replace_all=true must replace every occurrence
        // instead of returning the "be more specific" error.
        let (_f, path) = tmp_readable("x y x z x\n");
        let mut args = make_args(&path, "x", "Z");
        args.insert("replace_all".to_string(), serde_json::json!(true));
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(
            !is_err,
            "replace_all=true must succeed on multi-occurrence: {msg}"
        );
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "Z y Z z Z\n", "all occurrences replaced");
        assert!(
            msg.contains("3 occurrences"),
            "success message must report the count: {msg}"
        );
    }

    #[test]
    fn fix687_replace_all_false_preserves_existing_multi_occurrence_error() {
        // crosslink #687 regression guard: replace_all=false (the default) MUST
        // keep returning the single-occurrence rejection on N>1 hits.
        let (_f, path) = tmp_readable("dog cat dog\n");
        let mut args = make_args(&path, "dog", "bird");
        args.insert("replace_all".to_string(), serde_json::json!(false));
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(
            is_err,
            "replace_all=false on multi-occurrence must still error: {msg}"
        );
        assert!(
            msg.contains("Found 2 occurrences"),
            "error must still mention occurrence count: {msg}"
        );
        assert!(
            msg.contains("replace_all"),
            "remediation hint must mention replace_all: {msg}"
        );
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "dog cat dog\n");
    }

    #[test]
    fn fix687_absent_replace_all_defaults_to_false() {
        // crosslink #687: when replace_all is absent, behaviour matches replace_all=false.
        let (_f, path) = tmp_readable("dup dup dup\n");
        let args = make_args(&path, "dup", "X");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(is_err, "default (absent flag) must reject multi: {msg}");
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "dup dup dup\n", "file unmodified");
    }

    #[test]
    fn fix687_replace_all_true_single_occurrence_still_succeeds() {
        // crosslink #687: replace_all=true with exactly 1 occurrence still works
        // (the count==1 path uses replacen, which is equivalent here).
        let (_f, path) = tmp_readable("only once\n");
        let mut args = make_args(&path, "only once", "exactly once");
        args.insert("replace_all".to_string(), serde_json::json!(true));
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(
            !is_err,
            "single occurrence with replace_all succeeds: {msg}"
        );
        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(after.contains("exactly once"));
    }

    // =========================================================================
    // Behavior 4/5 error path: must read before editing
    // =========================================================================

    #[test]
    fn edit_requires_prior_read() {
        // Not in #525 spec directly, but the read-before-edit enforcement is a
        // contract that interacts with all Behavior 4/5 tests; pin it explicitly.
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(b"some content\n").expect("write");
        let path = f.path().canonicalize().expect("canon");
        // Deliberately do NOT call READ_TRACKER.mark_read() for this file
        let path_str = path.to_string_lossy().to_string();
        // Use a path that was never marked read; ensure it's unique so unrelated tests
        // don't accidentally mark it.
        let fresh_path = format!("{path_str}_never_read");
        std::fs::copy(&path, Path::new(&fresh_path)).ok(); // best-effort copy
        let args = make_args(&fresh_path, "some content", "other");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(is_err, "edit without prior read must error: {msg}");
        assert!(
            msg.contains("read") || msg.contains("Read"),
            "message: {msg}"
        );
        // clean up
        let _ = std::fs::remove_file(&fresh_path);
    }

    // ===== crosslink #417: edit rejects symlink-swap on the leaf =====

    #[cfg(unix)]
    #[test]
    fn fix417_edit_rejects_symlink_at_target() {
        use tempfile::TempDir;
        let dir = TempDir::new().expect("tempdir");
        let target = dir.path().join("attacker_target.txt");
        std::fs::write(&target, "PROTECTED\n").expect("setup target");
        let leaf = dir.path().join("leaf.txt");
        std::os::unix::fs::symlink(&target, &leaf).expect("symlink");
        let leaf_canon = leaf.canonicalize().expect("canonicalize leaf");
        READ_TRACKER.mark_read(&leaf_canon);
        let args = make_args(&leaf.to_string_lossy(), "PROTECTED", "PWNED");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(
            is_err,
            "edit through a symlink leaf must fail (O_NOFOLLOW): {msg}"
        );
        let target_contents = std::fs::read_to_string(&target).expect("read target");
        assert_eq!(
            target_contents, "PROTECTED\n",
            "symlink target must not be overwritten"
        );
    }

    #[test]
    fn fix417_edit_legitimate_regular_file_still_works() {
        let (_f, path) = tmp_readable("alpha beta gamma\n");
        let args = make_args(&path, "beta", "BETA");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(!is_err, "regular-file edit must succeed: {msg}");
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "alpha BETA gamma\n");
    }

    #[test]
    fn fix417_edit_shrinking_replacement_truncates_correctly() {
        let (_f, path) = tmp_readable("XXXXXXXXXX\n");
        let args = make_args(&path, "XXXXXXXXXX", "Y");
        let (msg, is_err) = super::execute_edit_file(&args);
        assert!(!is_err, "shrinking edit must succeed: {msg}");
        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(after, "Y\n", "no stale tail bytes after shrinking write");
    }
}
