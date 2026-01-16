//! Stateful memory module for OpenClaudia
//!
//! Implements Letta/MemGPT-style archival memory using SQLite.
//! Each project gets its own memory database that persists across sessions.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Memory database file name
const MEMORY_DB_NAME: &str = "memory.db";

/// Current schema version - increment when adding migrations
const SCHEMA_VERSION: i64 = 2;

/// Short-term memory expiration (hours)
const SHORT_TERM_EXPIRY_HOURS: i64 = 48;

/// Core memory section names
pub const SECTION_PERSONA: &str = "persona";
pub const SECTION_PROJECT_INFO: &str = "project_info";
pub const SECTION_USER_PREFS: &str = "user_preferences";

/// Recent session summary (short-term memory)
#[derive(Debug, Clone)]
pub struct RecentSession {
    pub id: i64,
    pub session_id: String,
    pub summary: String,
    pub files_modified: Vec<String>,
    pub issues_worked: Vec<String>,
    pub started_at: String,
    pub ended_at: String,
}

/// Recent activity entry
#[derive(Debug, Clone)]
pub struct RecentActivity {
    pub id: i64,
    pub session_id: String,
    pub activity_type: String, // "file_read", "file_write", "tool_call", "issue_created", "issue_closed"
    pub target: String,        // file path, tool name, issue number
    pub details: Option<String>,
    pub created_at: String,
}

/// A single archival memory entry
#[derive(Debug, Clone)]
pub struct ArchivalMemory {
    pub id: i64,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Core memory block (always in context)
#[derive(Debug, Clone)]
pub struct CoreMemory {
    pub section: String,
    pub content: String,
    pub updated_at: String,
}

/// Memory database handle
pub struct MemoryDb {
    conn: Connection,
    path: PathBuf,
}

impl MemoryDb {
    /// Open or create memory database at the specified path
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open memory database at {:?}", path))?;

        let mut db = Self {
            conn,
            path: path.to_path_buf(),
        };

        db.ensure_schema()?;
        Ok(db)
    }

    /// Open or create memory database in .openclaudia directory
    pub fn open_for_project(project_dir: &Path) -> Result<Self> {
        let openclaudia_dir = project_dir.join(".openclaudia");
        std::fs::create_dir_all(&openclaudia_dir).with_context(|| {
            format!(
                "Failed to create .openclaudia directory at {:?}",
                openclaudia_dir
            )
        })?;

        let db_path = openclaudia_dir.join(MEMORY_DB_NAME);
        Self::open(&db_path)
    }

    /// Get the database path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ensure database schema exists and run migrations
    fn ensure_schema(&mut self) -> Result<()> {
        // Create version tracking table first
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
            [],
        )?;

        // Get current version (0 if table is empty = new db or pre-versioning db)
        let current_version: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Run migrations
        if current_version < SCHEMA_VERSION {
            tracing::info!(
                "Migrating memory database from version {} to {}",
                current_version,
                SCHEMA_VERSION
            );
            self.run_migrations(current_version)?;
        }

