//! Auto-learning memory module for `OpenClaudia`.
//!
//! Provides structured, automatic knowledge capture using `SQLite`.
//! Learns from tool execution signals, user corrections, and session patterns.
//! Each project gets its own memory database that persists across sessions.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Memory database file name
const MEMORY_DB_NAME: &str = "memory.db";

/// Current schema version - increment when adding migrations
const SCHEMA_VERSION: i64 = 3;

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

/// A coding pattern observed in the codebase
#[derive(Debug, Clone)]
pub struct CodingPattern {
    pub id: i64,
    pub file_glob: String,
    pub pattern_type: String, // "convention", "pitfall", "dependency", "architecture"
    pub description: String,
    pub confidence: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// An error pattern and its resolution
#[derive(Debug, Clone)]
pub struct ErrorPattern {
    pub id: i64,
    pub error_signature: String,
    pub file_context: Option<String>,
    pub resolution: Option<String>,
    pub occurrences: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A user preference learned from corrections or explicit statements
#[derive(Debug, Clone)]
pub struct LearnedPreference {
    pub id: i64,
    pub category: String, // "style", "workflow", "naming", "tool_usage", "correction"
    pub preference: String,
    pub source: Option<String>,
    pub confidence: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Memory database handle
pub struct MemoryDb {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl MemoryDb {
    /// Open or create memory database at the specified path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or schema migration fails.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open memory database at {}", path.display()))?;

        // Run schema migrations on the bare connection before wrapping in Mutex
        Self::ensure_schema_on(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    /// Open or create memory database in `.openclaudia` directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or the database cannot be opened.
    pub fn open_for_project(project_dir: &Path) -> Result<Self> {
        let openclaudia_dir = project_dir.join(".openclaudia");
        std::fs::create_dir_all(&openclaudia_dir).with_context(|| {
            format!(
                "Failed to create .openclaudia directory at {}",
                openclaudia_dir.display()
            )
        })?;

        let db_path = openclaudia_dir.join(MEMORY_DB_NAME);
        Self::open(&db_path)
    }

    /// Get the database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Execute a raw SQL statement (for maintenance operations like pruning).
    ///
    /// # Errors
    ///
    /// Returns an error if the SQL execution fails.
    pub fn execute_raw(&self, sql: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(sql)
            .with_context(|| format!("Failed to execute: {}", crate::tools::safe_truncate(sql, 100)))
    }

    /// Ensure database schema exists and run migrations (operates on bare `Connection`).
    fn ensure_schema_on(conn: &Connection) -> Result<()> {
        // Create version tracking table first
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
            [],
        )?;

        // Get current version (0 if table is empty = new db or pre-versioning db)
        let current_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Run migrations
        if current_version < SCHEMA_VERSION {
            tracing::info!(
                "Migrating memory database from version {current_version} to {SCHEMA_VERSION}",
            );
            Self::run_migrations_on(conn, current_version)?;
        }

        Ok(())
    }

    /// Run all migrations from `current_version` to `SCHEMA_VERSION` (operates on bare `Connection`).
    fn run_migrations_on(conn: &Connection, from_version: i64) -> Result<()> {
        // Version 1: Original schema (archival_memory, core_memory)
        if from_version < 1 {
            Self::migrate_v1_on(conn)?;
        }

        // Version 2: Add short-term memory tables
        if from_version < 2 {
            Self::migrate_v2_on(conn)?;
        }

        // Version 3: Add auto-learning tables
        if from_version < 3 {
            Self::migrate_v3_on(conn)?;
        }

        // Record current version
        conn.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;

        tracing::info!("Database migration complete. Now at version {SCHEMA_VERSION}");
        Ok(())
    }

