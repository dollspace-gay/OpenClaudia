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
use std::sync::{LazyLock, Mutex};

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

/// Snapshot of the project root, captured the first time [`resolve_path`] runs.
///
/// Pinned at startup so that later `cd`s (via the worktree tool, shell
/// commands, etc.) cannot move the jail underneath us.
static PROJECT_ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::current_dir()
        .and_then(|cwd| cwd.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."))
});

/// Process temp directory, canonicalized. Used as a second allowed jail for
/// tests (tempfile creates paths under `/tmp/...`) and for legitimate
/// intermediate-file workflows.
static TEMP_ROOT: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::temp_dir().canonicalize().ok());

/// Returns `true` when the strict jail is active (the default).
///
/// Disabled only when the operator explicitly sets
/// `OPENCLAUDIA_ALLOW_OUT_OF_ROOT=1`. Any other value keeps strict mode on,
/// matching the secure-by-default posture.
fn strict_mode() -> bool {
    !matches!(std::env::var("OPENCLAUDIA_ALLOW_OUT_OF_ROOT"), Ok(ref v) if v == "1")
}

/// Returns `true` if `canonical` is inside `root` (including `root` itself).
fn path_is_within(canonical: &Path, root: &Path) -> bool {
    canonical == root || canonical.starts_with(root)
}

/// Resolve a path argument to a canonical absolute path inside the project-root jail.
///
/// Defenses applied, in order:
///  1. Early rejection of `..` components (clearer error than letting `canonicalize` eat them).
///  2. `canonicalize()` follows symlinks; if the target does not yet exist,
///     the first existing ancestor is canonicalized and the non-existent
///     suffix rejoined — this closes the symlink-escape-on-read hole while
///     still supporting write/edit of new files.
///  3. Containment check against `PROJECT_ROOT` OR the process temp dir.
///     Anything outside both is rejected unless
///     `OPENCLAUDIA_ALLOW_OUT_OF_ROOT=1` is set.
///
/// The previous implementation returned absolute paths as-is, which allowed
/// a prompt-injected model to read `/etc/passwd`, `/root/.ssh/id_rsa`, etc.
/// See crosslink issue #269.
fn resolve_path(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);

    // Step 1: absolutize (pure string join — does NOT follow symlinks yet).
    let absolute = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Cannot resolve relative path (no working directory): {e}"))?
            .join(p)
    };

    // Step 2: reject `..` components upfront with a clearer error.
    if absolute
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(format!("Path traversal not allowed: '{path}'"));
    }

    // Step 3: canonicalize (resolves symlinks). For paths to files that do not
    // yet exist (write_file's target, deeply nested new directories), walk up
    // the chain to the first existing ancestor, canonicalize THAT, then
    // rejoin the virtual suffix — this closes the symlink-escape-on-read hole
    // while still supporting `write_file path/to/newly/created/file.txt`.
    let canonical = match absolute.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            let mut ancestor = absolute.as_path();
            let mut suffix_components: Vec<&std::ffi::OsStr> = Vec::new();
            let canonical_ancestor = loop {
                match ancestor.canonicalize() {
                    Ok(c) => break c,
                    Err(_) => {
                        let file_name = ancestor.file_name().ok_or_else(|| {
                            format!(
                                "Cannot resolve any ancestor of '{path}' — reached filesystem root"
                            )
                        })?;
                        suffix_components.push(file_name);
                        ancestor = ancestor.parent().ok_or_else(|| {
                            format!("Cannot resolve parent while walking up '{path}'")
                        })?;
                    }
                }
            };
            let mut built = canonical_ancestor;
            for comp in suffix_components.iter().rev() {
                built.push(comp);
            }
            built
        }
    };

    // Step 4: enforce jail containment.
    if strict_mode() {
        let in_project = path_is_within(&canonical, &PROJECT_ROOT);
        let in_temp = TEMP_ROOT
            .as_ref()
            .is_some_and(|t| path_is_within(&canonical, t));

        if !in_project && !in_temp {
            return Err(format!(
                "Path '{path}' resolves to '{}' which is outside the project root ('{}') \
                 and outside the process temp directory. Set \
                 OPENCLAUDIA_ALLOW_OUT_OF_ROOT=1 to disable this jail (not recommended).",
                canonical.display(),
                PROJECT_ROOT.display(),
            ));
        }
    }

    Ok(canonical)
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
