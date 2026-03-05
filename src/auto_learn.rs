//! Auto-learning module for OpenClaudia
//!
//! Captures knowledge automatically from tool execution signals,
//! user corrections, and session patterns. No model discretion required.

use crate::memory::MemoryDb;
use std::collections::HashSet;
use tracing::{debug, warn};

/// Tracks pending error context for resolution matching
struct PendingError {
    error_signature: String,
    file_context: Option<String>,
}

/// AutoLearner captures knowledge from tool signals and user interactions
pub struct AutoLearner<'a> {
    db: &'a MemoryDb,
    /// Files modified in this session (for co-edit tracking)
    session_files_modified: HashSet<String>,
    /// Last error seen (for resolution matching on subsequent success)
    pending_error: Option<PendingError>,
}

impl<'a> AutoLearner<'a> {
    pub fn new(db: &'a MemoryDb) -> Self {
        Self {
            db,
            session_files_modified: HashSet::new(),
            pending_error: None,
        }
    }

    /// Called after a tool executes successfully
    pub fn on_tool_success(&mut self, tool_name: &str, args: &serde_json::Value, result: &str) {
        match tool_name {
            "edit_file" | "write_file" => {
                self.handle_file_write_success(args, result);
            }
            "bash" => {
                self.handle_bash_success(args, result);
            }
            _ => {}
        }
    }

    /// Called after a tool execution fails
    pub fn on_tool_failure(&mut self, tool_name: &str, args: &serde_json::Value, error: &str) {
        match tool_name {
            "bash" => {
                self.handle_bash_failure(args, error);
            }
            "edit_file" => {
                self.handle_edit_failure(args, error);
            }
            _ => {}
        }
    }

    /// Called when the user sends a message (for correction/preference detection)
    pub fn on_user_message(&mut self, message: &str, _previous_assistant: Option<&str>) {
        self.detect_preferences(message);
    }

    /// Called at session end to finalize learnings
    pub fn on_session_end(&mut self) {
        self.compute_file_relationships();
    }

    // === Internal: File Write Success ===

    fn handle_file_write_success(&mut self, args: &serde_json::Value, _result: &str) {
        let file_path = args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if file_path.is_empty() {
            return;
        }

        self.session_files_modified.insert(file_path.to_string());

        // If there was a pending error for this file, the edit might be the resolution
        if let Some(ref pending) = self.pending_error {
            if pending.file_context.as_deref() == Some(file_path) {
                let resolution = "File was edited after error";
                if let Err(e) = self.db.resolve_error_pattern(
                    &pending.error_signature,
                    pending.file_context.as_deref(),
                    resolution,
                ) {
                    warn!("Failed to resolve error pattern: {}", e);
                }
                self.pending_error = None;
            }
        }
    }

    // === Internal: Bash Success ===

    fn handle_bash_success(&mut self, args: &serde_json::Value, result: &str) {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");

        // If a bash command succeeds after a pending error, record the resolution
        if let Some(pending) = self.pending_error.take() {
            let resolution = format!("Resolved by running: {}", truncate_str(command, 100));
            if let Err(e) = self.db.resolve_error_pattern(
                &pending.error_signature,
                pending.file_context.as_deref(),
                &resolution,
            ) {
                warn!("Failed to resolve error pattern: {}", e);
            }
        }

        // Detect clippy/fmt patterns from successful runs
        if command.contains("cargo clippy") || command.contains("cargo fmt") {
            self.extract_lint_patterns(command, result);
        }
    }

    // === Internal: Bash Failure ===

    fn handle_bash_failure(&mut self, args: &serde_json::Value, error: &str) {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");

        // Extract error signature (first meaningful line)
        let error_sig = extract_error_signature(error);
        if error_sig.is_empty() {
            return;
        }

        // Try to extract file context from the error or command
        let file_context =
            extract_file_from_error(error).or_else(|| extract_file_from_command(command));

        debug!(
            "Recording error pattern: sig={}, file={:?}",
            error_sig, file_context
        );

        if let Err(e) = self.db.save_error_pattern(
            &error_sig,
            file_context.as_deref(),
            None, // No resolution yet
        ) {
            warn!("Failed to save error pattern: {}", e);
        }

        // Store as pending so we can match resolution later
        self.pending_error = Some(PendingError {
            error_signature: error_sig,
            file_context,
        });
    }

    // === Internal: Edit Failure ===

    fn handle_edit_failure(&mut self, args: &serde_json::Value, error: &str) {
        let file_path = args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if file_path.is_empty() {
            return;
        }

        // Record as a pitfall for this file
        if error.contains("not found") || error.contains("no match") {
            if let Err(e) = self.db.save_coding_pattern(
                file_path,
                "pitfall",
                "File content changes frequently; always re-read before editing",
            ) {
                warn!("Failed to save coding pattern: {}", e);
            }
        }
    }