    /// Migration v1: Original schema
    fn migrate_v1_on(conn: &Connection) -> Result<()> {
        tracing::debug!("Running migration v1: core schema");
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS archival_memory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                tags TEXT DEFAULT '',
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS archival_memory_fts USING fts5(
                content, tags, content=archival_memory, content_rowid=id
            );
            CREATE TRIGGER IF NOT EXISTS archival_memory_ai AFTER INSERT ON archival_memory BEGIN
                INSERT INTO archival_memory_fts(rowid, content, tags) VALUES (new.id, new.content, new.tags);
            END;
            CREATE TRIGGER IF NOT EXISTS archival_memory_ad AFTER DELETE ON archival_memory BEGIN
                INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content, tags) VALUES('delete', old.id, old.content, old.tags);
            END;
            CREATE TRIGGER IF NOT EXISTS archival_memory_au AFTER UPDATE ON archival_memory BEGIN
                INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content, tags) VALUES('delete', old.id, old.content, old.tags);
                INSERT INTO archival_memory_fts(rowid, content, tags) VALUES (new.id, new.content, new.tags);
            END;
            CREATE TABLE IF NOT EXISTS core_memory (
                section TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                updated_at TEXT DEFAULT (datetime('now'))
            );
            INSERT OR IGNORE INTO core_memory (section, content) VALUES
                ('persona', 'I am an AI assistant helping with this project. I will learn about the codebase and remember important details across sessions.'),
                ('project_info', 'No project information recorded yet.'),
                ('user_preferences', 'No user preferences recorded yet.');
            ",
        ).context("Failed to create v1 schema")?;

        Ok(())
    }

    /// Migration v2: Add short-term memory tables
    fn migrate_v2_on(conn: &Connection) -> Result<()> {
        tracing::debug!("Running migration v2: short-term memory tables");
        conn.execute_batch(
            r"
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
            ",
        )
        .context("Failed to create v2 schema (short-term memory)")?;

        Ok(())
    }

    /// Migration v3: Add auto-learning tables
    fn migrate_v3_on(conn: &Connection) -> Result<()> {
        tracing::debug!("Running migration v3: auto-learning tables");
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS coding_patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_glob TEXT NOT NULL,
                pattern_type TEXT NOT NULL,
                description TEXT NOT NULL,
                confidence INTEGER DEFAULT 1,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_coding_patterns_glob ON coding_patterns(file_glob);
            CREATE INDEX IF NOT EXISTS idx_coding_patterns_type ON coding_patterns(pattern_type);
            CREATE TABLE IF NOT EXISTS file_relationships (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_a TEXT NOT NULL,
                file_b TEXT NOT NULL,
                co_edit_count INTEGER DEFAULT 1,
                last_seen TEXT DEFAULT (datetime('now')),
                UNIQUE(file_a, file_b)
            );
            CREATE TABLE IF NOT EXISTS error_patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                error_signature TEXT NOT NULL,
                file_context TEXT,
                resolution TEXT,
                occurrences INTEGER DEFAULT 1,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_error_patterns_sig ON error_patterns(error_signature);
            CREATE TABLE IF NOT EXISTS learned_preferences (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                category TEXT NOT NULL,
                preference TEXT NOT NULL,
                source TEXT,
                confidence INTEGER DEFAULT 1,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );
            ",
        )
        .context("Failed to create v3 schema (auto-learning tables)")?;

        Ok(())
    }

    // === Archival Memory Operations ===

    /// Save a new memory entry.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn memory_save(&self, content: &str, tags: &[String]) -> Result<i64> {
        let tags_str = tags.join(",");
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO archival_memory (content, tags) VALUES (?1, ?2)",
            params![content, tags_str],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Search archival memory using full-text search.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the FTS query or database read fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn memory_search(&self, query: &str, limit: usize) -> Result<Vec<ArchivalMemory>> {
        let sanitized_query = format!("\"{}\"", query.replace('"', "\"\""));
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT am.id, am.content, am.tags, am.created_at, am.updated_at,
                   bm25(archival_memory_fts) as rank
            FROM archival_memory_fts
            JOIN archival_memory am ON archival_memory_fts.rowid = am.id
            WHERE archival_memory_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2",
        )?;

        let memories = stmt
            .query_map(params![sanitized_query, limit_i64], |row| {
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

    /// Get a memory by ID.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn memory_get(&self, id: i64) -> Result<Option<ArchivalMemory>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
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

    /// Update an existing memory.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn memory_update(&self, id: i64, content: &str) -> Result<bool> {
        let rows = self.conn.lock().unwrap().execute(
            "UPDATE archival_memory SET content = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![content, id],
        )?;
        Ok(rows > 0)
    }

    /// Delete a memory entry.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn memory_delete(&self, id: i64) -> Result<bool> {
        let rows = self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM archival_memory WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// List recent memories.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn memory_list(&self, limit: usize) -> Result<Vec<ArchivalMemory>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, content, tags, created_at, updated_at FROM archival_memory ORDER BY updated_at DESC LIMIT ?1",
        )?;

