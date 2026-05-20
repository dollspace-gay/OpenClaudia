//! Auto-learning module for `OpenClaudia`
//!
//! Captures knowledge automatically from tool execution signals,
//! user corrections, and session patterns. No model discretion required.
//!
//! # SQL discipline (crosslink #443)
//!
//! **`format!`-based SQL construction is FORBIDDEN in this module.**
//!
//! Earlier revisions interpolated both table names and row-limit values
//! directly into `DELETE` strings via `format!("... {table} ... {max_rows}")`
//! and then handed the result to `execute_raw`. Although every interpolated
//! value was a compile-time constant at the time, the pattern set a
//! dangerous precedent: a future contributor passing a computed table name
//! or a config-derived limit would silently introduce SQL injection.
//!
//! Discipline enforced here:
//!
//! 1. Table identities are expressed through the [`PruneTable`] enum (a
//!    re-export of [`crate::memory::AutoLearnTable`]). The enum is the
//!    single source of truth for prunable tables — each variant maps to a
//!    compile-time-known prepared statement string inside
//!    [`MemoryDb::prune_auto_learn_table`]. Adding a new prunable table
//!    forces the compiler to update every `match` site, so no caller can
//!    smuggle a string through.
//! 2. Row counts (`max_rows` / `keep`) are bound via `params![keep]`,
//!    never formatted into the SQL string. The only `?` parameter in the
//!    `DELETE` is the `LIMIT` value, which `SQLite` treats as an integer
//!    literal — there is no parser surface a hostile value can reach.
//! 3. Direct calls to `format!(...)` followed by `execute_raw` /
//!    `conn.execute` are prohibited anywhere in this module. Logging
//!    helpers that use `format!` to build a *log label* (not SQL) are
//!    permitted; any future use must include a comment justifying that
//!    the formatted string is never passed to a SQL-executing function.

use crate::memory::MemoryDb;
use std::collections::HashSet;
use tracing::debug;

/// Allowlist of tables that the auto-learning prune sweep is permitted to
/// touch. Re-exported from [`crate::memory::AutoLearnTable`] so the
/// `auto_learn` module's vocabulary matches the security mandate from
/// crosslink #443 ("define `enum PruneTable`") while preserving a single
/// source of truth in the storage layer.
///
/// Adding a new variant requires updating the exhaustive `match` in
/// [`MemoryDb::prune_auto_learn_table`], which forces a compile-time-known
/// prepared statement to exist for every prunable table.
pub use crate::memory::AutoLearnTable as PruneTable;

/// Tracks pending error context for resolution matching
struct PendingError {
    error_signature: String,
    file_context: Option<String>,
}

/// `AutoLearner` captures knowledge from tool signals and user interactions
pub struct AutoLearner<'a> {
    db: &'a MemoryDb,
    /// Files modified in this session (for co-edit tracking)
    session_files_modified: HashSet<String>,
    /// Last error seen (for resolution matching on subsequent success)
    pending_error: Option<PendingError>,
    /// Count of database errors — indicates degraded auto-learning
    db_error_count: std::sync::atomic::AtomicU32,
}

