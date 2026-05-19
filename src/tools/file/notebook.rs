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

/// Tool-result tuple used end-to-end: `(message, is_error)`. Helpers return
/// `Result<T, ToolFailure>` so the entry point can `?`-bubble errors and keep
/// its body linear (validate → resolve → dispatch → persist).
type ToolFailure = (String, bool);

/// Parsed-and-validated arguments. Owning `String`s avoids tying the lifetime
/// of the helper chain to the borrowed `HashMap` arg map.
struct ParsedArgs {
    raw_path: String,
    cell_id: Option<String>,
    cell_number: Option<usize>,
    new_source: String,
    cell_type: Option<String>,
    edit_mode: String,
}

/// Path/preflight context: paths and open handle shared across read+write.
struct NotebookHandle {
    /// Canonicalized path, used in user-facing error messages and guardrails.
    canonical_path: String,
    /// Single FD opened with `O_NOFOLLOW` against the leaf-preserving path
    /// — used for both the initial read and the truncating write back.
    /// Closes the TOCTOU window from crosslink #417.
    file: std::fs::File,
}

/// Result of resolving `cell_id` / `cell_number` against the parsed cells.
struct Locator {
    /// `Some(idx)` when a locator was supplied and (for `cell_id`) found.
    /// `None` only when neither locator was supplied — handled per-mode.
    index: Option<usize>,
    /// Human-readable description used in out-of-bounds error messages
    /// (`"id 'abc'"` vs `"number 3"` vs `"<unspecified>"`).
    target_desc: String,
}

/// What happened during the dispatch step, threaded into the summary line.
struct EditOutcome {
    /// Index that should appear in `Replaced/Inserted/Deleted cell <N>`.
    /// `None` only for "insert at the head with no locator" which falls
    /// back to `target_desc`.
    summary_index: Option<usize>,
}

