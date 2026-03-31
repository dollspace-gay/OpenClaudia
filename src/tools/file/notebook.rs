use super::READ_TRACKER;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Split source text into a JSON array of line strings for notebook cell source format.
/// Each line except possibly the last ends with '\n'.
pub(crate) fn source_to_line_array(source: &str) -> Value {
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
pub(crate) fn execute_notebook_edit(args: &HashMap<String, Value>) -> (String, bool) {
    let notebook_path = match args.get("notebook_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'notebook_path' argument".to_string(), true),
    };

    let cell_number = match args.get("cell_number").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => return ("Missing 'cell_number' argument".to_string(), true),
    };

    let new_source = match args.get("new_source").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'new_source' argument".to_string(), true),
    };

    let cell_type = args.get("cell_type").and_then(|v| v.as_str());
    let edit_mode = args
        .get("edit_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("replace");

    // Validate edit_mode
    if !["replace", "insert", "delete"].contains(&edit_mode) {
        return (
            format!(
                "Invalid edit_mode '{}'. Must be 'replace', 'insert', or 'delete'.",
                edit_mode
            ),
            true,
        );
    }

    // Enforce read-before-edit
    if !READ_TRACKER.has_been_read(Path::new(notebook_path)) {
        return (
            format!(
                "You must read '{}' before editing it. Use read_file first to see the actual contents.",
                notebook_path
            ),
            true,
        );
    }

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(notebook_path) {
        return (msg, true);
    }

    // Read and parse the notebook
    let content = match fs::read_to_string(notebook_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                format!("Failed to read notebook '{}': {}", notebook_path, e),
                true,
            )
        }
    };

    let mut notebook: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return (
                format!(
                    "Failed to parse notebook '{}' as JSON: {}",
                    notebook_path, e
                ),
                true,
            )
        }
    };

    let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
        Some(c) => c,
        None => return ("Notebook has no 'cells' array.".to_string(), true),
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
            let ct = match cell_type {
                Some(ct) => ct,
                None => return (
                    "cell_type is required when inserting a new cell. Use 'code' or 'markdown'."
                        .to_string(),
                    true,
                ),
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
    let old_lines = content.lines().count() as u32;
    match serde_json::to_string_pretty(&notebook) {
        Ok(pretty) => {
            let new_lines = pretty.lines().count() as u32;
            match fs::write(notebook_path, &pretty) {
                Ok(()) => {
                    crate::guardrails::record_file_modification(
                        notebook_path,
                        new_lines,
                        old_lines,
                    );
                    let action = match edit_mode {
                        "replace" => format!("Replaced cell {} contents", cell_number),
                        "insert" => format!(
                            "Inserted new {} cell at position {}",
                            cell_type.unwrap_or("unknown"),
                            cell_number
                        ),
                        "delete" => format!("Deleted cell {}", cell_number),
                        _ => unreachable!(),
                    };
                    let mut result = format!(
                        "Successfully edited '{}'. {}. Notebook now has {} cells.",
                        notebook_path,
                        action,
                        notebook
                            .get("cells")
                            .and_then(|c| c.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0)
                    );
                    if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                        result.push_str(&format!("\n\nWarning: {}", warning.message));
                    }
                    (result, false)
                }
                Err(e) => (
                    format!("Failed to write notebook '{}': {}", notebook_path, e),
                    true,
                ),
            }
        }
        Err(e) => (format!("Failed to serialize notebook: {}", e), true),
    }
}