impl<'a> AutoLearner<'a> {
    pub fn new(db: &'a MemoryDb) -> Self {
        Self {
            db,
            session_files_modified: HashSet::new(),
            pending_error: None,
            db_error_count: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Number of database errors encountered during this session.
    /// If non-zero, the auto-learning system is degraded.
    #[must_use]
    pub fn error_count(&self) -> u32 {
        self.db_error_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Log a database error and increment the failure counter.
    fn log_db_error(&self, operation: &str, err: &impl std::fmt::Display) {
        let count = self
            .db_error_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        tracing::warn!(
            operation,
            error = %err,
            total_errors = count,
            "Auto-learning database error (system degraded)"
        );
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

    /// Called at session end to finalize learnings and prune old data.
    pub fn on_session_end(&mut self) {
        self.compute_file_relationships();
        self.prune_old_data();
    }

    /// Prune auto-learned data to prevent unbounded growth.
    /// Keeps the most recent entries in each table.
    ///
    /// Each table is addressed through the [`PruneTable`] enum allowlist so
    /// no caller-controlled string can reach SQL (crosslink #255, #443).
    /// `max_rows` is bound as a `?` parameter inside
    /// [`MemoryDb::prune_auto_learn_table`] — never interpolated.
    ///
    /// The `format!` call below builds a *log label only*; the resulting
    /// string is never handed to a SQL-executing function. This is the one
    /// permitted `format!` use in this module per the discipline at the top
    /// of the file.
    fn prune_old_data(&self) {
        const MAX_CODING_PATTERNS: u32 = 500;
        const MAX_ERROR_PATTERNS: u32 = 200;
        const MAX_PREFERENCES: u32 = 100;
        const MAX_FILE_RELATIONSHIPS: u32 = 500;

        let prune_targets: [(PruneTable, u32); 4] = [
            (PruneTable::CodingPatterns, MAX_CODING_PATTERNS),
            (PruneTable::ErrorPatterns, MAX_ERROR_PATTERNS),
            (PruneTable::LearnedPreferences, MAX_PREFERENCES),
            (PruneTable::FileRelationships, MAX_FILE_RELATIONSHIPS),
        ];

        for (table, keep) in prune_targets {
            if let Err(e) = self.db.prune_auto_learn_table(table, keep) {
                // `format!` here builds a log label, NOT SQL — see module docs.
                self.log_db_error(&format!("prune_{table:?}"), &e);
            }
        }
    }

    /// Normalize a file path from tool arguments — canonicalize if possible,
    /// reject paths with `..` components to prevent path traversal in DB.
    fn normalize_path(raw: &str) -> Option<String> {
        if raw.is_empty() {
            return None;
        }
        let path = std::path::Path::new(raw);
        // Reject path traversal
        if path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return None;
        }
        // Canonicalize if file exists, otherwise use as-is
        std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .ok()
            .or_else(|| Some(raw.to_string()))
    }

    // === Internal: File Write Success ===

    fn handle_file_write_success(&mut self, args: &serde_json::Value, _result: &str) {
        let raw_path = args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let Some(file_path) = Self::normalize_path(raw_path) else {
            return;
        };

        self.session_files_modified.insert(file_path.clone());

        // If there was a pending error for this file, the edit might be the resolution
        if let Some(ref pending) = self.pending_error {
            if pending.file_context.as_deref() == Some(file_path.as_str()) {
                let resolution = "File was edited after error";
                if let Err(e) = self.db.resolve_error_pattern(
                    &pending.error_signature,
                    pending.file_context.as_deref(),
                    resolution,
                ) {
                    self.log_db_error("resolve_error_pattern", &e);
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
                self.log_db_error("resolve_error_pattern", &e);
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
            self.log_db_error("save_error_pattern", &e);
        }

        // Store as pending so we can match resolution later
        self.pending_error = Some(PendingError {
            error_signature: error_sig,
            file_context,
        });
    }

    // === Internal: Edit Failure ===

    fn handle_edit_failure(&self, args: &serde_json::Value, error: &str) {
        let raw_path = args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let Some(file_path) = Self::normalize_path(raw_path) else {
            return;
        };

        // Record as a pitfall for this file
        if error.contains("not found") || error.contains("no match") {
            if let Err(e) = self.db.save_coding_pattern(
                &file_path,
                "pitfall",
                "File content changes frequently; always re-read before editing",
            ) {
                self.log_db_error("save_coding_pattern", &e);
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
                    self.log_db_error("save_lint_pattern", &e);
                }
            }
        }
    }

    // === Internal: Preference Detection ===

    /// Heuristically classify a user message as a preference statement.
    ///
    /// The original heuristic recorded any short message whose lowercased
    /// text began with `always`, `never`, `prefer`, `use`, `dont use`, etc.
    /// That captured tool-invocation imperatives ("use the `read_file` tool"),
    /// idiomatic phrases ("never mind"), and questions ("should I always
    /// X?") as preferences, polluting `learned_preferences` (crosslink #448).
    ///
    /// Tightened gate:
    ///
    /// * Message must be a single sentence with no `?` characters anywhere
    ///   — preferences are imperative, not interrogative.
    /// * The preference verb must appear at position 0 of the trimmed
    ///   lowercased message (imperative mood), so conditionals like
    ///   `if you always X` and subordinate clauses are rejected.
    /// * Following the verb there must be a substantive object phrase of
    ///   at least two alphabetic tokens.
    /// * Idiomatic non-preferences (`never mind`, `dont worry`, etc.) are
    ///   denylisted.
    /// * The bare `use ` prefix is removed entirely — it almost always
    ///   introduces a tool-invocation imperative.
    /// * Correction prefixes must end at a clause boundary (comma or
    ///   period) so they cannot match tool imperatives.
    fn detect_preferences(&self, message: &str) {
        let trimmed_raw = message.trim();
        if trimmed_raw.is_empty() {
            return;
        }
        let lower = trimmed_raw.to_lowercase();

        if !is_single_imperative_sentence(&lower) {
            return;
        }

        // Preference verbs in imperative mood. The bare `use ` prefix was
        // removed deliberately — see doc comment above.
        let preference_patterns: &[(&str, &str)] = &[
            ("always ", "style"),
            ("never ", "style"),
            ("prefer ", "style"),
            ("don't use ", "style"),
            ("dont use ", "style"),
        ];

        for (prefix, category) in preference_patterns {
            if let Some(rest) = lower.strip_prefix(prefix) {
                if !is_substantive_object_phrase(rest) {
                    continue;
                }
                if is_idiomatic_non_preference(&lower) {
                    continue;
                }
                if trimmed_raw.len() >= 200 {
                    continue;
                }
                if let Err(e) =
                    self.db
                        .save_learned_preference(category, trimmed_raw, Some("user_message"))
                {
                    self.log_db_error("save_preference", &e);
                }
                return;
            }
        }

        // Correction prefixes — each must end at a punctuation boundary.
        let correction_patterns: &[&str] =
            &["no, ", "wrong, ", "wrong. ", "actually, ", "instead, "];

        for prefix in correction_patterns {
            if let Some(rest) = lower.strip_prefix(prefix) {
                if !is_substantive_object_phrase(rest) {
                    continue;
                }
                if trimmed_raw.len() >= 300 {
                    continue;
                }
                if let Err(e) = self.db.save_learned_preference(
                    "correction",
                    trimmed_raw,
                    Some("user_correction"),
                ) {
                    self.log_db_error("save_correction", &e);
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
                    self.log_db_error("save_file_relationship", &e);
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

/// Check if a word has a source-code file extension (case-insensitive).
fn has_source_extension(word: &str) -> bool {
    let path = std::path::Path::new(word);
    path.extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("rs")
            || ext.eq_ignore_ascii_case("py")
            || ext.eq_ignore_ascii_case("ts")
            || ext.eq_ignore_ascii_case("js")
    })
}

/// Check if a word has a config/source file extension (case-insensitive).
fn has_file_extension(word: &str) -> bool {
    let path = std::path::Path::new(word);
    path.extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("rs")
            || ext.eq_ignore_ascii_case("py")
            || ext.eq_ignore_ascii_case("ts")
            || ext.eq_ignore_ascii_case("js")
            || ext.eq_ignore_ascii_case("toml")
            || ext.eq_ignore_ascii_case("yaml")
            || ext.eq_ignore_ascii_case("json")
    })
}

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
                if has_source_extension(word) && (word.contains('/') || word.contains('\\')) {
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
        if has_file_extension(word) && (word.contains('/') || word.contains('\\')) {
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

/// Return `true` if `lower` is a single, non-interrogative sentence.
///
/// Used to gate preference detection (crosslink #448). The lowercased
/// message must contain zero `?` characters anywhere and at most one
/// trailing sentence terminator (`.` or `!`); any internal terminator that
/// splits the text into two clauses with substantive alphabetic content on
/// both sides is rejected.
fn is_single_imperative_sentence(lower: &str) -> bool {
    if lower.contains('?') {
        return false;
    }
    let terminators = ['.', '!'];
    let mut saw_terminator = false;
    let mut alpha_in_current_clause = false;

    for ch in lower.chars() {
        if terminators.contains(&ch) {
            if alpha_in_current_clause {
                if saw_terminator {
                    return false;
                }
                saw_terminator = true;
                alpha_in_current_clause = false;
            }
        } else if ch.is_alphabetic() {
            if saw_terminator {
                // Alphabetic content after a closed sentence opens a
                // second clause — disallowed.
                return false;
            }
            alpha_in_current_clause = true;
        }
    }
    true
}

/// Return `true` if `rest` (the portion of a message after the preference
/// verb prefix) contains at least two alphabetic tokens, so we don't record
/// bare exclamations like "always!" or "never." as preferences.
fn is_substantive_object_phrase(rest: &str) -> bool {
    let alpha_tokens = rest
        .split(|c: char| !c.is_alphabetic())
        .filter(|t| !t.is_empty())
        .count();
    alpha_tokens >= 2
}

/// Idiomatic phrases that pattern-match a preference verb but carry no
/// preference content. Listed in lowercased form.
fn is_idiomatic_non_preference(lower: &str) -> bool {
    const IDIOMS: &[&str] = &[
        "never mind",
        "always has been",
        "don't worry",
        "dont worry",
        "don't bother",
        "dont bother",
        "don't sweat it",
        "dont sweat it",
    ];
    IDIOMS.iter().any(|idiom| lower.starts_with(idiom))
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

        // normalize_path canonicalizes if file exists, keeps as-is otherwise
        let expected = std::fs::canonicalize("src/main.rs").map_or_else(
            |_| "src/main.rs".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        assert!(learner.session_files_modified.contains(&expected));
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

    // === Crosslink #448 regression coverage =================================
    //
    // The original `detect_preferences` recorded any short message starting
    // with `always`/`never`/`prefer`/`use`/`dont` as a preference, capturing
    // tool-invocation imperatives, questions, conditionals, and idioms.
    // Each test below documents one false-positive class that the tightened
    // heuristic must reject, plus positive cases that must still record.

    #[test]
    fn fix448_question_starting_with_always_is_not_a_preference() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("always run the formatter before commit?");

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "interrogative message must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_use_as_tool_invocation_is_not_a_preference() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        // The motivating false positive from the issue.
        learner.detect_preferences("use the read_file tool to check config.yaml");

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "bare `use ...` tool invocation must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_never_mind_idiom_is_not_a_preference() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("never mind, try a different approach");

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "`never mind` idiom must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_multi_sentence_message_is_not_a_preference() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("always check first. then run the script and report back");

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "multi-sentence message must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_conditional_clause_is_not_a_preference() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        // "if you always X" is a conditional, not a directive.
        learner.detect_preferences("if you always rerun the tests it will pass eventually");

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "conditional clause must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_question_form_with_multiple_punctuation_is_not_a_preference() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("prefer tabs?? or spaces");

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "message containing `?` must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_prefer_with_multi_sentence_discussion_not_recorded() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences(
            "prefer we skip testing for now. it will be faster and we can revisit later.",
        );

        let prefs = db.get_all_preferences().unwrap();
        assert!(
            prefs.is_empty(),
            "multi-sentence `prefer ...` discussion must not be recorded, got {prefs:?}"
        );
    }

    #[test]
    fn fix448_genuine_preference_still_recorded() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("always use snake_case for function names");

        let prefs = db.get_all_preferences().unwrap();
        assert_eq!(
            prefs.len(),
            1,
            "genuine imperative preference must still be recorded, got {prefs:?}"
        );
        assert_eq!(prefs[0].category, "style");
    }

    #[test]
    fn fix448_genuine_preference_with_trailing_period_recorded() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("prefer explicit error types over anyhow.");

        let prefs = db.get_all_preferences().unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].category, "style");
    }

    #[test]
    fn fix448_genuine_correction_still_recorded() {
        let (_dir, db) = create_test_db();
        let learner = AutoLearner::new(&db);

        learner.detect_preferences("actually, use tabs not spaces");

        let prefs = db.get_all_preferences().unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].category, "correction");
    }