/// Step 1 of the entry point: extract & validate every argument. No I/O,
/// no path resolution — just argument shape and the `edit_mode` enum check.
fn parse_args(args: &HashMap<String, Value>) -> Result<ParsedArgs, ToolFailure> {
    let raw_path = args
        .get("notebook_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ("Missing 'notebook_path' argument".to_string(), true))?
        .to_string();

    let new_source = args
        .get("new_source")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ("Missing 'new_source' argument".to_string(), true))?
        .to_string();

    let edit_mode = args
        .get("edit_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("replace")
        .to_string();

    if !["replace", "insert", "delete"].contains(&edit_mode.as_str()) {
        return Err((
            format!("Invalid edit_mode '{edit_mode}'. Must be 'replace', 'insert', or 'delete'."),
            true,
        ));
    }

    let cell_id = args
        .get("cell_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let cell_number = args
        .get("cell_number")
        .and_then(serde_json::Value::as_u64)
        .map(|n| usize::try_from(n).unwrap_or(usize::MAX));
    let cell_type = args
        .get("cell_type")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(ParsedArgs {
        raw_path,
        cell_id,
        cell_number,
        new_source,
        cell_type,
        edit_mode,
    })
}

/// Step 2: resolve the path, enforce read-before-edit, canonicalize for the
/// blast-radius check, then open ONCE with `O_NOFOLLOW`. Returns the open
/// handle plus the canonicalized path for downstream messages.
fn preflight_and_open(raw_path: &str) -> Result<NotebookHandle, ToolFailure> {
    let resolved = resolve_path(raw_path).map_err(|e| (e, true))?;
    // Leaf-preserving path for the O_NOFOLLOW open. See crosslink #417.
    let open_path = resolve_open_path(raw_path).map_err(|e| (e, true))?;

    if !READ_TRACKER.has_been_read(&resolved) {
        return Err((
            format!(
                "You must read '{}' before editing it. Use read_file first to see the actual contents.",
                resolved.display()
            ),
            true,
        ));
    }

    let canonical_path = std::fs::canonicalize(&resolved)
        .map(|c| c.to_string_lossy().to_string())
        .map_err(|_| {
            (
                format!("Cannot resolve notebook path '{}'", resolved.display()),
                true,
            )
        })?;

    crate::guardrails::check_file_access(&canonical_path).map_err(|msg| (msg, true))?;

    // Open ONCE with O_NOFOLLOW against the LEAF-PRESERVING path. All
    // subsequent reads/writes use this FD — closing the TOCTOU window
    // described in crosslink #417 (dup #428).
    let file = open_notebook_nofollow(&open_path).map_err(|e| {
        (
            format!("Failed to open notebook '{canonical_path}': {e}"),
            true,
        )
    })?;

    Ok(NotebookHandle {
        canonical_path,
        file,
    })
}

/// Step 3: read the open handle and parse JSON. Returns the parsed notebook
/// plus the raw text so `record_file_modification` can count old lines.
fn read_and_parse(handle: &mut NotebookHandle) -> Result<(Value, String), ToolFailure> {
    let mut content = String::new();
    handle.file.read_to_string(&mut content).map_err(|e| {
        (
            format!("Failed to read notebook '{}': {e}", handle.canonical_path),
            true,
        )
    })?;

    let notebook: Value = serde_json::from_str(&content).map_err(|e| {
        (
            format!(
                "Failed to parse notebook '{}' as JSON: {e}",
                handle.canonical_path
            ),
            true,
        )
    })?;

    Ok((notebook, content))
}

/// Step 4: resolve `cell_id` / `cell_number` against the cells array. When
/// `cell_id` is present it wins (stable id beats positional). Unknown ids
/// are a hard error; absent locators yield `index = None` for the modes
/// that allow it (`insert`).
fn resolve_locator(parsed: &ParsedArgs, cells: &[Value]) -> Result<Locator, ToolFailure> {
    let index = if let Some(id) = parsed.cell_id.as_deref() {
        Some(
            find_cell_by_id(cells, id)
                .ok_or_else(|| (format!("No cell with id '{id}' found in notebook."), true))?,
        )
    } else {
        parsed.cell_number
    };

    let target_desc = parsed.cell_id.as_deref().map_or_else(
        || {
            parsed
                .cell_number
                .map_or_else(|| "<unspecified>".to_string(), |n| format!("number {n}"))
        },
        |id| format!("id '{id}'"),
    );

    Ok(Locator { index, target_desc })
}

/// Replace-mode dispatch. `cell_id` or `cell_number` is required; index
/// must be in range (`index == len` is rejected, NOT silently promoted to
/// insert — see test `notebook_replace_out_of_bounds_returns_error_not_insert`).
fn apply_replace(
    cells: &mut [Value],
    locator: &Locator,
    parsed: &ParsedArgs,
) -> Result<EditOutcome, ToolFailure> {
    let index = locator.index.ok_or_else(|| {
        (
            "replace requires either 'cell_id' or 'cell_number'.".to_string(),
            true,
        )
    })?;
    if index >= cells.len() {
        return Err((
            format!(
                "Cell {} is out of bounds. Notebook has {} cells (valid range: 0-{}).",
                locator.target_desc,
                cells.len(),
                cells.len().saturating_sub(1)
            ),
            true,
        ));
    }
    cells[index]["source"] = source_to_line_array(&parsed.new_source);
    if let Some(ct) = parsed.cell_type.as_deref() {
        cells[index]["cell_type"] = json!(ct);
    }
    Ok(EditOutcome {
        summary_index: Some(index),
    })
}

/// Insert-mode dispatch. `cell_type` is required. `cell_id` semantics
/// diverge from replace/delete: "insert AFTER the cell with this id".
/// Legacy `cell_number` still means "at this exact position". Omitting
/// both inserts at the head.
fn apply_insert(
    cells: &mut Vec<Value>,
    locator: &Locator,
    parsed: &ParsedArgs,
) -> Result<EditOutcome, ToolFailure> {
    let ct = parsed.cell_type.as_deref().ok_or_else(|| {
        (
            "cell_type is required when inserting a new cell. Use 'code' or 'markdown'."
                .to_string(),
            true,
        )
    })?;

    let insert_at = match (parsed.cell_id.as_deref(), parsed.cell_number) {
        (Some(_), _) => locator.index.map_or(0, |i| i + 1),
        (None, Some(n)) => n,
        (None, None) => 0,
    };

    if insert_at > cells.len() {
        return Err((
            format!(
                "Cell {} is out of bounds for insertion. Notebook has {} cells (valid range: 0-{}).",
                locator.target_desc,
                cells.len(),
                cells.len()
            ),
            true,
        ));
    }

    let mut new_cell = json!({
        "cell_type": ct,
        "metadata": {},
        "source": source_to_line_array(&parsed.new_source)
    });
    if ct == "code" {
        new_cell["outputs"] = json!([]);
        new_cell["execution_count"] = Value::Null;
    }
    cells.insert(insert_at, new_cell);
    Ok(EditOutcome {
        summary_index: Some(insert_at),
    })
}

/// Delete-mode dispatch. Same locator + bounds rules as replace.
fn apply_delete(cells: &mut Vec<Value>, locator: &Locator) -> Result<EditOutcome, ToolFailure> {
    let index = locator.index.ok_or_else(|| {
        (
            "delete requires either 'cell_id' or 'cell_number'.".to_string(),
            true,
        )
    })?;
    if index >= cells.len() {
        return Err((
            format!(
                "Cell {} is out of bounds. Notebook has {} cells (valid range: 0-{}).",
                locator.target_desc,
                cells.len(),
                cells.len().saturating_sub(1)
            ),
            true,
        ));
    }
    cells.remove(index);
    Ok(EditOutcome {
        summary_index: Some(index),
    })
}

/// Step 5: dispatch on `edit_mode`. `parse_args` already validated the
/// string is one of the three known modes, so the wildcard is unreachable.
fn dispatch_edit(
    cells: &mut Vec<Value>,
    locator: &Locator,
    parsed: &ParsedArgs,
) -> Result<EditOutcome, ToolFailure> {
    match parsed.edit_mode.as_str() {
        "replace" => apply_replace(cells, locator, parsed),
        "insert" => apply_insert(cells, locator, parsed),
        "delete" => apply_delete(cells, locator),
        _ => unreachable!("edit_mode validated in parse_args"),
    }
}

/// Step 6: write the mutated notebook back through the SAME FD (rewind +
/// truncate + write) and update guardrail diff counters. See #417 for
/// why we don't reopen by path here.
fn write_notebook(
    handle: &mut NotebookHandle,
    notebook: &Value,
    original_content: &str,
) -> Result<(), ToolFailure> {
    let pretty = serde_json::to_string_pretty(notebook)
        .map_err(|e| (format!("Failed to serialize notebook: {e}"), true))?;

    let old_lines = u32::try_from(original_content.lines().count()).unwrap_or(u32::MAX);
    let new_lines = u32::try_from(pretty.lines().count()).unwrap_or(u32::MAX);

    handle
        .file
        .seek(SeekFrom::Start(0))
        .and_then(|_| handle.file.set_len(0))
        .and_then(|()| handle.file.write_all(pretty.as_bytes()))
        .map_err(|e| {
            (
                format!("Failed to write notebook '{}': {e}", handle.canonical_path),
                true,
            )
        })?;

    crate::guardrails::record_file_modification(&handle.canonical_path, new_lines, old_lines);
    Ok(())
}

/// Step 7: format the success summary. The summary index falls back to
/// the locator's target description for the (rare) head-insert case where
/// the caller supplied no locator.
fn format_success(
    handle: &NotebookHandle,
    notebook: &Value,
    locator: &Locator,
    outcome: &EditOutcome,
    parsed: &ParsedArgs,
) -> String {
    let where_str = outcome
        .summary_index
        .map_or_else(|| locator.target_desc.clone(), |idx| format!("{idx}"));
    let action = match parsed.edit_mode.as_str() {
        "replace" => format!("Replaced cell {where_str} contents"),
        "insert" => format!(
            "Inserted new {} cell at position {}",
            parsed.cell_type.as_deref().unwrap_or("unknown"),
            where_str
        ),
        "delete" => format!("Deleted cell {where_str}"),
        _ => unreachable!("edit_mode validated in parse_args"),
    };
    let mut result = format!(
        "Successfully edited '{}'. {}. Notebook now has {} cells.",
        handle.canonical_path,
        action,
        notebook
            .get("cells")
            .and_then(|c| c.as_array())
            .map_or(0, std::vec::Vec::len)
    );
    if let Some(warning) = crate::guardrails::check_diff_thresholds() {
        let _ = write!(result, "\n\nWarning: {}", warning.message);
    }
    result
}

/// Edit a Jupyter notebook cell.
///
/// Accepts either `cell_id` (Claude Code-compatible — matches the `id`
/// field Jupyter clients write into each cell's top-level metadata) or
/// `cell_number` (legacy 0-indexed position, kept for back-compat). At
/// least one of the two must be present for `replace` and `delete`.
/// For `insert`, `cell_id` means "insert AFTER the cell with this id";
/// omitting both inserts at position 0.
///
/// Body is the linear pipeline: validate → preflight → read → resolve
/// → dispatch → persist → summarize. Each step is a private helper above.
/// Refactored from a 200+-line god function per crosslink #681.
pub fn execute_notebook_edit(args: &HashMap<String, Value>) -> (String, bool) {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut handle = match preflight_and_open(&parsed.raw_path) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let (mut notebook, original_content) = match read_and_parse(&mut handle) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let Some(cells) = notebook.get_mut("cells").and_then(|c| c.as_array_mut()) else {
        return ("Notebook has no 'cells' array.".to_string(), true);
    };
    let locator = match resolve_locator(&parsed, cells) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let outcome = match dispatch_edit(cells, &locator, &parsed) {
        Ok(o) => o,
        Err(e) => return e,
    };
    if let Err(e) = write_notebook(&mut handle, &notebook, &original_content) {
        return e;
    }
    (
        format_success(&handle, &notebook, &locator, &outcome, &parsed),
        false,
    )
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