        Ok(())
    }

    /// Run all migrations from current_version to SCHEMA_VERSION
    fn run_migrations(&mut self, from_version: i64) -> Result<()> {
        // Version 1: Original schema (archival_memory, core_memory)
        if from_version < 1 {
            self.migrate_v1()?;
        }

        // Version 2: Add short-term memory tables
        if from_version < 2 {
            self.migrate_v2()?;
        }

        // Record current version
        self.conn.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;

        tracing::info!(
            "Database migration complete. Now at version {}",
            SCHEMA_VERSION
        );
        Ok(())
    }

    /// Migration v1: Original schema
    fn migrate_v1(&mut self) -> Result<()> {
        tracing::debug!("Running migration v1: core schema");
        self.conn.execute_batch(
            r#"
            -- Archival memory table for long-term storage
            CREATE TABLE IF NOT EXISTS archival_memory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                tags TEXT DEFAULT '',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );

            -- FTS5 virtual table for full-text search
            CREATE VIRTUAL TABLE IF NOT EXISTS archival_memory_fts USING fts5(
                content,
                tags,
                content=archival_memory,
                content_rowid=id
            );

            -- Triggers to keep FTS index in sync
            CREATE TRIGGER IF NOT EXISTS archival_memory_ai AFTER INSERT ON archival_memory BEGIN
                INSERT INTO archival_memory_fts(rowid, content, tags)
                VALUES (new.id, new.content, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS archival_memory_ad AFTER DELETE ON archival_memory BEGIN
                INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content, tags)
                VALUES('delete', old.id, old.content, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS archival_memory_au AFTER UPDATE ON archival_memory BEGIN
                INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content, tags)
                VALUES('delete', old.id, old.content, old.tags);
                INSERT INTO archival_memory_fts(rowid, content, tags)
                VALUES (new.id, new.content, new.tags);
            END;

            -- Core memory table (always in context)
            CREATE TABLE IF NOT EXISTS core_memory (
                section TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                updated_at TEXT DEFAULT (datetime('now'))
            );

            -- Initialize default core memory sections if not exist
            INSERT OR IGNORE INTO core_memory (section, content) VALUES
                ('persona', 'I am an AI assistant helping with this project. I will learn about the codebase and remember important details across sessions.'),
                ('project_info', 'No project information recorded yet.'),
                ('user_preferences', 'No user preferences recorded yet.');
            "#,
        ).context("Failed to create v1 schema")?;

        Ok(())
    }

    /// Migration v2: Add short-term memory tables
    fn migrate_v2(&mut self) -> Result<()> {
        tracing::debug!("Running migration v2: short-term memory tables");
        self.conn
            .execute_batch(
                r#"
            -- Short-term memory: Recent session summaries
            CREATE TABLE IF NOT EXISTS recent_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT UNIQUE NOT NULL,
                summary TEXT NOT NULL,
                files_modified TEXT DEFAULT '',
                issues_worked TEXT DEFAULT '',
                started_at TEXT DEFAULT (datetime('now')),
                ended_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_recent_sessions_ended ON recent_sessions(ended_at);

            -- Short-term memory: Recent activity log
            CREATE TABLE IF NOT EXISTS recent_activity (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                activity_type TEXT NOT NULL,
                target TEXT NOT NULL,
                details TEXT,
                created_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_recent_activity_created ON recent_activity(created_at);
            CREATE INDEX IF NOT EXISTS idx_recent_activity_session ON recent_activity(session_id);
            "#,
            )
            .context("Failed to create v2 schema (short-term memory)")?;

        Ok(())
    }

    // === Archival Memory Operations ===

    /// Save a new memory entry
    pub fn memory_save(&self, content: &str, tags: &[String]) -> Result<i64> {
        let tags_str = tags.join(",");
        self.conn.execute(
            "INSERT INTO archival_memory (content, tags) VALUES (?1, ?2)",
            params![content, tags_str],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Search archival memory using full-text search
    pub fn memory_search(&self, query: &str, limit: usize) -> Result<Vec<ArchivalMemory>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT am.id, am.content, am.tags, am.created_at, am.updated_at,
                   bm25(archival_memory_fts) as rank
            FROM archival_memory_fts
            JOIN archival_memory am ON archival_memory_fts.rowid = am.id
            WHERE archival_memory_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;

        let memories = stmt
            .query_map(params![query, limit as i64], |row| {
                Ok(ArchivalMemory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    tags: row
                        .get::<_, String>(2)?
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(memories)
    }

    /// Get a memory by ID
    pub fn memory_get(&self, id: i64) -> Result<Option<ArchivalMemory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, tags, created_at, updated_at FROM archival_memory WHERE id = ?1",
        )?;

        let memory = stmt
            .query_row(params![id], |row| {
                Ok(ArchivalMemory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    tags: row
                        .get::<_, String>(2)?
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })
            .optional()?;

        Ok(memory)
    }

    /// Update an existing memory
    pub fn memory_update(&self, id: i64, content: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE archival_memory SET content = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![content, id],
        )?;
        Ok(rows > 0)
    }

    /// Delete a memory entry
    pub fn memory_delete(&self, id: i64) -> Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM archival_memory WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// List recent memories
    pub fn memory_list(&self, limit: usize) -> Result<Vec<ArchivalMemory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, tags, created_at, updated_at FROM archival_memory ORDER BY updated_at DESC LIMIT ?1",
        )?;

        let memories = stmt
            .query_map(params![limit as i64], |row| {
                Ok(ArchivalMemory {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    tags: row
                        .get::<_, String>(2)?
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(memories)
    }

    /// Get memory statistics
    pub fn memory_stats(&self) -> Result<MemoryStats> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM archival_memory", [], |row| row.get(0))?;

        let total_size: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(content)), 0) FROM archival_memory",
            [],
            |row| row.get(0),
        )?;

        let last_updated: Option<String> =
            self.conn
                .query_row("SELECT MAX(updated_at) FROM archival_memory", [], |row| {
                    row.get(0)
                })?;

        Ok(MemoryStats {
            count: count as usize,
            total_size: total_size as usize,
            last_updated,
        })
    }

    // === Core Memory Operations ===

    /// Get all core memory sections
    pub fn get_core_memory(&self) -> Result<Vec<CoreMemory>> {
        let mut stmt = self
            .conn
            .prepare("SELECT section, content, updated_at FROM core_memory ORDER BY section")?;

        let memories = stmt
            .query_map([], |row| {
                Ok(CoreMemory {
                    section: row.get(0)?,
                    content: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(memories)
    }

    /// Get a specific core memory section
    pub fn get_core_memory_section(&self, section: &str) -> Result<Option<CoreMemory>> {
        let mut stmt = self
            .conn
            .prepare("SELECT section, content, updated_at FROM core_memory WHERE section = ?1")?;

        let memory = stmt
            .query_row(params![section], |row| {
                Ok(CoreMemory {
                    section: row.get(0)?,
                    content: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })
            .optional()?;

        Ok(memory)
    }

    /// Update a core memory section
    pub fn update_core_memory(&self, section: &str, content: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO core_memory (section, content, updated_at) VALUES (?1, ?2, datetime('now'))",
            params![section, content],
        )?;
        Ok(())
    }

    /// Format core memory for injection into system prompt
    pub fn format_core_memory_for_prompt(&self) -> Result<String> {
        let core = self.get_core_memory()?;

        let mut output = String::from("<core_memory>\n");

        for mem in core {
            output.push_str(&format!(
                "<{}>\n{}\n</{}>\n",
                mem.section, mem.content, mem.section
            ));
        }

        output.push_str("</core_memory>");
        Ok(output)
    }

    /// Clear all archival memory (keeps core memory)
    pub fn clear_archival_memory(&self) -> Result<usize> {
        let rows = self.conn.execute("DELETE FROM archival_memory", [])?;
        Ok(rows)
    }

    // === Short-Term Memory Operations ===

    /// Save a session summary when the session ends
    pub fn save_session_summary(
        &self,
        session_id: &str,
        summary: &str,
        files_modified: &[String],
        issues_worked: &[String],
        started_at: &str,
    ) -> Result<i64> {
        let files_str = files_modified.join("\n");
        let issues_str = issues_worked.join("\n");

        self.conn.execute(
            r#"INSERT OR REPLACE INTO recent_sessions
               (session_id, summary, files_modified, issues_worked, started_at, ended_at)
               VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))"#,
            params![session_id, summary, files_str, issues_str, started_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get recent sessions (within expiry window)
    pub fn get_recent_sessions(&self, limit: usize) -> Result<Vec<RecentSession>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT id, session_id, summary, files_modified, issues_worked, started_at, ended_at
               FROM recent_sessions
               WHERE ended_at > datetime('now', ?1)
               ORDER BY ended_at DESC
               LIMIT ?2"#,
        )?;

        let expiry = format!("-{} hours", SHORT_TERM_EXPIRY_HOURS);
        let sessions = stmt
            .query_map(params![expiry, limit as i64], |row| {
                Ok(RecentSession {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    summary: row.get(2)?,
                    files_modified: row
                        .get::<_, String>(3)?
                        .lines()
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                    issues_worked: row
                        .get::<_, String>(4)?
                        .lines()
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                    started_at: row.get(5)?,
                    ended_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(sessions)
    }

    /// Log an activity (file read, file write, tool call, etc.)
    pub fn log_activity(
        &self,
        session_id: &str,
        activity_type: &str,
        target: &str,
        details: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO recent_activity (session_id, activity_type, target, details) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, activity_type, target, details],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get recent activities for a session
    pub fn get_session_activities(&self, session_id: &str) -> Result<Vec<RecentActivity>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT id, session_id, activity_type, target, details, created_at
               FROM recent_activity
               WHERE session_id = ?1
               ORDER BY created_at DESC"#,
        )?;

        let activities = stmt
            .query_map(params![session_id], |row| {
                Ok(RecentActivity {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    activity_type: row.get(2)?,
                    target: row.get(3)?,
                    details: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(activities)
    }

    /// Get unique files modified in a session
    pub fn get_session_files_modified(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT DISTINCT target FROM recent_activity
               WHERE session_id = ?1 AND activity_type IN ('file_write', 'file_edit')
               ORDER BY target"#,
        )?;

        let files = stmt
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;

        Ok(files)
    }

    /// Get unique issues worked on in a session
    pub fn get_session_issues(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT DISTINCT target FROM recent_activity
               WHERE session_id = ?1 AND activity_type IN ('issue_created', 'issue_closed', 'issue_comment')
               ORDER BY target"#,
        )?;

        let issues = stmt
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;

        Ok(issues)
    }

    /// Clean up expired short-term memory entries
    pub fn cleanup_expired_short_term(&self) -> Result<(usize, usize)> {
        let expiry = format!("-{} hours", SHORT_TERM_EXPIRY_HOURS);

        let sessions_deleted = self.conn.execute(
            "DELETE FROM recent_sessions WHERE ended_at < datetime('now', ?1)",
            params![expiry],
        )?;

        let activities_deleted = self.conn.execute(
            "DELETE FROM recent_activity WHERE created_at < datetime('now', ?1)",
            params![expiry],
        )?;

        Ok((sessions_deleted, activities_deleted))
    }

    /// Format recent sessions for injection into system prompt
    pub fn format_recent_context_for_prompt(&self) -> Result<String> {
        let sessions = self.get_recent_sessions(5)?;

        if sessions.is_empty() {
            return Ok(String::new());
        }

        let mut output = String::from("<recent_sessions>\n");
        output.push_str("The following sessions occurred recently. Use this context to maintain continuity:\n\n");

        for (i, session) in sessions.iter().enumerate() {
            output.push_str(&format!(
                "### Session {} (ended {})\n",
                i + 1,
                session.ended_at
            ));
            output.push_str(&session.summary);
            output.push('\n');

            if !session.files_modified.is_empty() {
                output.push_str("Files modified: ");
                output.push_str(&session.files_modified.join(", "));
                output.push('\n');
            }

            if !session.issues_worked.is_empty() {
                output.push_str("Issues worked: ");
                output.push_str(&session.issues_worked.join(", "));
                output.push('\n');
            }

            output.push('\n');
        }

        output.push_str("</recent_sessions>");
        Ok(output)
    }

    /// Reset everything including core memory and short-term memory
    pub fn reset_all(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            DELETE FROM archival_memory;
            DELETE FROM core_memory;
            DELETE FROM recent_sessions;
            DELETE FROM recent_activity;
            INSERT INTO core_memory (section, content) VALUES
                ('persona', 'I am an AI assistant helping with this project. I will learn about the codebase and remember important details across sessions.'),
                ('project_info', 'No project information recorded yet.'),
                ('user_preferences', 'No user preferences recorded yet.');
            "#,
        )?;
        Ok(())
    }
}

/// Memory statistics
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub count: usize,
    pub total_size: usize,
    pub last_updated: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_memory_db_creation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let _db = MemoryDb::open(&db_path).unwrap();
        assert!(db_path.exists());
    }

    #[test]
    fn test_memory_save_and_search() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        // Save a memory
        let id = db
            .memory_save(
                "The project uses Rust and tokio for async",
                &["rust".into(), "async".into()],
            )
            .unwrap();
        assert!(id > 0);

        // Search for it
        let results = db.memory_search("Rust", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
    }

    #[test]
    fn test_memory_update() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        let id = db.memory_save("Original content", &[]).unwrap();
        db.memory_update(id, "Updated content").unwrap();

        let mem = db.memory_get(id).unwrap().unwrap();
        assert_eq!(mem.content, "Updated content");
    }

    #[test]
    fn test_core_memory() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        // Check default core memory exists
        let core = db.get_core_memory().unwrap();
        assert_eq!(core.len(), 3);

        // Update persona
        db.update_core_memory("persona", "I am the OpenClaudia assistant")
            .unwrap();

        let persona = db.get_core_memory_section("persona").unwrap().unwrap();
        assert_eq!(persona.content, "I am the OpenClaudia assistant");
    }

    #[test]
    fn test_format_core_memory() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        let formatted = db.format_core_memory_for_prompt().unwrap();
        assert!(formatted.contains("<core_memory>"));
        assert!(formatted.contains("<persona>"));
        assert!(formatted.contains("</core_memory>"));
    }

    #[test]
    fn test_short_term_session_summary() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        // Save a session summary
        let id = db
            .save_session_summary(
                "session-123",
                "Fixed bug in authentication module",
                &["src/auth.rs".into(), "src/main.rs".into()],
                &["#42".into(), "#43".into()],
                "2024-01-01 10:00:00",
            )
            .unwrap();
        assert!(id > 0);

        // Retrieve recent sessions
        let sessions = db.get_recent_sessions(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-123");
        assert_eq!(sessions[0].files_modified.len(), 2);
        assert_eq!(sessions[0].issues_worked.len(), 2);
    }

    #[test]
    fn test_short_term_activity_logging() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        // Log some activities
        db.log_activity(
            "session-123",
            "file_write",
            "src/lib.rs",
            Some("Created new module"),
        )
        .unwrap();
        db.log_activity("session-123", "file_edit", "src/main.rs", None)
            .unwrap();
        db.log_activity(
            "session-123",
            "issue_created",
            "#100",
            Some("Add feature X"),
        )
        .unwrap();

        // Get activities
        let activities = db.get_session_activities("session-123").unwrap();
        assert_eq!(activities.len(), 3);

        // Get files modified
        let files = db.get_session_files_modified("session-123").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(files.contains(&"src/main.rs".to_string()));

        // Get issues
        let issues = db.get_session_issues("session-123").unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0], "#100");
    }

    #[test]
    fn test_format_recent_context() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();

        // Empty at first
        let empty = db.format_recent_context_for_prompt().unwrap();
        assert!(empty.is_empty());

        // Add a session
        db.save_session_summary(
            "session-1",
            "Implemented user login",
            &["src/auth.rs".into()],
            &["#50".into()],
            "2024-01-01 10:00:00",
        )
        .unwrap();

        let formatted = db.format_recent_context_for_prompt().unwrap();
        assert!(formatted.contains("<recent_sessions>"));
        assert!(formatted.contains("Implemented user login"));
        assert!(formatted.contains("src/auth.rs"));
        assert!(formatted.contains("#50"));
        assert!(formatted.contains("</recent_sessions>"));
    }
}