    #[test]
    fn fix448_single_imperative_sentence_helper() {
        assert!(is_single_imperative_sentence("always use snake_case"));
        assert!(is_single_imperative_sentence("prefer tabs over spaces."));
        assert!(is_single_imperative_sentence(
            "never panic in library code!"
        ));

        assert!(!is_single_imperative_sentence(
            "should i always run the tests?"
        ));
        assert!(!is_single_imperative_sentence(
            "always check first. then run it"
        ));
        assert!(!is_single_imperative_sentence("prefer x? or y"));
    }

    #[test]
    fn fix448_substantive_object_phrase_helper() {
        assert!(is_substantive_object_phrase("use snake_case for names"));
        assert!(is_substantive_object_phrase("tabs over spaces"));

        assert!(!is_substantive_object_phrase(""));
        assert!(!is_substantive_object_phrase("!"));
        assert!(!is_substantive_object_phrase("x"));
    }

    #[test]
    fn fix448_idiomatic_non_preference_helper() {
        assert!(is_idiomatic_non_preference("never mind"));
        assert!(is_idiomatic_non_preference("never mind, try again"));
        assert!(is_idiomatic_non_preference("don't worry about it"));
        assert!(is_idiomatic_non_preference("dont worry about it"));

        assert!(!is_idiomatic_non_preference("never panic in library code"));
        assert!(!is_idiomatic_non_preference("always use snake_case"));
    }