    // === Internal: Lint Pattern Extraction ===

    fn extract_lint_patterns(&self, _command: &str, result: &str) {
        // Look for clippy warnings that mention files
        for line in result.lines() {
            if let Some(pattern) = parse_clippy_warning(line) {
                if let Err(e) =
                    self.db
                        .save_coding_pattern(&pattern.file, "convention", &pattern.description)
                {
                    warn!("Failed to save lint pattern: {}", e);
                }
            }
        }
    }

    // === Internal: Preference Detection ===

    fn detect_preferences(&self, message: &str) {
        let lower = message.to_lowercase();
        let trimmed = lower.trim();

        // Detect explicit preference statements
        let preference_patterns = [
            ("always ", "style"),
            ("never ", "style"),
            ("prefer ", "style"),
            ("don't use ", "style"),
            ("use ", "workflow"),
        ];

        for (prefix, category) in &preference_patterns {
            if trimmed.starts_with(prefix) && trimmed.len() < 200 {
                // Short enough to be a preference, not a code block
                if let Err(e) =
                    self.db
                        .save_learned_preference(category, message.trim(), Some("user_message"))
                {
                    warn!("Failed to save preference: {}", e);
                }
                return;
            }
        }

        // Detect corrections
        let correction_starts = ["no,", "wrong", "don't", "stop", "actually,", "instead,"];
        for start in &correction_starts {
            if trimmed.starts_with(start) && trimmed.len() < 300 {
                if let Err(e) = self.db.save_learned_preference(
                    "correction",
                    message.trim(),
                    Some("user_correction"),
                ) {
                    warn!("Failed to save correction: {}", e);
                }
                return;
            }
        }
    }

    // === Internal: Session End ===

    fn compute_file_relationships(&self) {
        let files: Vec<&String> = self.session_files_modified.iter().collect();
        if files.len() < 2 {
            return;
        }

        // Record pairwise co-edit relationships
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                if let Err(e) = self.db.save_file_relationship(files[i], files[j]) {
                    warn!("Failed to save file relationship: {}", e);
                }
            }
        }

        debug!(
            "Recorded {} file co-edit relationships",
            files.len() * (files.len() - 1) / 2
        );
    }
}

// === Helper Functions ===

/// Extract the most meaningful error line from stderr output
fn extract_error_signature(error: &str) -> String {
    for line in error.lines() {
        let trimmed = line.trim();
        // Skip empty lines and common noise
        if trimmed.is_empty()
            || trimmed.starts_with("warning:")
            || trimmed.starts_with("Compiling")
            || trimmed.starts_with("Downloading")
            || trimmed.starts_with("Finished")
            || trimmed == "^"
        {
            continue;
        }
        // Found a meaningful error line
        return truncate_str(trimmed, 200).to_string();
    }
    String::new()
}

/// Try to extract a file path from an error message
fn extract_file_from_error(error: &str) -> Option<String> {
    for line in error.lines() {
        let trimmed = line.trim();
        // Match patterns like "error[E0308]: src/main.rs:42:5" or "  --> src/main.rs:42:5"
        if let Some(arrow_pos) = trimmed.find("--> ") {
            let after = &trimmed[arrow_pos + 4..];
            if let Some(colon_pos) = after.find(':') {
                let path = &after[..colon_pos];
                if path.contains('/') || path.contains('\\') {
                    return Some(path.to_string());
                }
            }
        }
        // Match "error: file.rs" or similar
        if trimmed.starts_with("error") {
            for word in trimmed.split_whitespace() {
                if (word.ends_with(".rs")
                    || word.ends_with(".py")
                    || word.ends_with(".ts")
                    || word.ends_with(".js"))
                    && (word.contains('/') || word.contains('\\'))
                {
                    return Some(
                        word.trim_matches(|c: char| {
                            !c.is_alphanumeric()
                                && c != '/'
                                && c != '\\'
                                && c != '.'
                                && c != '_'
                                && c != '-'
                        })
                        .to_string(),
                    );
                }
            }
        }
    }
    None
}

/// Try to extract a file path from a command string
fn extract_file_from_command(command: &str) -> Option<String> {
    for word in command.split_whitespace() {
        if (word.ends_with(".rs")
            || word.ends_with(".py")
            || word.ends_with(".ts")
            || word.ends_with(".js")
            || word.ends_with(".toml")
            || word.ends_with(".yaml")
            || word.ends_with(".json"))
            && (word.contains('/') || word.contains('\\'))
        {
            return Some(word.to_string());
        }
    }
    None
}

