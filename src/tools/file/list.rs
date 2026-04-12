use super::resolve_path;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;

/// List files in a directory
pub fn execute_list_files(args: &HashMap<String, Value>) -> (String, bool) {
    let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let path = match resolve_path(raw_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    match fs::read_dir(&path) {
        Ok(entries) => {
            let mut items: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let file_type = entry
                    .file_type()
                    .map(|ft| if ft.is_dir() { "/" } else { "" })
                    .unwrap_or("");
                items.push(format!("{name}{file_type}"));
            }
            items.sort();
            (items.join("\n"), false)
        }
        Err(e) => (
            format!("Failed to list directory '{}': {e}", path.display()),
            true,
        ),
    }
}