    // === Crosslink #443 regression coverage =================================
    //
    // `prune_old_data` previously assembled DELETE statements via
    //   format!("DELETE FROM {table} ... LIMIT {max_rows}")
    // and dispatched them through `execute_raw`. Even though every
    // interpolated value was a compile-time constant, the pattern set a
    // precedent for future SQL injection. The fix routes every table through
    // the [`PruneTable`] enum allowlist (re-exported from `memory::AutoLearnTable`)
    // and binds `max_rows` as a `?` query parameter. The tests below prove:
    //
    //   1. Each variant executes a real, syntactically valid DELETE.
    //   2. `keep = 0` truncates the table (parameter binding is honoured,
    //      not silently replaced by some default).
    //   3. `keep` larger than the row count is a no-op (LIMIT clamps).
    //   4. The variant set is exhaustive — every `PruneTable` discriminant
    //      has a compiled prepared statement (compile-time enforcement via
    //      an unreachable arm-less `match`, plus a runtime smoke-test that
    //      every variant returns Ok).

    /// Insert N rows into every auto-learn table so prune behaviour can be
    /// observed. Returns the per-table row count after population so tests
    /// can assert against a known baseline.
    fn populate_all_tables(db: &MemoryDb, rows: u32) {
        for i in 0..rows {
            db.save_coding_pattern(
                &format!("src/f{i}.rs"),
                "convention",
                &format!("pattern-{i}"),
            )
            .unwrap();
            db.save_error_pattern(&format!("err-sig-{i}"), Some(&format!("src/f{i}.rs")), None)
                .unwrap();
            db.save_learned_preference("style", &format!("pref-{i}"), Some("test"))
                .unwrap();
        }
        // file_relationships requires two distinct files.
        for i in 0..rows {
            db.save_file_relationship(&format!("src/a{i}.rs"), &format!("src/b{i}.rs"))
                .unwrap();
        }
    }

