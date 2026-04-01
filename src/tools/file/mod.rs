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

use std::path::Path;
use std::sync::Mutex;

/// Maximum number of entries in the read tracker before eviction kicks in
const READ_TRACKER_MAX_ENTRIES: usize = 10_000;

/// Tracks which files have been read in the current session.
/// `edit_file` will fail if the file hasn't been read first.
pub static READ_TRACKER: std::sync::LazyLock<ReadFileTracker> =
    std::sync::LazyLock::new(ReadFileTracker::new);

pub struct ReadFileTracker {
    read_files: Mutex<std::collections::HashSet<std::path::PathBuf>>,
}

impl ReadFileTracker {
    fn new() -> Self {
        Self {
            read_files: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Mark a file as having been read
    pub(crate) fn mark_read(&self, path: &Path) {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            if let Ok(mut set) = self.read_files.lock() {
                set.insert(canonical);
            }
        } else {
            // If we can't canonicalize, use the path as-is
            if let Ok(mut set) = self.read_files.lock() {
                set.insert(path.to_path_buf());
            }
        }
        self.enforce_size_cap(READ_TRACKER_MAX_ENTRIES);
    }
    /// Check if a file has been read
    pub(crate) fn has_been_read(&self, path: &Path) -> bool {
        let check_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.read_files
            .lock()
            .ok()
            .is_some_and(|set| set.contains(&check_path))
    }

    /// Clear tracking (called on new session)
    pub(crate) fn clear(&self) {
        if let Ok(mut set) = self.read_files.lock() {
            set.clear();
        }
    }

    /// Enforce a size cap on tracked files to prevent unbounded memory growth.
    /// If the tracker exceeds `max_entries`, the oldest half of entries are removed.
    fn enforce_size_cap(&self, max_entries: usize) {
        if let Ok(mut set) = self.read_files.lock() {
            if set.len() > max_entries {
                // HashSet has no ordering, so we drain half arbitrarily.
                // This is acceptable because the tracker is advisory (for the
                // "you must read before editing" guard) and losing some entries
                // only means the user may be asked to re-read a file.
                let to_remove = set.len() / 2;
                let keys: Vec<_> = set.iter().take(to_remove).cloned().collect();
                for k in keys {
                    set.remove(&k);
                }
            }
        }
    }
}

/// Read a file's contents
pub fn execute_read_file(
    args: &std::collections::HashMap<String, serde_json::Value>,
) -> (String, bool) {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return ("Missing 'path' argument".to_string(), true);
    };

    // Reject path traversal attempts (relative paths with ..)
    let p = Path::new(path);
    if !p.is_absolute() {
        return (
            format!("Path must be absolute, got relative path: '{path}'"),
            true,
        );
    }

    // Track that this file has been read (for edit_file and notebook_edit enforcement)
    READ_TRACKER.mark_read(p);

    // Detect file type and dispatch accordingly
    match detect_file_type(path) {
        FileType::Image(mime_type) => read_image_file(path, mime_type),
        FileType::Pdf => {
            let pages = args.get("pages").and_then(|v| v.as_str());
            read::read_pdf_file(path, pages)
        }
        FileType::Notebook => read_notebook_file(path),
        FileType::Text => read_text_file(path, args),
    }
}