        let memories = stmt
            .query_map(params![limit_i64], |row| {
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

    /// Get memory statistics.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn memory_stats(&self) -> Result<MemoryStats> {
        let conn = self.conn.lock().unwrap();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM archival_memory", [], |row| row.get(0))?;
        let total_size: i64 = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(content)), 0) FROM archival_memory",
            [],
            |row| row.get(0),
        )?;
        let last_updated: Option<String> =
            conn.query_row("SELECT MAX(updated_at) FROM archival_memory", [], |row| {
                row.get(0)
            })?;
        drop(conn);

        Ok(MemoryStats {
            count: usize::try_from(count).unwrap_or(0),
            total_size: usize::try_from(total_size).unwrap_or(0),
            last_updated,
        })
    }

    // === Core Memory Operations ===

    /// Get all core memory sections.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_core_memory(&self) -> Result<Vec<CoreMemory>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT section, content, updated_at FROM core_memory ORDER BY section")?;

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

    /// Get a specific core memory section.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_core_memory_section(&self, section: &str) -> Result<Option<CoreMemory>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
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

    /// Update a core memory section.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database upsert fails.
    pub fn update_core_memory(&self, section: &str, content: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO core_memory (section, content, updated_at) VALUES (?1, ?2, datetime('now'))",
            params![section, content],
        )?;
        Ok(())
    }

    /// Format core memory for injection into system prompt.
    ///
    /// # Errors
    ///
    /// Returns an error if core memory cannot be read from the database.
    pub fn format_core_memory_for_prompt(&self) -> Result<String> {
        let core = self.get_core_memory()?;
        let mut output = String::from("<core_memory>\n");
        for mem in core {
            let _ = write!(
                output,
                "<{}>\n{}\n</{}>\n",
                mem.section, mem.content, mem.section
            );
        }
        output.push_str("</core_memory>");
        Ok(output)
    }

    /// Clear all archival memory (keeps core memory).
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn clear_archival_memory(&self) -> Result<usize> {
        let rows = self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM archival_memory", [])?;
        Ok(rows)
    }

    // === Short-Term Memory Operations ===

    /// Save a session summary when the session ends.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r"INSERT OR REPLACE INTO recent_sessions
               (session_id, summary, files_modified, issues_worked, started_at, ended_at)
               VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![session_id, summary, files_str, issues_str, started_at],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get recent sessions (within expiry window).
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_recent_sessions(&self, limit: usize) -> Result<Vec<RecentSession>> {
        let expiry = format!("-{SHORT_TERM_EXPIRY_HOURS} hours");
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT id, session_id, summary, files_modified, issues_worked, started_at, ended_at
               FROM recent_sessions
               WHERE ended_at > datetime('now', ?1)
               ORDER BY ended_at DESC LIMIT ?2",
        )?;

        let sessions = stmt
            .query_map(params![expiry, limit_i64], |row| {
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

    /// Log an activity (file read, file write, tool call, etc.).
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn log_activity(
        &self,
        session_id: &str,
        activity_type: &str,
        target: &str,
        details: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO recent_activity (session_id, activity_type, target, details) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, activity_type, target, details],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get recent activities for a session.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_session_activities(&self, session_id: &str) -> Result<Vec<RecentActivity>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT id, session_id, activity_type, target, details, created_at
               FROM recent_activity WHERE session_id = ?1 ORDER BY created_at DESC",
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

    /// Get unique files modified in a session.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_session_files_modified(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT DISTINCT target FROM recent_activity
               WHERE session_id = ?1 AND activity_type IN ('file_write', 'file_edit') ORDER BY target",
        )?;
        let files = stmt
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(files)
    }

    /// Get unique issues worked on in a session.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_session_issues(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT DISTINCT target FROM recent_activity
               WHERE session_id = ?1 AND activity_type IN ('issue_created', 'issue_closed', 'issue_comment') ORDER BY target",
        )?;
        let issues = stmt
            .query_map(params![session_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(issues)
    }

    /// Clean up expired short-term memory entries.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn cleanup_expired_short_term(&self) -> Result<(usize, usize)> {
        let expiry = format!("-{SHORT_TERM_EXPIRY_HOURS} hours");
        let conn = self.conn.lock().unwrap();
        let sessions_deleted = conn.execute(
            "DELETE FROM recent_sessions WHERE ended_at < datetime('now', ?1)",
            params![expiry],
        )?;
        let activities_deleted = conn.execute(
            "DELETE FROM recent_activity WHERE created_at < datetime('now', ?1)",
            params![expiry],
        )?;
        drop(conn);
        Ok((sessions_deleted, activities_deleted))
    }

    /// Format recent sessions for injection into system prompt.
    ///
    /// # Errors
    ///
    /// Returns an error if recent sessions cannot be read from the database.
    pub fn format_recent_context_for_prompt(&self) -> Result<String> {
        let sessions = self.get_recent_sessions(5)?;
        if sessions.is_empty() {
            return Ok(String::new());
        }

        let mut output = String::from("<recent_sessions>\nThe following sessions occurred recently. Use this context to maintain continuity:\n\n");
        for (i, session) in sessions.iter().enumerate() {
            let _ = writeln!(output, "### Session {} (ended {})", i + 1, session.ended_at);
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

    // === Auto-Learning: Coding Patterns ===

    /// Save a coding pattern for a file glob.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query or insert fails.
    pub fn save_coding_pattern(
        &self,
        file_glob: &str,
        pattern_type: &str,
        description: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM coding_patterns WHERE file_glob = ?1 AND pattern_type = ?2 AND description = ?3",
                params![file_glob, pattern_type, description], |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            conn.execute("UPDATE coding_patterns SET confidence = confidence + 1, updated_at = datetime('now') WHERE id = ?1", params![id])?;
            Ok(id)
        } else {
            conn.execute("INSERT INTO coding_patterns (file_glob, pattern_type, description) VALUES (?1, ?2, ?3)", params![file_glob, pattern_type, description])?;
            Ok(conn.last_insert_rowid())
        }
    }

    /// Get coding patterns matching a file path (checks against globs).
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_patterns_for_file(&self, file_path: &str) -> Result<Vec<CodingPattern>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, file_glob, pattern_type, description, confidence, created_at, updated_at FROM coding_patterns ORDER BY confidence DESC",
        )?;

        let all_patterns = stmt
            .query_map([], |row| {
                Ok(CodingPattern {
                    id: row.get(0)?,
                    file_glob: row.get(1)?,
                    pattern_type: row.get(2)?,
                    description: row.get(3)?,
                    confidence: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(all_patterns
            .into_iter()
            .filter(|p| glob_matches(&p.file_glob, file_path))
            .collect())
    }

    // === Auto-Learning: File Relationships ===

    /// Record that two files were edited together (upsert).
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database upsert fails.
    pub fn save_file_relationship(&self, file_a: &str, file_b: &str) -> Result<()> {
        let (fa, fb) = if file_a <= file_b {
            (file_a, file_b)
        } else {
            (file_b, file_a)
        };
        self.conn.lock().unwrap().execute(
            r"INSERT INTO file_relationships (file_a, file_b) VALUES (?1, ?2)
               ON CONFLICT(file_a, file_b) DO UPDATE SET co_edit_count = co_edit_count + 1, last_seen = datetime('now')",
            params![fa, fb],
        )?;
        Ok(())
    }

    /// Get files frequently co-edited with the given file.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_related_files(&self, file_path: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT CASE WHEN file_a = ?1 THEN file_b ELSE file_a END as related, co_edit_count
               FROM file_relationships WHERE file_a = ?1 OR file_b = ?1
               ORDER BY co_edit_count DESC LIMIT 10",
        )?;
        let results = stmt
            .query_map(params![file_path], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    // === Auto-Learning: Error Patterns ===

    /// Save or update an error pattern.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query or upsert fails.
    pub fn save_error_pattern(
        &self,
        error_signature: &str,
        file_context: Option<&str>,
        resolution: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<(i64, Option<String>)> = conn
            .query_row(
                "SELECT id, resolution FROM error_patterns WHERE error_signature = ?1 AND (file_context = ?2 OR (?2 IS NULL AND file_context IS NULL))",
                params![error_signature, file_context], |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((id, existing_resolution)) = existing {
            if resolution.is_some() && existing_resolution.is_none() {
                conn.execute("UPDATE error_patterns SET occurrences = occurrences + 1, resolution = ?1, updated_at = datetime('now') WHERE id = ?2", params![resolution, id])?;
            } else {
                conn.execute("UPDATE error_patterns SET occurrences = occurrences + 1, updated_at = datetime('now') WHERE id = ?1", params![id])?;
            }
            Ok(id)
        } else {
            conn.execute("INSERT INTO error_patterns (error_signature, file_context, resolution) VALUES (?1, ?2, ?3)", params![error_signature, file_context, resolution])?;
            Ok(conn.last_insert_rowid())
        }
    }

    /// Get error patterns for a specific file.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_error_patterns_for_file(&self, file_path: &str) -> Result<Vec<ErrorPattern>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r"SELECT id, error_signature, file_context, resolution, occurrences, created_at, updated_at
               FROM error_patterns WHERE file_context = ?1 ORDER BY occurrences DESC LIMIT 10",
        )?;

        let patterns = stmt
            .query_map(params![file_path], |row| {
                Ok(ErrorPattern {
                    id: row.get(0)?,
                    error_signature: row.get(1)?,
                    file_context: row.get(2)?,
                    resolution: row.get(3)?,
                    occurrences: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(patterns)
    }

    /// Update the resolution for an existing error pattern.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn resolve_error_pattern(
        &self,
        error_signature: &str,
        file_context: Option<&str>,
        resolution: &str,
    ) -> Result<bool> {
        let rows = self.conn.lock().unwrap().execute(
            "UPDATE error_patterns SET resolution = ?1, updated_at = datetime('now') WHERE error_signature = ?2 AND (file_context = ?3 OR (?3 IS NULL AND file_context IS NULL)) AND resolution IS NULL",
            params![resolution, error_signature, file_context],
        )?;
        Ok(rows > 0)
    }

    // === Auto-Learning: Learned Preferences ===

    /// Save a learned user preference.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query or insert fails.
    pub fn save_learned_preference(
        &self,
        category: &str,
        preference: &str,
        source: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM learned_preferences WHERE category = ?1 AND preference = ?2",
                params![category, preference],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            conn.execute("UPDATE learned_preferences SET confidence = confidence + 1, updated_at = datetime('now') WHERE id = ?1", params![id])?;
            Ok(id)
        } else {
            conn.execute("INSERT INTO learned_preferences (category, preference, source) VALUES (?1, ?2, ?3)", params![category, preference, source])?;
            Ok(conn.last_insert_rowid())
        }
    }

    /// Get all learned preferences.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_all_preferences(&self) -> Result<Vec<LearnedPreference>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, category, preference, source, confidence, created_at, updated_at FROM learned_preferences ORDER BY confidence DESC",
        )?;

        let prefs = stmt
            .query_map([], |row| {
                Ok(LearnedPreference {
                    id: row.get(0)?,
                    category: row.get(1)?,
                    preference: row.get(2)?,
                    source: row.get(3)?,
                    confidence: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(prefs)
    }

    // === Formatting for Context Injection ===

    /// Format knowledge about a specific file for context injection.
    ///
    /// # Errors
    ///
    /// Returns an error if patterns, errors, or related files cannot be read from the database.
    pub fn format_file_knowledge(&self, file_path: &str) -> Result<String> {
        let patterns = self.get_patterns_for_file(file_path)?;
        let errors = self.get_error_patterns_for_file(file_path)?;
        let related = self.get_related_files(file_path)?;

        if patterns.is_empty() && errors.is_empty() && related.is_empty() {
            return Ok(String::new());
        }

        let mut output = format!("<file_knowledge path=\"{file_path}\">\n");
        if !patterns.is_empty() {
            output.push_str("Patterns:\n");
            for p in patterns.iter().take(5) {
                let _ = writeln!(
                    output,
                    "- [{}] {} (seen {}x)",
                    p.pattern_type, p.description, p.confidence
                );
            }
        }
        if !errors.is_empty() {
            output.push_str("Known issues:\n");
            for e in errors.iter().take(5) {
                let _ = write!(output, "- {} ({}x)", e.error_signature, e.occurrences);
                if let Some(ref res) = e.resolution {
                    let _ = write!(output, " \u{2192} fix: {res}");
                }
                output.push('\n');
            }
        }
        if !related.is_empty() {
            let related_str: Vec<String> = related
                .iter()
                .take(5)
                .map(|(f, count)| format!("{f} ({count}x)"))
                .collect();
            let _ = writeln!(output, "Often co-edited with: {}", related_str.join(", "));
        }
        output.push_str("</file_knowledge>");
        Ok(output)
    }

    /// Format learned preferences for system prompt injection.
    ///
    /// # Errors
    ///
    /// Returns an error if preferences cannot be read from the database.
    pub fn format_learned_preferences(&self) -> Result<String> {
        let prefs = self.get_all_preferences()?;
        if prefs.is_empty() {
            return Ok(String::new());
        }

        let mut output = String::from("<learned_preferences>\n");
        for p in prefs.iter().take(15) {
            let _ = writeln!(
                output,
                "- [{}] {} (confidence: {})",
                p.category, p.preference, p.confidence
            );
        }
        output.push_str("</learned_preferences>");
        Ok(output)
    }

    /// Get auto-learning statistics.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database queries fail.
    pub fn auto_learn_stats(&self) -> Result<AutoLearnStats> {
        let conn = self.conn.lock().unwrap();
        let patterns: i64 =
            conn.query_row("SELECT COUNT(*) FROM coding_patterns", [], |row| row.get(0))?;
        let relationships: i64 =
            conn.query_row("SELECT COUNT(*) FROM file_relationships", [], |row| {
                row.get(0)
            })?;
        let errors: i64 =
            conn.query_row("SELECT COUNT(*) FROM error_patterns", [], |row| row.get(0))?;
        let resolved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM error_patterns WHERE resolution IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        let preferences: i64 =
            conn.query_row("SELECT COUNT(*) FROM learned_preferences", [], |row| {
                row.get(0)
            })?;
        drop(conn);

        Ok(AutoLearnStats {
            coding_patterns: usize::try_from(patterns).unwrap_or(0),
            file_relationships: usize::try_from(relationships).unwrap_or(0),
            error_patterns: usize::try_from(errors).unwrap_or(0),
            errors_resolved: usize::try_from(resolved).unwrap_or(0),
            learned_preferences: usize::try_from(preferences).unwrap_or(0),
        })
    }

    /// Reset everything including core memory, short-term memory, and auto-learning data.
    ///
    /// # Panics
    ///
    /// Panics if the internal database mutex is poisoned.
    ///
    /// # Errors
    ///
    /// Returns an error if the database batch execution fails.
    pub fn reset_all(&self) -> Result<()> {
        self.conn.lock().unwrap().execute_batch(
            r"
            DELETE FROM archival_memory;
            DELETE FROM core_memory;
            DELETE FROM recent_sessions;
            DELETE FROM recent_activity;
            DELETE FROM coding_patterns;
            DELETE FROM file_relationships;
            DELETE FROM error_patterns;
            DELETE FROM learned_preferences;
            INSERT INTO core_memory (section, content) VALUES
                ('persona', 'I am an AI assistant helping with this project. I will learn about the codebase and remember important details across sessions.'),
                ('project_info', 'No project information recorded yet.'),
                ('user_preferences', 'No user preferences recorded yet.');
            ",
        )?;
        Ok(())
    }
}

/// Simple glob matching (supports `*` and exact match).
#[must_use]
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return path.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return path.ends_with(suffix);
    }
    if let Some(star_pos) = pattern.find('*') {
        let prefix = &pattern[..star_pos];
        let suffix = &pattern[star_pos + 1..];
        return path.starts_with(prefix) && path.ends_with(suffix);
    }
    false
}

/// Memory statistics
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub count: usize,
    pub total_size: usize,
    pub last_updated: Option<String>,
}

/// Auto-learning statistics
#[derive(Debug, Clone)]
pub struct AutoLearnStats {
    pub coding_patterns: usize,
    pub file_relationships: usize,
    pub error_patterns: usize,
    pub errors_resolved: usize,
    pub learned_preferences: usize,
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
        let id = db
            .memory_save(
                "The project uses Rust and tokio for async",
                &["rust".into(), "async".into()],
            )
            .unwrap();
        assert!(id > 0);
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
        let core = db.get_core_memory().unwrap();
        assert_eq!(core.len(), 3);
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
        let activities = db.get_session_activities("session-123").unwrap();
        assert_eq!(activities.len(), 3);
        let files = db.get_session_files_modified("session-123").unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(files.contains(&"src/main.rs".to_string()));
        let issues = db.get_session_issues("session-123").unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0], "#100");
    }

    #[test]
    fn test_format_recent_context() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = MemoryDb::open(&db_path).unwrap();
        let empty = db.format_recent_context_for_prompt().unwrap();
        assert!(empty.is_empty());
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