    /// #443 — Every [`PruneTable`] variant routes through
    /// `prune_old_data` without a SQL syntax error or panic.
    ///
    /// `prune_old_data` calls `prune_auto_learn_table` for every variant in
    /// sequence; if any variant emitted malformed SQL or a missing
    /// prepared statement, the inner `MemoryDb` call would push an error
    /// onto `error_count`. We seed the DB with rows below the prune cap
    /// (which is 100+ for every table) so the DELETE is a structural
    /// exercise — no rows should actually be removed.
    #[test]
    fn fix443_prune_old_data_each_variant_succeeds() {
        let (_dir, db) = create_test_db();
        let mut learner = AutoLearner::new(&db);

        populate_all_tables(&db, 3);
        let before = db.auto_learn_stats().unwrap();
        assert_eq!(before.coding_patterns, 3);
        assert_eq!(before.error_patterns, 3);
        assert_eq!(before.learned_preferences, 3);
        assert_eq!(before.file_relationships, 3);

        learner.on_session_end(); // exercises prune_old_data + compute_file_relationships

        assert_eq!(
            learner.error_count(),
            0,
            "prune_old_data must not record any DB error — got {} errors",
            learner.error_count()
        );

        // Row counts must be unchanged (well below each table's keep cap).
        let after = db.auto_learn_stats().unwrap();
        assert_eq!(after.coding_patterns, before.coding_patterns);
        assert_eq!(after.error_patterns, before.error_patterns);
        assert_eq!(after.learned_preferences, before.learned_preferences);
    }

