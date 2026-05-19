use super::{resolve_open_path, resolve_path, READ_TRACKER};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

/// Open the notebook ONCE with `O_NOFOLLOW` on the leaf. All reads/writes go
/// through this single file handle — closing the TOCTOU window between
/// canonicalize and write described in crosslink #417 (dup #428).
#[cfg(unix)]
fn open_notebook_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_notebook_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
}

/// Split source text into a JSON array of line strings for notebook cell source format.
/// Each line except possibly the last ends with '\n'.
pub fn source_to_line_array(source: &str) -> Value {
    if source.is_empty() {
        return json!([]);
    }
    let lines: Vec<&str> = source.split('\n').collect();
    let mut result: Vec<Value> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if i < lines.len() - 1 {
            // Not the last line: append \n
            result.push(json!(format!("{}\n", line)));
        } else {
            // Last line: include as-is (no trailing \n unless empty)
            if !line.is_empty() {
                result.push(json!(*line));
            }
        }
    }
    result.into()
}

/// Look up a cell's position in the array by its stable `id` field
/// (set by modern Jupyter clients in each cell's top-level metadata).
/// Returns `None` when no cell matches.
fn find_cell_by_id(cells: &[Value], cell_id: &str) -> Option<usize> {
    cells.iter().position(|c| {
        c.get("id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| id == cell_id)
    })
}

/// Edit a Jupyter notebook cell.
///
/// Accepts either `cell_id` (Claude Code-compatible — matches the `id`
/// field Jupyter clients write into each cell's top-level metadata) or
/// `cell_number` (legacy 0-indexed position, kept for back-compat). At
/// least one of the two must be present for `replace` and `delete`.
/// For `insert`, `cell_id` means "insert AFTER the cell with this id";
/// omitting both inserts at position 0.
#[allow(clippy::too_many_lines)]
pub fn execute_notebook_edit(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(raw_path) = args.get("notebook_path").and_then(|v| v.as_str()) else {
        return ("Missing 'notebook_path' argument".to_string(), true);
    };

    let resolved = match resolve_path(raw_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    // Leaf-preserving path for the O_NOFOLLOW open. See crosslink #417.
    let open_path = match resolve_open_path(raw_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    let cell_id_arg = args
        .get("cell_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let cell_number_arg = args
        .get("cell_number")
        .and_then(serde_json::Value::as_u64)
        .map(|n| usize::try_from(n).unwrap_or(usize::MAX));

    let Some(new_source) = args.get("new_source").and_then(|v| v.as_str()) else {
        return ("Missing 'new_source' argument".to_string(), true);
    };

    let cell_type = args.get("cell_type").and_then(|v| v.as_str());
    let edit_mode = args
        .get("edit_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("replace");

    // Validate edit_mode
    if !["replace", "insert", "delete"].contains(&edit_mode) {
        return (
            format!("Invalid edit_mode '{edit_mode}'. Must be 'replace', 'insert', or 'delete'."),
            true,
        );
    }

    // Enforce read-before-edit
    if !READ_TRACKER.has_been_read(&resolved) {
        return (
            format!(
                "You must read '{}' before editing it. Use read_file first to see the actual contents.",
                resolved.display()
            ),
            true,
        );
    }

    // Blast radius check
    // Resolve symlinks to prevent path traversal
    let notebook_path = match std::fs::canonicalize(&resolved) {
        Ok(canon) => canon.to_string_lossy().to_string(),
        Err(_) => {
            return (
                format!("Cannot resolve notebook path '{}'", resolved.display()),
                true,
            );
        }
    };
    let notebook_path = notebook_path.as_str();

    if let Err(msg) = crate::guardrails::check_file_access(notebook_path) {
        return (msg, true);
    }

    // Open ONCE with O_NOFOLLOW against the LEAF-PRESERVING path. All
    // subsequent reads/writes use this FD — closing the TOCTOU window
    // described in crosslink #417 (dup #428).
    let mut file = match open_notebook_nofollow(&open_path) {
        Ok(f) => f,
        Err(e) => {
            return (
                format!("Failed to open notebook '{notebook_path}': {e}"),
                true,
            );
        }
    };

    // Read through the open handle.
    let mut content = String::new();
    if let Err(e) = file.read_to_string(&mut content) {
        return (
            format!("Failed to read notebook '{notebook_path}': {e}"),
            true,
        );
    }

    let mut notebook: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return (
                format!("Failed to parse notebook '{notebook_path}' as JSON: {e}"),
                true,
            )
        }
    };

    let Some(cells) = notebook.get_mut("cells").and_then(|c| c.as_array_mut()) else {
        return ("Notebook has no 'cells' array.".to_string(), true);
    };

    // Resolve the target index from whichever of cell_id / cell_number
    // the caller supplied. `cell_id` wins when both are present — it's
    // the stable identifier, `cell_number` shifts whenever a cell gets
    // inserted above it.
    let resolved_index: Option<usize> = if let Some(id) = cell_id_arg.as_deref() {
        match find_cell_by_id(cells, id) {
            Some(idx) => Some(idx),
            None => {
                return (format!("No cell with id '{id}' found in notebook."), true);
            }
        }
    } else {
        cell_number_arg
    };
    // Display text for error messages — "id 'abc'" when id was provided,
    // "number N" otherwise.
    let target_desc = cell_id_arg.as_deref().map_or_else(
        || cell_number_arg.map_or_else(|| "<unspecified>".to_string(), |n| format!("number {n}")),
        |id| format!("id '{id}'"),
    );

    // Index used when we print "Replaced cell ..." / "Deleted cell ..."
    // in the summary. Filled in per-branch; stays None for the "insert
    // at the beginning" case where no prior cell_id was passed.
    let mut summary_index: Option<usize> = resolved_index;

    match edit_mode {
        "replace" => {
            let Some(index) = resolved_index else {
                return (
                    "replace requires either 'cell_id' or 'cell_number'.".to_string(),
                    true,
                );
            };
            if index >= cells.len() {
                return (
                    format!(
                        "Cell {target_desc} is out of bounds. Notebook has {} cells (valid range: 0-{}).",
                        cells.len(),
                        cells.len().saturating_sub(1)
                    ),
                    true,
                );
            }
            cells[index]["source"] = source_to_line_array(new_source);
            if let Some(ct) = cell_type {
                cells[index]["cell_type"] = json!(ct);
            }
        }
        "insert" => {
            let Some(ct) = cell_type else {
                return (
                    "cell_type is required when inserting a new cell. Use 'code' or 'markdown'."
                        .to_string(),
                    true,
                );
            };

            // Semantics diverge from replace/delete here: Claude Code's
            // cell_id means "insert AFTER this cell", so the insertion
            // position is index+1. Legacy cell_number still means "at
            // this position". Omitting both inserts at the beginning.
            let insert_at = match (cell_id_arg.as_deref(), cell_number_arg) {
                (Some(_), _) => resolved_index.map_or(0, |i| i + 1),
                (None, Some(n)) => n,
                (None, None) => 0,
            };

            if insert_at > cells.len() {
                return (
                    format!(
                        "Cell {target_desc} is out of bounds for insertion. Notebook has {} cells (valid range: 0-{}).",
                        cells.len(),
                        cells.len()
                    ),
                    true,
                );
            }

            let mut new_cell = json!({
                "cell_type": ct,
                "metadata": {},
                "source": source_to_line_array(new_source)
            });
            if ct == "code" {
                new_cell["outputs"] = json!([]);
                new_cell["execution_count"] = Value::Null;
            }
            cells.insert(insert_at, new_cell);
            summary_index = Some(insert_at);
        }
        "delete" => {
            let Some(index) = resolved_index else {
                return (
                    "delete requires either 'cell_id' or 'cell_number'.".to_string(),
                    true,
                );
            };
            if index >= cells.len() {
                return (
                    format!(
                        "Cell {target_desc} is out of bounds. Notebook has {} cells (valid range: 0-{}).",
                        cells.len(),
                        cells.len().saturating_sub(1)
                    ),
                    true,
                );
            }
            cells.remove(index);
        }
        _ => unreachable!(),
    }

    // Write back THROUGH THE SAME FD: rewind, truncate, write. See #417.
    let old_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);
    match serde_json::to_string_pretty(&notebook) {
        Ok(pretty) => {
            let new_lines = u32::try_from(pretty.lines().count()).unwrap_or(u32::MAX);
            let write_result = file
                .seek(SeekFrom::Start(0))
                .and_then(|_| file.set_len(0).map(|()| 0u64))
                .and_then(|_| file.write_all(pretty.as_bytes()).map(|()| 0u64));
            match write_result {
                Ok(_) => {
                    crate::guardrails::record_file_modification(
                        notebook_path,
                        new_lines,
                        old_lines,
                    );
                    let where_str =
                        summary_index.map_or_else(|| target_desc.clone(), |idx| format!("{idx}"));
                    let action = match edit_mode {
                        "replace" => format!("Replaced cell {where_str} contents"),
                        "insert" => format!(
                            "Inserted new {} cell at position {}",
                            cell_type.unwrap_or("unknown"),
                            where_str
                        ),
                        "delete" => format!("Deleted cell {where_str}"),
                        _ => unreachable!(),
                    };
                    let mut result = format!(
                        "Successfully edited '{}'. {}. Notebook now has {} cells.",
                        notebook_path,
                        action,
                        notebook
                            .get("cells")
                            .and_then(|c| c.as_array())
                            .map_or(0, std::vec::Vec::len)
                    );
                    if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                        let _ = write!(result, "\n\nWarning: {}", warning.message);
                    }
                    (result, false)
                }
                Err(e) => (
                    format!("Failed to write notebook '{notebook_path}': {e}"),
                    true,
                ),
            }
        }
        Err(e) => (format!("Failed to serialize notebook: {e}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::super::READ_TRACKER;
    use super::{execute_notebook_edit, source_to_line_array};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // =========================================================================
    // source_to_line_array unit tests
    // =========================================================================

    #[test]
    fn source_to_line_array_empty_yields_empty_array() {
        let v = source_to_line_array("");
        assert_eq!(v, json!([]));
    }

    #[test]
    fn source_to_line_array_single_line_no_trailing_newline() {
        let v = source_to_line_array("hello");
        assert_eq!(v, json!(["hello"]));
    }

    #[test]
    fn source_to_line_array_multiline_adds_newlines_to_non_last() {
        let v = source_to_line_array("a\nb\nc");
        // Lines "a" and "b" get \n appended; last line "c" does not.
        assert_eq!(v, json!(["a\n", "b\n", "c"]));
    }

    // =========================================================================
    // Helpers for notebook edit tests
    // =========================================================================

    /// Build a minimal valid .ipynb JSON with the given cells.
    fn make_notebook(cells: &Value) -> Value {
        json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": cells
        })
    }

    /// Write a notebook JSON to a `NamedTempFile`, mark it read in `READ_TRACKER`,
    /// and return (file, `canonical_path_string`).
    fn tmp_notebook(nb: &Value) -> (NamedTempFile, String) {
        let mut f = NamedTempFile::new().expect("tempfile");
        let text = serde_json::to_string_pretty(nb).expect("serialize");
        f.write_all(text.as_bytes()).expect("write");
        let canon = f.path().canonicalize().expect("canonicalize");
        READ_TRACKER.mark_read(&canon);
        (f, canon.to_string_lossy().to_string())
    }

    fn args_replace_by_id(path: &str, cell_id: &str, new_source: &str) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("notebook_path".to_string(), json!(path));
        m.insert("cell_id".to_string(), json!(cell_id));
        m.insert("new_source".to_string(), json!(new_source));
        m.insert("edit_mode".to_string(), json!("replace"));
        m
    }

    fn args_replace_by_number(
        path: &str,
        cell_number: u64,
        new_source: &str,
    ) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("notebook_path".to_string(), json!(path));
        m.insert("cell_number".to_string(), json!(cell_number));
        m.insert("new_source".to_string(), json!(new_source));
        m.insert("edit_mode".to_string(), json!("replace"));
        m
    }

    fn args_insert(
        path: &str,
        cell_id: Option<&str>,
        cell_number: Option<u64>,
        cell_type: &str,
        new_source: &str,
    ) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("notebook_path".to_string(), json!(path));
        m.insert("cell_type".to_string(), json!(cell_type));
        m.insert("new_source".to_string(), json!(new_source));
        m.insert("edit_mode".to_string(), json!("insert"));
        if let Some(id) = cell_id {
            m.insert("cell_id".to_string(), json!(id));
        }
        if let Some(n) = cell_number {
            m.insert("cell_number".to_string(), json!(n));
        }
        m
    }

    fn args_delete_by_id(path: &str, cell_id: &str) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("notebook_path".to_string(), json!(path));
        m.insert("cell_id".to_string(), json!(cell_id));
        m.insert("new_source".to_string(), json!(""));
        m.insert("edit_mode".to_string(), json!("delete"));
        m
    }

    /// Read the cells array back from a written notebook file.
    fn read_cells(path: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(path).expect("read back");
        let nb: Value = serde_json::from_str(&text).expect("parse");
        nb["cells"].as_array().expect("cells array").clone()
    }

    // =========================================================================
    // Behavior 7: replace by cell_id — primary lookup
    // =========================================================================

    #[test]
    fn notebook_replace_by_cell_id_succeeds() {
        // Behavior 7: cell found by id field → source updated
        let nb = make_notebook(&json!([
            {"id": "cell-a", "cell_type": "code", "source": "old source", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_replace_by_id(&path, "cell-a", "new source");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "replace by id must succeed: {msg}");
        let cells = read_cells(&path);
        let src: String = match &cells[0]["source"] {
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            Value::String(s) => s.clone(),
            _ => panic!("unexpected source type"),
        };
        assert_eq!(src, "new source");
    }

    #[test]
    fn notebook_replace_by_cell_id_not_found_returns_error() {
        // Behavior 7 edge: cell_id not found and no cell_number fallback → error
        let nb = make_notebook(&json!([
            {"id": "cell-a", "cell_type": "code", "source": "x", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_replace_by_id(&path, "nonexistent-id", "y");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(is_err, "unknown cell_id must error: {msg}");
        assert!(msg.contains("No cell with id"), "message: {msg}");
    }

    // =========================================================================
    // Behavior 7: replace by cell_number — fallback when no cell_id given
    // =========================================================================

    #[test]
    fn notebook_replace_by_cell_number_succeeds() {
        // Behavior 7: OC exposes cell_number as a distinct parameter (not a
        // fallback parse of cell_id as CC does). When cell_id is absent,
        // cell_number is used directly as the 0-indexed position.
        let nb = make_notebook(&json!([
            {"cell_type": "code", "source": "first", "metadata": {}, "outputs": [], "execution_count": null},
            {"cell_type": "code", "source": "second", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_replace_by_number(&path, 1, "updated second");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "replace by cell_number must succeed: {msg}");
        let cells = read_cells(&path);
        let src: String = match &cells[1]["source"] {
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            Value::String(s) => s.clone(),
            _ => panic!("unexpected source type"),
        };
        assert!(src.contains("updated second"), "source updated: {src}");
    }

    #[test]
    fn notebook_replace_without_cell_id_or_number_errors() {
        // Behavior 7 edge: replace requires cell_id or cell_number
        let nb = make_notebook(&json!([
            {"cell_type": "code", "source": "x", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(&path));
        args.insert("new_source".to_string(), json!("y"));
        args.insert("edit_mode".to_string(), json!("replace"));
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(is_err, "replace without locator must error: {msg}");
        assert!(msg.contains("replace requires"), "message: {msg}");
    }

    // =========================================================================
    // Behavior 7: out-of-bounds replace → error (NOT silent promote to insert)
    // =========================================================================

    #[test]
    fn notebook_replace_out_of_bounds_returns_error_not_insert() {
        // Behavior 7 edge: index == cells.len() in OC returns out-of-bounds error.
        // CC silently promotes to insert (line 372-376 of CC source).
        // Pinned as current OC behavior.
        let nb = make_notebook(&json!([
            {"cell_type": "code", "source": "only", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        // cell_number = 1 but there is only 1 cell (index 0)
        let args = args_replace_by_number(&path, 1, "oob");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(
            is_err,
            "out-of-bounds replace must error in OC (CC parity gap — CC promotes to insert): {msg}"
        );
        assert!(msg.contains("out of bounds"), "message: {msg}");
        // File must be unchanged
        let cells = read_cells(&path);
        assert_eq!(cells.len(), 1, "cell count unchanged");
    }

    // =========================================================================
    // Behavior 7: code cell replace does NOT reset execution_count/outputs (OC gap)
    // =========================================================================

    #[test]
    fn notebook_replace_code_cell_does_not_reset_outputs() {
        // Behavior 7 edge (GAP): CC resets execution_count=null and outputs=[]
        // on code-cell replace (CC source line 420-423). OC does NOT reset them.
        // Pinned as current OC behavior; gap noted in #525 spec.
        let nb = make_notebook(&json!([
            {
                "id": "cell-x",
                "cell_type": "code",
                "source": "print('hello')",
                "metadata": {},
                "outputs": [{"output_type": "stream", "text": ["hello\n"]}],
                "execution_count": 3
            }
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_replace_by_id(&path, "cell-x", "print('world')");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "replace must succeed: {msg}");
        let cells = read_cells(&path);
        // OC: execution_count and outputs are preserved (not reset to null/[])
        // CC parity: CC would reset both. Pinned as current OC behavior.
        assert!(
            !cells[0]["outputs"]
                .as_array()
                .is_none_or(std::vec::Vec::is_empty),
            "OC does NOT clear outputs on replace (CC parity gap — CC clears them)"
        );
        assert!(
            cells[0]["execution_count"] != Value::Null,
            "OC does NOT reset execution_count on replace (CC parity gap)"
        );
    }

    // =========================================================================
    // Behavior 7: insert — no cell_id inserts at position 0
    // =========================================================================

    #[test]
    fn notebook_insert_without_cell_id_inserts_at_position_zero() {
        // Behavior 7 edge: omitting both cell_id and cell_number on insert → position 0
        let nb = make_notebook(&json!([
            {"cell_type": "code", "source": "existing", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_insert(&path, None, None, "markdown", "# new first");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "insert at 0 must succeed: {msg}");
        let cells = read_cells(&path);
        assert_eq!(cells.len(), 2, "cell count grew by 1");
        let first_src: String = match &cells[0]["source"] {
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            Value::String(s) => s.clone(),
            _ => panic!(),
        };
        assert!(first_src.contains("# new first"), "new cell at position 0");
    }

    #[test]
    fn notebook_insert_after_cell_id_inserts_at_next_position() {
        // Behavior 7: insert with cell_id means "insert AFTER" that cell
        let nb = make_notebook(&json!([
            {"id": "first", "cell_type": "code", "source": "a", "metadata": {}, "outputs": [], "execution_count": null},
            {"id": "second", "cell_type": "code", "source": "b", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_insert(&path, Some("first"), None, "markdown", "inserted");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "insert after cell must succeed: {msg}");
        let cells = read_cells(&path);
        assert_eq!(cells.len(), 3, "cell count");
        let mid_src: String = match &cells[1]["source"] {
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            Value::String(s) => s.clone(),
            _ => panic!(),
        };
        assert!(mid_src.contains("inserted"), "inserted cell at index 1");
    }

    // =========================================================================
    // Behavior 7: delete by cell_id
    // =========================================================================

    #[test]
    fn notebook_delete_by_cell_id_removes_correct_cell() {
        let nb = make_notebook(&json!([
            {"id": "keep", "cell_type": "code", "source": "keep me", "metadata": {}, "outputs": [], "execution_count": null},
            {"id": "remove", "cell_type": "code", "source": "remove me", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_delete_by_id(&path, "remove");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "delete must succeed: {msg}");
        let cells = read_cells(&path);
        assert_eq!(cells.len(), 1, "one cell remains");
        let src: String = match &cells[0]["source"] {
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            Value::String(s) => s.clone(),
            _ => panic!(),
        };
        assert!(src.contains("keep me"), "correct cell remains");
    }

    // =========================================================================
    // Behavior 7 / error path: invalid JSON notebook
    // =========================================================================

    #[test]
    fn notebook_invalid_json_returns_error() {
        // Behavior 7 error path: invalid JSON → error (both CC and OC agree)
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(b"not valid json {{{{").expect("write");
        let canon = f.path().canonicalize().expect("canon");
        READ_TRACKER.mark_read(&canon);
        let path = canon.to_string_lossy().to_string();
        let args = args_replace_by_number(&path, 0, "x");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(is_err, "invalid JSON must error: {msg}");
        assert!(msg.contains("Failed to parse notebook"), "message: {msg}");
    }

    // =========================================================================
    // Behavior 7 / error path: invalid edit_mode
    // =========================================================================

    #[test]
    fn notebook_invalid_edit_mode_returns_error() {
        let nb = make_notebook(&json!([]));
        let (_f, path) = tmp_notebook(&nb);
        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(&path));
        args.insert("new_source".to_string(), json!("x"));
        args.insert("edit_mode".to_string(), json!("upsert"));
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(is_err, "invalid edit_mode must error: {msg}");
        assert!(msg.contains("Invalid edit_mode"), "message: {msg}");
    }

    // ===== crosslink #417: notebook_edit rejects symlink-swap on the leaf =====

    #[cfg(unix)]
    #[test]
    fn fix417_notebook_rejects_symlink_at_target() {
        use tempfile::TempDir;
        let dir = TempDir::new().expect("tempdir");
        let target = dir.path().join("attacker_target.ipynb");
        let nb = make_notebook(&json!([
            {"id": "guarded", "cell_type": "code", "source": "SAFE", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        std::fs::write(
            &target,
            serde_json::to_string_pretty(&nb).expect("serialize"),
        )
        .expect("setup target");
        let leaf = dir.path().join("leaf.ipynb");
        std::os::unix::fs::symlink(&target, &leaf).expect("symlink");
        let leaf_canon = leaf.canonicalize().expect("canonicalize leaf");
        READ_TRACKER.mark_read(&leaf_canon);
        let args = args_replace_by_id(
            &leaf.to_string_lossy(),
            "guarded",
            "ATTACKER_INJECTED_SOURCE",
        );
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(
            is_err,
            "notebook_edit through a symlink leaf must fail (O_NOFOLLOW): {msg}"
        );
        let after = std::fs::read_to_string(&target).expect("read target");
        assert!(
            after.contains("SAFE"),
            "symlink target must not be overwritten; got: {after}"
        );
        assert!(
            !after.contains("ATTACKER_INJECTED_SOURCE"),
            "injected source must not appear in target"
        );
    }

    #[test]
    fn fix417_notebook_legitimate_edit_still_works() {
        let nb = make_notebook(&json!([
            {"id": "a", "cell_type": "code", "source": "old", "metadata": {}, "outputs": [], "execution_count": null}
        ]));
        let (_f, path) = tmp_notebook(&nb);
        let args = args_replace_by_id(&path, "a", "new");
        let (msg, is_err) = execute_notebook_edit(&args);
        assert!(!is_err, "regular notebook edit must succeed: {msg}");
        let cells = read_cells(&path);
        let src: String = match &cells[0]["source"] {
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            Value::String(s) => s.clone(),
            _ => panic!(),
        };
        assert_eq!(src, "new");
    }
}
