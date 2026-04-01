use super::READ_TRACKER;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

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

/// Edit a Jupyter notebook cell
#[allow(clippy::too_many_lines)]
pub fn execute_notebook_edit(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(notebook_path) = args.get("notebook_path").and_then(|v| v.as_str()) else {
        return ("Missing 'notebook_path' argument".to_string(), true);
    };

    let Some(cell_number) = args.get("cell_number").and_then(serde_json::Value::as_u64) else {
        return ("Missing 'cell_number' argument".to_string(), true);
    };
    let cell_number = usize::try_from(cell_number).unwrap_or(usize::MAX);

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
    if !READ_TRACKER.has_been_read(Path::new(notebook_path)) {
        return (
            format!(
                "You must read '{notebook_path}' before editing it. Use read_file first to see the actual contents."
            ),
            true,
        );
    }

    // Blast radius check
    // Resolve symlinks to prevent path traversal
    let notebook_path = match std::fs::canonicalize(notebook_path) {
        Ok(canon) => canon.to_string_lossy().to_string(),
        Err(_) => {
            return (
                format!("Cannot resolve notebook path '{notebook_path}'"),
                true,
            );
        }
    };
    let notebook_path = notebook_path.as_str();

    if let Err(msg) = crate::guardrails::check_file_access(notebook_path) {
        return (msg, true);
    }

    // Read and parse the notebook
    let content = match fs::read_to_string(notebook_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                format!("Failed to read notebook '{notebook_path}': {e}"),
                true,
            )
        }
    };

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

    match edit_mode {
        "replace" => {
            if cell_number >= cells.len() {
                return (
                    format!(
                        "Cell number {} is out of bounds. Notebook has {} cells (0-indexed).",
                        cell_number,
                        cells.len()
                    ),
                    true,
                );
            }

            // Update the cell's source
            cells[cell_number]["source"] = source_to_line_array(new_source);

            // Optionally update cell_type if provided
            if let Some(ct) = cell_type {
                cells[cell_number]["cell_type"] = json!(ct);
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

            if cell_number > cells.len() {
                return (
                    format!(
                        "Cell number {} is out of bounds for insertion. Notebook has {} cells (valid range: 0-{}).",
                        cell_number,
                        cells.len(),
                        cells.len()
                    ),
                    true,
                );
            }

            // Create a new cell
            let mut new_cell = json!({
                "cell_type": ct,
                "metadata": {},
                "source": source_to_line_array(new_source)
            });

            // Code cells have an outputs array and execution_count
            if ct == "code" {
                new_cell["outputs"] = json!([]);
                new_cell["execution_count"] = Value::Null;
            }

            cells.insert(cell_number, new_cell);
        }
        "delete" => {
            if cell_number >= cells.len() {
                return (
                    format!(
                        "Cell number {} is out of bounds. Notebook has {} cells (0-indexed).",
                        cell_number,
                        cells.len()
                    ),
                    true,
                );
            }

            cells.remove(cell_number);
        }
        _ => unreachable!(),
    }

    // Write back with pretty formatting
    let old_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);
    match serde_json::to_string_pretty(&notebook) {
        Ok(pretty) => {
            let new_lines = u32::try_from(pretty.lines().count()).unwrap_or(u32::MAX);
            match fs::write(notebook_path, &pretty) {
                Ok(()) => {
                    crate::guardrails::record_file_modification(
                        notebook_path,
                        new_lines,
                        old_lines,
                    );
                    let action = match edit_mode {
                        "replace" => format!("Replaced cell {cell_number} contents"),
                        "insert" => format!(
                            "Inserted new {} cell at position {}",
                            cell_type.unwrap_or("unknown"),
                            cell_number
                        ),
                        "delete" => format!("Deleted cell {cell_number}"),
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