    /// #443 — `prune_auto_learn_table(table, 0)` truncates the table.
    ///
    /// This proves the `?1` parameter actually reaches `SQLite` — if the
    /// binding were dropped (e.g. silently replaced with the LIMIT cap of
    /// `-1` / unlimited), the DELETE would remove zero rows instead of all
    /// of them. Each variant is checked independently so any future regression
    /// in a single arm of the `match` is caught.
    #[test]
    fn fix443_prune_with_keep_zero_truncates_table() {
        let (_dir, db) = create_test_db();
        populate_all_tables(&db, 5);

        let stats_before = db.auto_learn_stats().unwrap();
        assert_eq!(stats_before.coding_patterns, 5);
        assert_eq!(stats_before.error_patterns, 5);
        assert_eq!(stats_before.learned_preferences, 5);
        assert_eq!(stats_before.file_relationships, 5);

        for table in [
            PruneTable::CodingPatterns,
            PruneTable::ErrorPatterns,
            PruneTable::LearnedPreferences,
            PruneTable::FileRelationships,
        ] {
            db.prune_auto_learn_table(table, 0)
                .unwrap_or_else(|e| panic!("prune {table:?} keep=0 failed: {e}"));
        }

        let stats_after = db.auto_learn_stats().unwrap();
        assert_eq!(
            stats_after.coding_patterns, 0,
            "keep=0 must clear coding_patterns"
        );
        assert_eq!(
            stats_after.error_patterns, 0,
            "keep=0 must clear error_patterns"
        );
        assert_eq!(
            stats_after.learned_preferences, 0,
            "keep=0 must clear learned_preferences"
        );
        assert_eq!(
            stats_after.file_relationships, 0,
            "keep=0 must clear file_relationships"
        );
    }

    /// #443 — `keep` much larger than the table size is a no-op.
    ///
    /// `SQLite`'s `LIMIT N` clamps when `N` exceeds the row count; the prune
    /// DELETE's subquery should therefore name every row as "to keep" and
    /// delete nothing. If the parameter were ignored, every row would be
    /// removed instead.
    #[test]
    fn fix443_prune_with_huge_keep_leaves_table_intact() {
        let (_dir, db) = create_test_db();
        populate_all_tables(&db, 4);

        for table in [
            PruneTable::CodingPatterns,
            PruneTable::ErrorPatterns,
            PruneTable::LearnedPreferences,
            PruneTable::FileRelationships,
        ] {
            // 1_000_000 ≫ 4 rows; LIMIT must clamp, DELETE must be a no-op.
            db.prune_auto_learn_table(table, 1_000_000).unwrap();
        }

        let stats = db.auto_learn_stats().unwrap();
        assert_eq!(stats.coding_patterns, 4);
        assert_eq!(stats.error_patterns, 4);
        assert_eq!(stats.learned_preferences, 4);
        assert_eq!(stats.file_relationships, 4);
    }

    /// #443 — Every `PruneTable` variant has a compiled prepared statement.
    ///
    /// The `match _v { PruneTable::X => () }` block below is the
    /// compile-time half of this test: if a future contributor adds a new
    /// variant to `AutoLearnTable` (= `PruneTable`) without extending the
    /// match in [`MemoryDb::prune_auto_learn_table`], this match will fail
    /// non-exhaustively at compile time. The runtime half iterates the
    /// known variants and asserts each one returns `Ok(())` against a
    /// fresh, empty database — proving each variant maps to real,
    /// executable SQL rather than a stub or panic macro.
    #[test]
    fn fix443_every_prune_table_variant_has_prepared_statement() {
        // Compile-time exhaustiveness probe. If a new PruneTable variant is
        // added, this match becomes non-exhaustive and the build fails,
        // forcing the author to (a) extend this test AND (b) extend the
        // prepared-statement match inside prune_auto_learn_table.
        fn _exhaustive_probe(v: PruneTable) {
            match v {
                PruneTable::CodingPatterns
                | PruneTable::ErrorPatterns
                | PruneTable::LearnedPreferences
                | PruneTable::FileRelationships => {}
            }
        }

        let (_dir, db) = create_test_db();

        // Runtime half: every known variant must execute without error.
        // Run twice — once on an empty DB, once on a populated one — to
        // exercise both code paths through the DELETE.
        let variants = [
            PruneTable::CodingPatterns,
            PruneTable::ErrorPatterns,
            PruneTable::LearnedPreferences,
            PruneTable::FileRelationships,
        ];

        for table in variants {
            db.prune_auto_learn_table(table, 10)
                .unwrap_or_else(|e| panic!("empty-DB prune {table:?} failed: {e}"));
        }

        populate_all_tables(&db, 2);

        for table in variants {
            db.prune_auto_learn_table(table, 10)
                .unwrap_or_else(|e| panic!("populated-DB prune {table:?} failed: {e}"));
        }

        // Sanity: no variant silently dropped rows when keep > row count.
        let stats = db.auto_learn_stats().unwrap();
        assert_eq!(stats.coding_patterns, 2);
        assert_eq!(stats.error_patterns, 2);
        assert_eq!(stats.learned_preferences, 2);
        assert_eq!(stats.file_relationships, 2);
    }
}