/// Parsed clippy warning
struct ClippyPattern {
    file: String,
    description: String,
}

/// Parse a clippy warning line into a pattern
fn parse_clippy_warning(line: &str) -> Option<ClippyPattern> {
    // Match "warning: <description>" lines followed by file references
    // Or "warning: <lint_name>" at "src/file.rs:line:col"
    let trimmed = line.trim();

    if !trimmed.starts_with("warning:") {
        return None;
    }

    let description = trimmed.strip_prefix("warning: ")?.trim().to_string();

    // Skip meta warnings
    if description.starts_with("unused import")
        || description.starts_with("unused variable")
        || description.contains("generated")
    {
        return None;
    }

    // Try to find a file reference in the same line
    if let Some(file) = extract_file_from_error(trimmed) {
        return Some(ClippyPattern { file, description });
    }

    None
}

/// Truncate a string to a max length, appending "..." if truncated
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find a safe UTF-8 boundary
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_db() -> (tempfile::TempDir, MemoryDb) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();
        (dir, db)
    }

    #[test]
    fn test_auto_learner_creation() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);
        assert!(learner.session_files_modified.is_empty());
        assert!(learner.pending_error.is_none());
    }

    #[test]
    fn test_file_write_tracking() {
        let (_dir, db) = create_test_db();
        let mut learner = AutoLearner::new(&db);

        let args = serde_json::json!({"path": "src/main.rs"});
        learner.on_tool_success("edit_file", &args, "success");

        assert!(learner.session_files_modified.contains("src/main.rs"));
    }

    #[test]
    fn test_bash_failure_records_error() {
        let (_dir, db) = create_test_db();
        let mut learner = AutoLearner::new(&db);

        let args = serde_json::json!({"command": "cargo build"});
        learner.on_tool_failure(
            "bash",
            &args,
            "error[E0308]: mismatched types\n  --> src/main.rs:42:5",
        );

        assert!(learner.pending_error.is_some());
        let errors = db.get_error_patterns_for_file("src/main.rs").unwrap();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_error_resolution_on_success() {
        let (_dir, db) = create_test_db();
        let mut learner = AutoLearner::new(&db);

        // First, a failure
        let args = serde_json::json!({"command": "cargo build"});
        learner.on_tool_failure(
            "bash",
            &args,
            "error[E0308]: mismatched types\n  --> src/main.rs:42:5",
        );

        // Then a success that resolves it
        let fix_args = serde_json::json!({"command": "cargo build"});
        learner.on_tool_success("bash", &fix_args, "Compiling...\nFinished");

        assert!(learner.pending_error.is_none());
    }

    #[test]
    fn test_session_end_file_relationships() {
        let (_dir, db) = create_test_db();
        let mut learner = AutoLearner::new(&db);

        // Simulate editing multiple files
        learner.session_files_modified.insert("src/main.rs".into());
        learner.session_files_modified.insert("src/tools.rs".into());
        learner
            .session_files_modified
            .insert("src/memory.rs".into());

        learner.on_session_end();

        // Should have recorded 3 pairwise relationships
        let related = db.get_related_files("src/main.rs").unwrap();
        assert_eq!(related.len(), 2);
    }

    #[test]
    fn test_preference_detection() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("always use snake_case for function names");

        let prefs = db.get_all_preferences().unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].category, "style");
    }

    #[test]
    fn test_correction_detection() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("no, use tabs not spaces");

        let prefs = db.get_all_preferences().unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].category, "correction");
    }

    #[test]
    fn test_extract_error_signature() {
        assert_eq!(
            extract_error_signature("error[E0308]: mismatched types\n  --> src/main.rs:42:5"),
            "error[E0308]: mismatched types"
        );
        assert_eq!(extract_error_signature(""), "");
        assert_eq!(
            extract_error_signature("Compiling foo\nwarning: unused\nerror: aborting"),
            "error: aborting"
        );
    }

    #[test]
    fn test_extract_file_from_error() {
        assert_eq!(
            extract_file_from_error("  --> src/main.rs:42:5"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(extract_file_from_error("no file here"), None);
    }

    #[test]
    fn test_glob_matches() {
        use crate::memory::glob_matches;
        assert!(glob_matches("src/main.rs", "src/main.rs"));
        assert!(glob_matches("src/*.rs", "src/main.rs"));
        assert!(glob_matches("src/*", "src/main.rs"));
        assert!(!glob_matches("src/*.rs", "tests/test.rs"));
        assert!(glob_matches("*.rs", "src/main.rs"));
    }
}
