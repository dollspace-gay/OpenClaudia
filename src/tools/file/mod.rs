mod edit;
mod list;
mod notebook;
mod read;
mod write;

pub use edit::execute_edit_file;
pub use list::execute_list_files;
#[allow(unused_imports)] // used by tests in tools::mod
pub use notebook::{execute_notebook_edit, source_to_line_array};
#[allow(unused_imports)] // used by tests in tools::mod
pub use read::{
    detect_file_type, parse_page_range, read_image_file, read_notebook_file, read_text_file,
    FileType,
};
pub use write::execute_write_file;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Maximum number of entries in the read tracker before eviction kicks in
const READ_TRACKER_MAX_ENTRIES: usize = 10_000;

/// Tracks which files have been read in the current session.
/// `edit_file` will fail if the file hasn't been read first.
pub static READ_TRACKER: std::sync::LazyLock<ReadFileTracker> =
    std::sync::LazyLock::new(ReadFileTracker::new);

pub struct ReadFileTracker {
    /// LRU-ordered list: most recently read files at the end.
    /// When capacity is exceeded, oldest entries (front) are evicted.
    read_files: Mutex<Vec<PathBuf>>,
}

impl ReadFileTracker {
    fn new() -> Self {
        Self {
            read_files: Mutex::new(Vec::new()),
        }
    }

    /// Mark a file as having been read. Moves to end (most recent) if already tracked.
    pub(crate) fn mark_read(&self, path: &Path) {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Ok(mut files) = self.read_files.lock() {
            // Remove existing entry (if any) so we can re-add at the end
            files.retain(|p| p != &resolved);
            files.push(resolved);
            // Evict oldest entries if over capacity
            if files.len() > READ_TRACKER_MAX_ENTRIES {
                let excess = files.len() - READ_TRACKER_MAX_ENTRIES;
                files.drain(..excess);
            }
        }
    }

    /// Check if a file has been read
    pub(crate) fn has_been_read(&self, path: &Path) -> bool {
        let check_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.read_files
            .lock()
            .ok()
            .is_some_and(|files| files.contains(&check_path))
    }

    /// Clear tracking (called on new session)
    pub(crate) fn clear(&self) {
        if let Ok(mut files) = self.read_files.lock() {
            files.clear();
        }
    }
}

/// Resolve a path argument to an absolute path.
///
/// If the path is already absolute, return it as-is.
/// If relative, resolve it against the current working directory.
/// Rejects paths containing `..` components to prevent traversal.
fn resolve_path(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Cannot resolve relative path (no working directory): {e}"))?
            .join(p)
    };

    // Reject path traversal attempts (../ in path)
    if resolved
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(format!("Path traversal not allowed: '{path}'"));
    }

    Ok(resolved)
}

/// Read a file's contents
pub fn execute_read_file(
    args: &std::collections::HashMap<String, serde_json::Value>,
) -> (String, bool) {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return ("Missing 'path' argument".to_string(), true);
    };

    let resolved = match resolve_path(path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };
    let resolved_str = resolved.to_string_lossy();

    // Track that this file has been read (for edit_file and notebook_edit enforcement)
    READ_TRACKER.mark_read(&resolved);

    // Detect file type and dispatch accordingly
    match detect_file_type(&resolved_str) {
        FileType::Image(mime_type) => read_image_file(&resolved_str, mime_type),
        FileType::Pdf => {
            let pages = args.get("pages").and_then(|v| v.as_str());
            read::read_pdf_file(&resolved_str, pages)
        }
        FileType::Notebook => read_notebook_file(&resolved_str),
        FileType::Text => read_text_file(&resolved_str, args),
    }
}
