//! Auto-learning memory module for `OpenClaudia`.
//!
//! Provides structured, automatic knowledge capture using `SQLite`.
//! Learns from tool execution signals, user corrections, and session patterns.
//! Each project gets its own memory database that persists across sessions.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Memory database file name
const MEMORY_DB_NAME: &str = "memory.db";

/// Current schema version - increment when adding migrations
const SCHEMA_VERSION: i64 = 4;

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

/// Escape a user-supplied string into a single FTS5 phrase literal.
///
/// The `SQLite` FTS5 MATCH grammar treats words as operators (`AND`, `OR`,
/// `NOT`, `NEAR`), supports column filters (`colname:token`), prefix
/// matching with `*`, parentheses, and bare double-quotes for phrase
/// expressions. Wrapping the entire query in a double-quoted phrase and
/// doubling interior double-quotes neutralizes all of those: FTS5 parses
/// the result as one opaque literal phrase.
///
/// Also strips ASCII control characters that FTS5's tokenizer would
/// choke on or that could produce surprising matches. See crosslink #444.
fn escape_fts5_phrase(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_ascii_control()).collect();
    let inner = cleaned.replace('"', "\"\"");
    format!("\"{inner}\"")
}

/// Tables that the auto-learning subsystem may prune.
///
/// Using an enum allowlist prevents callers from interpolating arbitrary
/// table names into SQL (the `SQLi` pattern flagged in crosslink `#255`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoLearnTable {
    /// Short-horizon code-style observations.
    CodingPatterns,
    /// Recorded tool-failure signatures.
    ErrorPatterns,
    /// Inferred user preferences.
    LearnedPreferences,
    /// Co-edit co-occurrence pairs.
    FileRelationships,
}

/// Memory database handle
pub struct MemoryDb {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl MemoryDb {
    /// Acquire the connection lock, converting mutex-poison into an `anyhow` error.
    ///
    /// Callers **must** release the guard before any `.await` point.
    /// All public synchronous methods use this helper so that a panicking
    /// worker thread does not propagate an unwind into the caller.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the internal mutex is poisoned (a previous holder panicked).
    fn lock_conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("memory mutex poisoned"))
    }

    /// Open or create memory database at the specified path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or schema migration fails.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open memory database at {}", path.display()))?;

        // Enable foreign-key enforcement (off by default in SQLite).  The v4
        // migration introduces `archival_memory_tags` with an `ON DELETE
        // CASCADE` reference to `archival_memory(id)` — without this PRAGMA
        // the cascade is silently a no-op and orphaned tag rows accumulate.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("Failed to enable SQLite foreign-key enforcement")?;

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

    /// Execute a raw SQL statement (crate-internal: for test fixtures only).
    ///
    /// External callers must use typed methods such as [`prune_auto_learn_tables`]
    /// to avoid SQL-injection at the call-site.
    ///
    /// # Errors
    ///
    /// Returns an error if the SQL execution fails or the mutex is poisoned.
    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute_batch(sql).with_context(|| {
            format!(
                "Failed to execute: {}",
                crate::tools::safe_truncate(sql, 100)
            )
        })
    }

    /// Prune one auto-learning table, retaining the `keep` most-recent rows.
    ///
    /// Table names come from the [`AutoLearnTable`] enum allowlist so no
    /// caller-controlled string can reach the SQL statement.  The row count
    /// is bound as a query parameter — never interpolated.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `DELETE` fails or the mutex is poisoned.
    pub fn prune_auto_learn_table(&self, table: AutoLearnTable, keep: u32) -> Result<()> {
        let conn = self.lock_conn()?;
        // SQLite does not support binding table names as parameters, so we
        // resolve the name through an exhaustive match — the compiler will
        // catch any future enum variant that has no corresponding arm.
        let stmt = match table {
            AutoLearnTable::CodingPatterns => {
                "DELETE FROM coding_patterns \
                 WHERE rowid NOT IN \
                 (SELECT rowid FROM coding_patterns ORDER BY rowid DESC LIMIT ?1)"
            }
            AutoLearnTable::ErrorPatterns => {
                "DELETE FROM error_patterns \
                 WHERE rowid NOT IN \
                 (SELECT rowid FROM error_patterns ORDER BY rowid DESC LIMIT ?1)"
            }
            AutoLearnTable::LearnedPreferences => {
                "DELETE FROM learned_preferences \
                 WHERE rowid NOT IN \
                 (SELECT rowid FROM learned_preferences ORDER BY rowid DESC LIMIT ?1)"
            }
            AutoLearnTable::FileRelationships => {
                "DELETE FROM file_relationships \
                 WHERE rowid NOT IN \
                 (SELECT rowid FROM file_relationships ORDER BY rowid DESC LIMIT ?1)"
            }
        };
        conn.execute(stmt, params![keep])
            .with_context(|| format!("Failed to prune auto-learn table {table:?}"))?;
        drop(conn);
        Ok(())
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

        // Version 4: Normalise archival-memory tags into a junction table
        // (crosslink #464).  The previous schema stored `tags` as a single
        // comma-joined `TEXT` column on `archival_memory`, which lost data
        // round-trip when a tag itself contained a comma and forced every
        // query to use substring `LIKE` semantics that produced false
        // matches.  Moving tags into `archival_memory_tags(memory_id, tag)`
        // restores 1NF, gives O(log n) tag look-up via the natural index,
        // and makes the comma no longer special.
        if from_version < 4 {
            Self::migrate_v4_on(conn)?;
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

    /// Migration v4: Normalise `archival_memory.tags` into a junction table.
    ///
    /// Forensic context (crosslink #464):
    /// * `archival_memory.tags` was a comma-joined `TEXT` column written by
    ///   `tags.join(",")` and read with `String::split(',')`.  A tag that
    ///   contained a literal comma — e.g. `"rust, tokio"` — was silently
    ///   broken into two fake tags on read; there was no escaping.
    /// * No index existed on `tags`, so every "memories tagged X" query
    ///   was a full-table `LIKE '%X%'` scan that also matched substrings
    ///   (`%rust%` matched `rustaceans`).
    /// * The FTS5 virtual table indexed the tag blob as one giant token,
    ///   so FTS searches on tags were equally unreliable.
    ///
    /// Migration plan, executed inside a `SAVEPOINT` so partial failure
    /// rolls back cleanly (same pattern as the #400 reset-all fix):
    /// 1. Create `archival_memory_tags(memory_id, tag)` with a CASCADE FK
    ///    onto `archival_memory(id)` and an index on `tag`.
    /// 2. Back-fill from the legacy column — `split(',')`, trim, dedupe per
    ///    row, skip empty fragments.
    /// 3. Rebuild the FTS5 virtual table so it indexes only `content`; the
    ///    tags filter is a separate JOIN going forward.
    /// 4. Drop the legacy `tags` column from `archival_memory`.
    fn migrate_v4_on(conn: &Connection) -> Result<()> {
        tracing::debug!("Running migration v4: archival_memory_tags junction table (#464)");

        // `Connection::execute_batch` does not wrap its statements in a
        // transaction, so we use an explicit SAVEPOINT to get rollback
        // semantics across the multi-step migration.  Inner work is
        // factored out so the savepoint guard stays short and the
        // function body stays inside clippy's 100-line ceiling.
        conn.execute_batch("SAVEPOINT migrate_v4;")
            .context("v4: failed to begin SAVEPOINT")?;

        match Self::migrate_v4_inner(conn) {
            Ok(()) => {
                conn.execute_batch("RELEASE SAVEPOINT migrate_v4;")
                    .context("v4: failed to RELEASE SAVEPOINT")?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK TO SAVEPOINT migrate_v4;");
                let _ = conn.execute_batch("RELEASE SAVEPOINT migrate_v4;");
                Err(e)
            }
        }
    }

    /// Inner body of [`migrate_v4_on`] — runs steps 1-4 of the schema
    /// migration so the savepoint wrapper stays under the per-function
    /// line ceiling.
    fn migrate_v4_inner(conn: &Connection) -> Result<()> {
        // Step 1: junction table + index.
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS archival_memory_tags (
                memory_id INTEGER NOT NULL
                    REFERENCES archival_memory(id) ON DELETE CASCADE,
                tag TEXT NOT NULL,
                PRIMARY KEY (memory_id, tag)
            );
            CREATE INDEX IF NOT EXISTS idx_archival_memory_tags_tag
                ON archival_memory_tags(tag);
            ",
        )
        .context("v4: failed to create archival_memory_tags table")?;

        // Step 2: back-fill from the legacy comma-joined column.  The
        // legacy column may or may not exist depending on how the db
        // was built; check first so a fresh db (which already lacks the
        // column post-v4) doesn't error.
        let has_legacy_tags_col = Self::archival_memory_has_legacy_tags(conn)?;
        if has_legacy_tags_col {
            Self::backfill_legacy_tags(conn)?;
        }

        // Step 3: rebuild the FTS5 virtual table without the `tags`
        // column.  The accompanying triggers must be dropped first
        // because they reference the old virtual-table schema.
        conn.execute_batch(
            r"
            DROP TRIGGER IF EXISTS archival_memory_ai;
            DROP TRIGGER IF EXISTS archival_memory_ad;
            DROP TRIGGER IF EXISTS archival_memory_au;
            DROP TABLE  IF EXISTS archival_memory_fts;

            CREATE VIRTUAL TABLE archival_memory_fts USING fts5(
                content, content=archival_memory, content_rowid=id
            );
            INSERT INTO archival_memory_fts(rowid, content)
                SELECT id, content FROM archival_memory;

            CREATE TRIGGER archival_memory_ai
                AFTER INSERT ON archival_memory BEGIN
                    INSERT INTO archival_memory_fts(rowid, content)
                        VALUES (new.id, new.content);
                END;
            CREATE TRIGGER archival_memory_ad
                AFTER DELETE ON archival_memory BEGIN
                    INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content)
                        VALUES ('delete', old.id, old.content);
                END;
            CREATE TRIGGER archival_memory_au
                AFTER UPDATE ON archival_memory BEGIN
                    INSERT INTO archival_memory_fts(archival_memory_fts, rowid, content)
                        VALUES ('delete', old.id, old.content);
                    INSERT INTO archival_memory_fts(rowid, content)
                        VALUES (new.id, new.content);
                END;
            ",
        )
        .context("v4: failed to rebuild FTS5 virtual table without tags column")?;

        // Step 4: drop the now-redundant legacy `tags` column.  SQLite
        // 3.35+ supports `ALTER TABLE ... DROP COLUMN`; the bundled
        // build in `rusqlite` 0.38 ships a newer SQLite than that.
        if has_legacy_tags_col {
            conn.execute_batch("ALTER TABLE archival_memory DROP COLUMN tags;")
                .context("v4: failed to drop legacy archival_memory.tags column")?;
        }

        Ok(())
    }

    /// Returns `true` when `archival_memory` still carries the legacy
    /// pre-v4 comma-joined `tags` column.
    fn archival_memory_has_legacy_tags(conn: &Connection) -> Result<bool> {
        let found = conn
            .prepare("PRAGMA table_info(archival_memory)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "tags");
        Ok(found)
    }

    /// Read the legacy comma-joined `tags` column and write each split
    /// fragment into `archival_memory_tags`.  Tags are trimmed; empty
    /// fragments are skipped; `INSERT OR IGNORE` collapses duplicates.
    fn backfill_legacy_tags(conn: &Connection) -> Result<()> {
        let mut select = conn
            .prepare(
                "SELECT id, tags FROM archival_memory \
                 WHERE tags IS NOT NULL AND tags != ''",
            )
            .context("v4: failed to prepare legacy tag read")?;
        let rows: Vec<(i64, String)> = select
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("v4: failed to read legacy tag rows")?;
        drop(select);

        let mut insert = conn
            .prepare(
                "INSERT OR IGNORE INTO archival_memory_tags (memory_id, tag) \
                 VALUES (?1, ?2)",
            )
            .context("v4: failed to prepare tag insert")?;
        for (memory_id, joined) in rows {
            for raw in joined.split(',') {
                let tag = raw.trim();
                if tag.is_empty() {
                    continue;
                }
                insert
                    .execute(params![memory_id, tag])
                    .context("v4: failed to back-fill tag row")?;
            }
        }
        Ok(())
    }

    // === Archival Memory Operations ===

    /// Save a new memory entry.
    ///
    /// The content row and any number of tag rows are inserted together
    /// inside a single SQL transaction so a partial write — say a tag
    /// insertion failing on disk-full — leaves the database with neither
    /// the content row nor any of its tags.  Tags are stored as separate
    /// rows in `archival_memory_tags(memory_id, tag)`; an empty `tags`
    /// slice writes only the content row.  Duplicate tags within a single
    /// call are coalesced by the table's `PRIMARY KEY(memory_id, tag)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails or the mutex is poisoned.
    pub fn memory_save(&self, content: &str, tags: &[String]) -> Result<i64> {
        // Delegate to a `&mut Connection` helper so the mutex guard is
        // dropped on return — keeps clippy::significant_drop_tightening
        // satisfied without a per-call `#[allow]` annotation.
        Self::memory_save_on(&mut *self.lock_conn()?, content, tags)
    }

    /// Inner save helper: insert the content row and any tag rows in a
    /// single transaction.  Extracted so the mutex guard in
    /// [`memory_save`] has no lifetime overlap with the returned `Ok(id)`.
    fn memory_save_on(conn: &mut Connection, content: &str, tags: &[String]) -> Result<i64> {
        let tx = conn
            .transaction()
            .context("memory_save: failed to begin transaction")?;

        tx.execute(
            "INSERT INTO archival_memory (content) VALUES (?1)",
            params![content],
        )
        .context("memory_save: archival_memory INSERT failed")?;
        let id = tx.last_insert_rowid();

        if !tags.is_empty() {
            let mut stmt = tx
                .prepare(
                    "INSERT OR IGNORE INTO archival_memory_tags (memory_id, tag) \
                     VALUES (?1, ?2)",
                )
                .context("memory_save: failed to prepare tag insert")?;
            for tag in tags {
                if tag.is_empty() {
                    continue;
                }
                stmt.execute(params![id, tag])
                    .context("memory_save: tag insert failed")?;
            }
        }

        tx.commit().context("memory_save: commit failed")?;
        Ok(id)
    }

    /// Load all tags for a given memory id (sorted for deterministic output).
    ///
    /// Returns an empty vector if the memory has no tags or does not exist.
    /// Called from every read-path so the public `ArchivalMemory` value
    /// always carries the live tag set, never a stale comma-joined string.
    fn load_tags_for(conn: &Connection, memory_id: i64) -> rusqlite::Result<Vec<String>> {
        let mut stmt =
            conn.prepare("SELECT tag FROM archival_memory_tags WHERE memory_id = ?1 ORDER BY tag")?;
        let tags = stmt
            .query_map(params![memory_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tags)
    }

    /// Search archival memory using full-text search.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the FTS query or database read fails, or the mutex is poisoned.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn memory_search(&self, query: &str, limit: usize) -> Result<Vec<ArchivalMemory>> {
        let phrase_query = escape_fts5_phrase(query);
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            r"SELECT am.id, am.content, am.created_at, am.updated_at,
                   bm25(archival_memory_fts) as rank
            FROM archival_memory_fts
            JOIN archival_memory am ON archival_memory_fts.rowid = am.id
            WHERE archival_memory_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2",
        )?;

        // Collect rows without tags first; then hydrate tags via a per-row
        // look-up so the FTS query plan stays simple and we don't have to
        // wrestle with GROUP_CONCAT (which would re-introduce a fragile
        // string join).  N+1 here is bounded by `limit`, which the caller
        // already chose; for the typical limit of ≤ 100 this is fine.
        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map(params![phrase_query, limit_i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut memories = Vec::with_capacity(rows.len());
        for (id, content, created_at, updated_at) in rows {
            let tags = Self::load_tags_for(&conn, id)?;
            memories.push(ArchivalMemory {
                id,
                content,
                tags,
                created_at,
                updated_at,
            });
        }

        Ok(memories)
    }

    /// Return every archival memory tagged with `tag` (exact match).
    ///
    /// This is the query path the FTS-on-tags approach broke: a literal
    /// `tag` lookup is a single equality on the `idx_archival_memory_tags_tag`
    /// index, so it is O(log n) and never matches substrings.  Comma is
    /// not special — `tag` is compared character-for-character.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails or the mutex is poisoned.
    pub fn memory_search_by_tag(&self, tag: &str, limit: usize) -> Result<Vec<ArchivalMemory>> {
        // Same delegate pattern as `memory_save` / `reset_all`: do the
        // work in a free helper that takes `&Connection` so the mutex
        // guard is dropped on return — keeps clippy's
        // `significant_drop_tightening` lint satisfied without a
        // per-call `#[allow]` annotation.
        Self::memory_search_by_tag_on(&*self.lock_conn()?, tag, limit)
    }

    /// Inner search helper: read rows tagged with `tag` and hydrate each
    /// with its full tag set.  Extracted so the mutex guard in
    /// [`memory_search_by_tag`] has no lifetime overlap with the returned
    /// `Vec`.
    fn memory_search_by_tag_on(
        conn: &Connection,
        tag: &str,
        limit: usize,
    ) -> Result<Vec<ArchivalMemory>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            r"SELECT am.id, am.content, am.created_at, am.updated_at
            FROM archival_memory am
            JOIN archival_memory_tags amt ON amt.memory_id = am.id
            WHERE amt.tag = ?1
            ORDER BY am.updated_at DESC
            LIMIT ?2",
        )?;

        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map(params![tag, limit_i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut memories = Vec::with_capacity(rows.len());
        for (id, content, created_at, updated_at) in rows {
            let tags = Self::load_tags_for(conn, id)?;
            memories.push(ArchivalMemory {
                id,
                content,
                tags,
                created_at,
                updated_at,
            });
        }

        Ok(memories)
    }

    /// Get a memory by ID.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails or the mutex is poisoned.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn memory_get(&self, id: i64) -> Result<Option<ArchivalMemory>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, content, created_at, updated_at FROM archival_memory WHERE id = ?1",
        )?;

        let core = stmt
            .query_row(params![id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .optional()?;

        let memory = match core {
            Some((row_id, content, created_at, updated_at)) => {
                let tags = Self::load_tags_for(&conn, row_id)?;
                Some(ArchivalMemory {
                    id: row_id,
                    content,
                    tags,
                    created_at,
                    updated_at,
                })
            }
            None => None,
        };

        Ok(memory)
    }

    /// Update an existing memory.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn memory_update(&self, id: i64, content: &str) -> Result<bool> {
        let rows = self.lock_conn()?.execute(
            "UPDATE archival_memory SET content = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![content, id],
        )?;
        Ok(rows > 0)
    }

    /// Delete a memory entry.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn memory_delete(&self, id: i64) -> Result<bool> {
        let rows = self
            .lock_conn()?
            .execute("DELETE FROM archival_memory WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// List recent memories.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn memory_list(&self, limit: usize) -> Result<Vec<ArchivalMemory>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, content, created_at, updated_at FROM archival_memory \
             ORDER BY updated_at DESC LIMIT ?1",
        )?;

        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map(params![limit_i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut memories = Vec::with_capacity(rows.len());
        for (id, content, created_at, updated_at) in rows {
            let tags = Self::load_tags_for(&conn, id)?;
            memories.push(ArchivalMemory {
                id,
                content,
                tags,
                created_at,
                updated_at,
            });
        }

        Ok(memories)
    }

    /// Get memory statistics.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn memory_stats(&self) -> Result<MemoryStats> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_core_memory(&self) -> Result<Vec<CoreMemory>> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_core_memory_section(&self, section: &str) -> Result<Option<CoreMemory>> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database upsert fails.
    pub fn update_core_memory(&self, section: &str, content: &str) -> Result<()> {
        self.lock_conn()?.execute(
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn clear_archival_memory(&self) -> Result<usize> {
        let rows = self
            .lock_conn()?
            .execute("DELETE FROM archival_memory", [])?;
        Ok(rows)
    }

    // === Short-Term Memory Operations ===

    /// Save a session summary when the session ends.
    ///
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
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_recent_sessions(&self, limit: usize) -> Result<Vec<RecentSession>> {
        let expiry = format!("-{SHORT_TERM_EXPIRY_HOURS} hours");
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        let conn = self.lock_conn()?;
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
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO recent_activity (session_id, activity_type, target, details) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, activity_type, target, details],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get recent activities for a session.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_session_activities(&self, session_id: &str) -> Result<Vec<RecentActivity>> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_session_files_modified(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_session_issues(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn cleanup_expired_short_term(&self) -> Result<(usize, usize)> {
        let expiry = format!("-{SHORT_TERM_EXPIRY_HOURS} hours");
        let conn = self.lock_conn()?;
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
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_patterns_for_file(&self, file_path: &str) -> Result<Vec<CodingPattern>> {
        let conn = self.lock_conn()?;
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
        self.lock_conn()?.execute(
            r"INSERT INTO file_relationships (file_a, file_b) VALUES (?1, ?2)
               ON CONFLICT(file_a, file_b) DO UPDATE SET co_edit_count = co_edit_count + 1, last_seen = datetime('now')",
            params![fa, fb],
        )?;
        Ok(())
    }

    /// Get files frequently co-edited with the given file.
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_related_files(&self, file_path: &str) -> Result<Vec<(String, i64)>> {
        let conn = self.lock_conn()?;
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
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_error_patterns_for_file(&self, file_path: &str) -> Result<Vec<ErrorPattern>> {
        let conn = self.lock_conn()?;
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
        let rows = self.lock_conn()?.execute(
            "UPDATE error_patterns SET resolution = ?1, updated_at = datetime('now') WHERE error_signature = ?2 AND (file_context = ?3 OR (?3 IS NULL AND file_context IS NULL)) AND resolution IS NULL",
            params![resolution, error_signature, file_context],
        )?;
        Ok(rows > 0)
    }

    // === Auto-Learning: Learned Preferences ===

    /// Save a learned user preference.
    ///
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
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::significant_drop_tightening)] // conn must outlive stmt which borrows it
    pub fn get_all_preferences(&self) -> Result<Vec<LearnedPreference>> {
        let conn = self.lock_conn()?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the database queries fail.
    pub fn auto_learn_stats(&self) -> Result<AutoLearnStats> {
        let conn = self.lock_conn()?;
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

    /// Canonical seed SQL used by [`Self::reset_all`] to restore the default
    /// core-memory rows after a wipe.
    ///
    /// Extracted as a constant so the regression test for crosslink #400 can
    /// substitute a deliberately failing seed and verify transactional
    /// rollback without duplicating production seed text.
    const DEFAULT_RESET_SEED_SQL: &'static str = r"
        INSERT INTO core_memory (section, content) VALUES
            ('persona', 'I am an AI assistant helping with this project. I will learn about the codebase and remember important details across sessions.'),
            ('project_info', 'No project information recorded yet.'),
            ('user_preferences', 'No user preferences recorded yet.');
    ";

    /// Reset everything including core memory, short-term memory, and
    /// auto-learning data.
    ///
    /// The DELETEs across every table and the re-seed of `core_memory`
    /// execute inside a single SQL transaction (crosslink #400). If the
    /// reseed step fails — for example, because a constraint is violated —
    /// the entire reset is rolled back so the database is never left
    /// without `persona` / `project_info` / `user_preferences` rows.
    ///
    /// # Errors
    ///
    /// Returns an error if any DELETE or INSERT in the transaction fails.
    /// On error the transaction is rolled back automatically when the
    /// `rusqlite::Transaction` guard is dropped, leaving the previous state
    /// intact.
    pub fn reset_all(&self) -> Result<()> {
        self.reset_all_with_seed_sql(Self::DEFAULT_RESET_SEED_SQL)
    }

    /// Transactional reset+reseed core, parameterised on the seed SQL.
    ///
    /// Always invoked via [`Self::reset_all`] in production with the
    /// canonical [`Self::DEFAULT_RESET_SEED_SQL`]. The tests for
    /// crosslink #400 use this helper with intentionally malformed seed
    /// SQL to exercise the rollback path; it is `pub(crate)` so it is not
    /// part of the public API.
    pub(crate) fn reset_all_with_seed_sql(&self, seed_sql: &str) -> Result<()> {
        // Delegate to a free function that takes the bare `&mut Connection`
        // so the mutex guard is dropped on return — avoids
        // clippy::significant_drop_tightening for a held `Transaction<'_>`
        // that borrows from a named guard.
        Self::reset_all_on_conn(&mut *self.lock_conn()?, seed_sql)
    }

    /// Inner helper: run the transactional reset on a `Connection`
    /// reference. Extracted so the mutex guard in
    /// [`Self::reset_all_with_seed_sql`] has no lifetime overlap with the
    /// returned `Ok(())`.
    fn reset_all_on_conn(conn: &mut Connection, seed_sql: &str) -> Result<()> {
        // `transaction()` issues `BEGIN DEFERRED` and returns a Transaction
        // whose Drop impl rolls back if `commit()` is never reached.
        let tx = conn
            .transaction()
            .context("failed to begin reset_all transaction")?;

        tx.execute_batch(
            r"
            DELETE FROM archival_memory_tags;
            DELETE FROM archival_memory;
            DELETE FROM core_memory;
            DELETE FROM recent_sessions;
            DELETE FROM recent_activity;
            DELETE FROM coding_patterns;
            DELETE FROM file_relationships;
            DELETE FROM error_patterns;
            DELETE FROM learned_preferences;
            ",
        )
        .context("reset_all: delete phase failed")?;

        tx.execute_batch(seed_sql)
            .context("reset_all: core_memory reseed failed")?;

        tx.commit().context("reset_all: commit failed")?;
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

    // --- Regression tests for crosslink #444 ---

    #[test]
    fn fts5_escape_wraps_bare_word_in_quotes() {
        assert_eq!(escape_fts5_phrase("hello"), r#""hello""#);
    }

    #[test]
    fn fts5_escape_doubles_interior_quotes() {
        assert_eq!(escape_fts5_phrase(r#"say "hi""#), r#""say ""hi""""#);
    }

    #[test]
    fn fts5_escape_neutralizes_boolean_operators() {
        // `AND`, `OR`, `NOT`, `NEAR` inside a phrase are treated as
        // literal words, not operators.
        let escaped = escape_fts5_phrase("foo OR bar NOT baz");
        assert!(escaped.starts_with('"') && escaped.ends_with('"'));
        assert!(escaped.contains("foo OR bar NOT baz"));
    }

    #[test]
    fn fts5_escape_neutralizes_column_filter() {
        // `colname:token` is how FTS5 restricts matching to one column;
        // wrapped in a phrase it becomes literal text.
        let escaped = escape_fts5_phrase("content:secret");
        assert_eq!(escaped, r#""content:secret""#);
    }

    #[test]
    fn fts5_escape_strips_control_chars() {
        // Newlines, NULs, and other ASCII control chars are stripped —
        // FTS5 tokenizer behaves oddly on them and they have no
        // legitimate place in a search query.
        let escaped = escape_fts5_phrase("foo\nbar\0baz\x1b[31m");
        assert_eq!(escaped, r#""foobarbaz[31m""#);
    }

    #[test]
    fn fts5_escape_handles_empty_input() {
        assert_eq!(escape_fts5_phrase(""), r#""""#);
    }

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

    // -----------------------------------------------------------------------
    // B4 — MemoryDb SQLite round-trips (spec §B4, crosslink #548)
    // Each sub-test uses a fresh tempfile DB to stay isolated.
    // -----------------------------------------------------------------------

    /// B4: fresh DB migrates to `schema_version` = 3 and pre-populates the
    /// three core memory sections (persona, `project_info`, `user_preferences`).
    #[test]
    fn b4_schema_migration_reaches_v3_with_core_sections() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let core = db.get_core_memory().unwrap();
        // Exactly three pre-populated sections.
        assert_eq!(core.len(), 3);
        let sections: Vec<&str> = core.iter().map(|c| c.section.as_str()).collect();
        assert!(sections.contains(&SECTION_PERSONA));
        assert!(sections.contains(&SECTION_PROJECT_INFO));
        assert!(sections.contains(&SECTION_USER_PREFS));

        // Sentinel placeholder text is present (not empty).
        for c in &core {
            assert!(
                !c.content.trim().is_empty(),
                "section '{}' must have placeholder text after migration",
                c.section
            );
        }
    }

    /// B4: `MemoryDb::open` accepts an explicit `path` argument — not a
    /// hardcoded location.  Two separate DBs at different paths are
    /// fully independent (no cross-contamination).
    #[test]
    fn b4_open_with_explicit_path_is_isolated() {
        let dir = tempdir().unwrap();
        let db_a = MemoryDb::open(&dir.path().join("a.db")).unwrap();
        let db_b = MemoryDb::open(&dir.path().join("b.db")).unwrap();

        db_a.memory_save("only in A", &[]).unwrap();

        let results_b = db_b.memory_search("only in A", 10).unwrap();
        assert!(
            results_b.is_empty(),
            "db_b must not see records written to db_a"
        );
    }

    /// B4: `archival_memory` FTS5 round-trip — save → search by content word.
    #[test]
    fn b4_archival_memory_fts_round_trip() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id = db
            .memory_save(
                "OpenClaudia uses FTS5 for full-text search",
                &["fts".into(), "sqlite".into()],
            )
            .unwrap();
        assert!(id > 0);

        // Search by a word that appears in the content.
        let hits = db.memory_search("FTS5", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        assert!(hits[0].tags.contains(&"fts".to_string()));
        assert!(hits[0].tags.contains(&"sqlite".to_string()));
    }

    /// B4: `RecentSession` CRUD — save → retrieve, field mapping preserved.
    #[test]
    fn b4_recent_session_round_trip() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id = db
            .save_session_summary(
                "sess-b4",
                "Refactored memory module",
                &["src/memory.rs".into(), "src/lib.rs".into()],
                &["#548".into()],
                "2026-05-18 00:00:00",
            )
            .unwrap();
        assert!(id > 0);

        let sessions = db.get_recent_sessions(5).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.session_id, "sess-b4");
        assert_eq!(s.summary, "Refactored memory module");
        assert_eq!(s.files_modified, vec!["src/memory.rs", "src/lib.rs"]);
        assert_eq!(s.issues_worked, vec!["#548"]);
    }

    /// B4: `CoreMemory` CRUD — upsert replaces content, section key stable.
    #[test]
    fn b4_core_memory_upsert_round_trip() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        db.update_core_memory(SECTION_PERSONA, "I am OpenClaudia test persona")
            .unwrap();
        let got = db
            .get_core_memory_section(SECTION_PERSONA)
            .unwrap()
            .unwrap();
        assert_eq!(got.content, "I am OpenClaudia test persona");

        // Upsert again — section count stays at 3, not 4.
        db.update_core_memory(SECTION_PERSONA, "Updated persona")
            .unwrap();
        let all = db.get_core_memory().unwrap();
        assert_eq!(all.len(), 3);
        let updated = all.iter().find(|c| c.section == SECTION_PERSONA).unwrap();
        assert_eq!(updated.content, "Updated persona");
    }

    /// B4: `CodingPattern` CRUD — save increments confidence on duplicate.
    #[test]
    fn b4_coding_pattern_confidence_increments_on_duplicate() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id1 = db
            .save_coding_pattern("src/*.rs", "convention", "Use thiserror for library errors")
            .unwrap();
        let id2 = db
            .save_coding_pattern("src/*.rs", "convention", "Use thiserror for library errors")
            .unwrap();
        // Same row — id unchanged.
        assert_eq!(id1, id2);

        let patterns = db.get_patterns_for_file("src/memory.rs").unwrap();
        let p = patterns
            .iter()
            .find(|p| p.description.contains("thiserror"))
            .unwrap();
        // confidence starts at 1 (INSERT), increments to 2 on second call.
        assert_eq!(p.confidence, 2);
    }

    /// B4: `memory_delete` removes the row; subsequent get returns None.
    #[test]
    fn b4_archival_memory_delete_removes_row() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id = db.memory_save("to be deleted", &[]).unwrap();
        assert!(db.memory_get(id).unwrap().is_some());

        let deleted = db.memory_delete(id).unwrap();
        assert!(deleted);
        assert!(db.memory_get(id).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // B6 — Missing entrypoint returns None, not an error (spec §B6)
    // These tests live in entrypoint.rs for load_entrypoint; here we pin
    // the MemoryDb side: open() on a fresh path succeeds (never errors on
    // missing file — SQLite creates it).
    // -----------------------------------------------------------------------

    /// B6 (`MemoryDb` side): opening a DB at a non-existent path creates it
    /// rather than returning an error.  Callers get a valid DB, not None.
    #[test]
    fn b6_open_new_path_creates_db_without_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("brand_new.db");
        assert!(!path.exists());

        let result = MemoryDb::open(&path);
        assert!(result.is_ok(), "open on fresh path must succeed");
        assert!(path.exists(), "DB file must be created");
    }

    /// B6: `open_for_project` creates the `.openclaudia/` directory if absent.
    #[test]
    fn b6_open_for_project_creates_directory() {
        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("myproject");
        std::fs::create_dir_all(&project_dir).unwrap();

        let result = MemoryDb::open_for_project(&project_dir);
        assert!(result.is_ok());
        assert!(project_dir.join(".openclaudia").join("memory.db").exists());
    }

    // -----------------------------------------------------------------------
    // B4 extra: escape_fts5_phrase + memory_search with special characters
    // -----------------------------------------------------------------------

    /// Pin B4: FTS5 search with query that contains FTS5 operator keywords
    /// does not panic or return an error — `escape_fts5_phrase` neutralizes them.
    #[test]
    fn b4_fts_search_with_operator_keywords_does_not_error() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();
        db.memory_save("Some neutral content", &[]).unwrap();

        // "AND", "OR", "NOT" are FTS5 operators — escaping wraps them in a phrase.
        let result = db.memory_search("AND OR NOT NEAR", 5);
        assert!(result.is_ok(), "FTS5 operator query must not error");
    }

    // -----------------------------------------------------------------------
    // Security regression tests — crosslink #255
    // `prune_auto_learn_table` must use parameterized queries; callers cannot
    // inject arbitrary SQL through either the table name or the row limit.
    // -----------------------------------------------------------------------

    /// #255: `prune_auto_learn_table` succeeds on an empty table without
    /// errors — the DELETE is a no-op, not a crash.
    #[test]
    fn sqli_255_prune_empty_table_is_noop() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        // No rows in any table; prune should silently succeed.
        let result = db.prune_auto_learn_table(AutoLearnTable::CodingPatterns, 500);
        assert!(result.is_ok(), "prune on empty table must not error");
    }

    /// #255: `prune_auto_learn_table` prunes to the requested limit and
    /// retains exactly that many rows (or fewer if the table is smaller).
    /// Proves the `?1` parameter binding is honoured — not a literal 0 or ∞.
    #[test]
    fn sqli_255_prune_retains_keep_most_recent_rows() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        // Insert 5 coding patterns (each unique description → 5 distinct rows).
        for i in 0..5u32 {
            db.save_coding_pattern("src/*.rs", "convention", &format!("pattern-{i}"))
                .unwrap();
        }

        // Prune to 3 — only 3 rows should remain.
        db.prune_auto_learn_table(AutoLearnTable::CodingPatterns, 3)
            .unwrap();

        let remaining = db.get_patterns_for_file("src/anything.rs").unwrap();
        assert_eq!(remaining.len(), 3, "prune keep=3 must leave exactly 3 rows");
    }

    /// #255: the new typed API covers all four auto-learn tables without
    /// panicking.  Each prune on an empty table must return Ok(()).
    /// This is a compile-time + runtime proof that the enum match is
    /// exhaustive — a future variant that has no match arm won't compile.
    #[test]
    fn sqli_255_all_auto_learn_table_variants_are_handled() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        // If any variant were missing a match arm, compilation would fail.
        for table in [
            AutoLearnTable::CodingPatterns,
            AutoLearnTable::ErrorPatterns,
            AutoLearnTable::LearnedPreferences,
            AutoLearnTable::FileRelationships,
        ] {
            let result = db.prune_auto_learn_table(table, 100);
            assert!(result.is_ok(), "prune on empty {table:?} must succeed");
        }
    }

    // -----------------------------------------------------------------------
    // #278 — MemoryDb ops are safe to call from async context via spawn_blocking.
    //
    // Forensic evidence: `MemoryDb` wrapped `rusqlite::Connection` in
    // `std::sync::Mutex`.  Public methods called `.lock().unwrap()`, which
    // (a) panics on mutex poison instead of returning `Err`, and
    // (b) blocks the Tokio worker thread for the full duration of the SQLite
    //     call when invoked from an async context without `spawn_blocking`.
    //
    // Fix applied: all `.lock().unwrap()` replaced with `lock_conn()?` which
    // converts mutex poison into `anyhow::Error` via `map_err`.  The test
    // below demonstrates the correct async call pattern: the `Arc<MemoryDb>`
    // is moved into `spawn_blocking` so the synchronous SQLite I/O executes
    // on a dedicated blocking thread and never occupies the async executor.
    // -----------------------------------------------------------------------

    /// #278: `MemoryDb` round-trip via `spawn_blocking` — save on blocking thread,
    /// search on blocking thread, results visible across both calls.
    #[tokio::test]
    async fn issue_278_memory_db_round_trip_via_spawn_blocking() {
        use std::sync::Arc;

        let dir = tempdir().unwrap();
        let db = Arc::new(MemoryDb::open(&dir.path().join("test.db")).unwrap());

        // Write: move Arc into blocking thread — guard dropped before .await.
        let db_write = Arc::clone(&db);
        let saved_id = tokio::task::spawn_blocking(move || {
            db_write.memory_save("blocking thread write for #278", &["spawn_blocking".into()])
        })
        .await
        .expect("spawn_blocking join must succeed")
        .expect("memory_save must succeed");

        assert!(saved_id > 0, "inserted row must have positive id");

        // Read: separate spawn_blocking call — guard is fully released before .await.
        let db_read = Arc::clone(&db);
        let results = tokio::task::spawn_blocking(move || db_read.memory_search("blocking", 10))
            .await
            .expect("spawn_blocking join must succeed")
            .expect("memory_search must succeed");

        assert_eq!(results.len(), 1, "search must find exactly the saved entry");
        assert_eq!(results[0].id, saved_id);
        assert!(
            results[0].content.contains("blocking thread write"),
            "content must round-trip"
        );
    }

    /// #278: `lock_conn` converts mutex poison into `Err` rather than panicking.
    ///
    /// We poison the mutex by spawning a thread that locks it and panics while
    /// holding the guard, then verify that `memory_save` returns `Err` with a
    /// message containing "poisoned" instead of unwinding the current thread.
    #[test]
    fn issue_278_poisoned_mutex_returns_err_not_panic() {
        let dir = tempdir().unwrap();
        let db = std::sync::Arc::new(MemoryDb::open(&dir.path().join("test.db")).unwrap());

        // Poison the mutex by locking and panicking inside a scoped thread.
        let db_poison = std::sync::Arc::clone(&db);
        let _ = std::thread::spawn(move || {
            let _guard = db_poison.conn.lock().unwrap();
            panic!("intentional poison");
        })
        .join(); // join returns Err (thread panicked) — that's expected.

        // After poisoning, lock_conn() must return Err, not panic.
        let result = db.memory_save("should fail", &[]);
        assert!(result.is_err(), "poisoned mutex must yield Err, not panic");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("poison"),
            "error message must mention 'poison': {msg}"
        );
    }

    // --- Regression tests for crosslink #400 -----------------------------
    //
    // Before the fix, `reset_all` issued one `execute_batch` containing
    // DELETEs across every table followed by INSERTs into `core_memory`.
    // Because `execute_batch` does NOT wrap its statements in a
    // transaction, a crash or constraint failure between the DELETEs and
    // the INSERTs left the database with zero rows in `core_memory` — no
    // persona, no project_info, no user_preferences. The tests below
    // exercise the transactional contract: success commits, mid-reseed
    // failure rolls back, and a concurrent reader observes either the
    // pre-reset state or the fully-reseeded state, never an in-flight
    // empty snapshot.
    // ---------------------------------------------------------------------

    /// #400: success path — `reset_all` commits both the wipe and the
    /// reseed as a single unit. Archival rows from before the reset are
    /// gone, and the three canonical core-memory sections are present
    /// afterwards.
    #[test]
    fn issue_400_reset_all_success_commits_wipe_and_reseed() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        // Pre-populate: archival rows plus a customised persona.
        db.memory_save("entry one", &["a".into()]).unwrap();
        db.memory_save("entry two", &["b".into()]).unwrap();
        db.update_core_memory(SECTION_PERSONA, "custom persona")
            .unwrap();

        db.reset_all()
            .expect("reset_all must succeed on healthy db");

        let leftovers = db.memory_list(100).unwrap();
        assert!(
            leftovers.is_empty(),
            "archival_memory must be empty after reset_all, found {} rows",
            leftovers.len()
        );

        let core = db.get_core_memory().unwrap();
        assert_eq!(
            core.len(),
            3,
            "reset_all must reseed exactly 3 core_memory rows, found {}",
            core.len()
        );
        let persona = db
            .get_core_memory_section(SECTION_PERSONA)
            .unwrap()
            .expect("persona row must exist after reset_all commit");
        assert!(
            persona.content.starts_with("I am an AI assistant"),
            "persona content must be the default seed, got: {}",
            persona.content
        );
    }

    /// #400: rollback path — if the reseed step fails (here, a duplicate
    /// PRIMARY KEY on `core_memory.section`), the transaction's Drop impl
    /// rolls back the preceding DELETEs. The pre-existing rows must
    /// survive.
    #[test]
    fn issue_400_reset_all_mid_reseed_failure_rolls_back() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let pre_id = db
            .memory_save("must survive failed reset", &["preserve".into()])
            .unwrap();
        db.update_core_memory(SECTION_PERSONA, "pre-reset persona marker")
            .unwrap();

        // Bad seed: two rows with the same PRIMARY KEY (section='persona').
        // The first INSERT succeeds, the second raises a UNIQUE constraint
        // failure, which aborts the txn. Because `execute_batch` returns
        // Err, `reset_all_with_seed_sql` never calls `tx.commit()`, so
        // Drop on the Transaction issues ROLLBACK.
        let bad_seed = r"
            INSERT INTO core_memory (section, content) VALUES
                ('persona', 'partial seed row 1'),
                ('persona', 'duplicate primary key -> UNIQUE violation');
        ";
        let err = db
            .reset_all_with_seed_sql(bad_seed)
            .expect_err("duplicate PRIMARY KEY must propagate as Err");
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("reseed") || msg.contains("unique"),
            "error must reference the reseed phase or UNIQUE violation, got: {msg}"
        );

        // Forensic evidence: pre-reset rows are intact, NOT half-wiped.
        let survivors = db.memory_list(100).unwrap();
        assert_eq!(
            survivors.len(),
            1,
            "rollback must preserve archival rows; found {} (expected 1)",
            survivors.len()
        );
        assert_eq!(
            survivors[0].id, pre_id,
            "the surviving row must be the one inserted before reset_all"
        );

        let core = db.get_core_memory().unwrap();
        assert_eq!(
            core.len(),
            3,
            "rollback must preserve the original 3 core_memory rows, found {}",
            core.len()
        );
        let persona = db
            .get_core_memory_section(SECTION_PERSONA)
            .unwrap()
            .expect("persona row must still exist after rollback");
        assert_eq!(
            persona.content, "pre-reset persona marker",
            "rollback must restore the pre-reset persona content, got: {}",
            persona.content
        );
    }

    /// #400: concurrent reader sees a consistent snapshot.
    ///
    /// One thread loops on `reset_all` while another loops on
    /// `get_core_memory`. The connection mutex plus rusqlite's
    /// `Transaction` (BEGIN DEFERRED + explicit COMMIT) guarantee the
    /// reader either sees the pre-reset state or the fully reseeded
    /// state — never a half-wiped database with zero `core_memory` rows.
    /// Before the fix this test would observe `core.len() == 0` between
    /// the DELETE and the INSERT in the non-transactional
    /// `execute_batch`.
    #[test]
    fn issue_400_reset_all_concurrent_reader_sees_consistent_state() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempdir().unwrap();
        let db = Arc::new(MemoryDb::open(&dir.path().join("test.db")).unwrap());

        // Seed some auto-learning state so reset_all has real work to do
        // in its delete phase.
        for i in 0..20 {
            db.memory_save(&format!("row {i}"), &["seed".into()])
                .unwrap();
        }

        let stop = Arc::new(AtomicBool::new(false));

        let writer_db = Arc::clone(&db);
        let writer_stop = Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            let mut iterations = 0_u32;
            while !writer_stop.load(Ordering::Relaxed) {
                writer_db.reset_all().expect("reset_all must succeed");
                iterations += 1;
                if iterations >= 50 {
                    break;
                }
            }
            iterations
        });

        let reader_db = Arc::clone(&db);
        let reader = std::thread::spawn(move || {
            let mut observations = 0_u32;
            let mut bad: Option<usize> = None;
            for _ in 0..500 {
                let core = reader_db
                    .get_core_memory()
                    .expect("get_core_memory must not error");
                if core.len() != 3 {
                    bad = Some(core.len());
                    break;
                }
                observations += 1;
            }
            (observations, bad)
        });

        let writer_iters = writer.join().expect("writer thread panicked");
        stop.store(true, Ordering::Relaxed);
        let (reader_obs, bad) = reader.join().expect("reader thread panicked");

        assert!(
            bad.is_none(),
            "reader observed inconsistent core_memory row count: got {} rows \
             (must always be exactly 3) after {} clean observations and {} \
             reset_all iterations — proves a non-transactional reset",
            bad.unwrap_or_default(),
            reader_obs,
            writer_iters,
        );
        assert!(
            writer_iters > 0,
            "writer must have completed at least one reset_all"
        );
        assert!(
            reader_obs > 0,
            "reader must have observed core_memory at least once"
        );
    }

    // ---------------------------------------------------------------------
    // Crosslink #464 — archival_memory tags normalised into a junction table
    //
    // Before the fix, `memory_save(content, &["rust, tokio".into()])` was
    // serialised as the literal string "rust, tokio" in `archival_memory.tags`.
    // Every read path split on ',' so the single tag came back as
    // ["rust", "tokio"] — silent data corruption.  Queries used substring
    // `LIKE` semantics so `%rust%` also matched `rustaceans`.
    //
    // The fix moves tags into `archival_memory_tags(memory_id, tag)`.
    // The tests below pin the four properties the schema must satisfy:
    //   1. A tag value with a literal comma round-trips unchanged.
    //   2. `memory_search_by_tag` returns exactly the memories tagged with
    //      that tag — no substring false positives, no false negatives.
    //   3. Pre-v4 comma-joined data is migrated into the new table without
    //      loss for well-formed input, and `ALTER TABLE DROP COLUMN`
    //      retires the legacy column.
    //   4. `memory_save(content, &[])` writes the content row with zero
    //      tag rows; the read path returns an empty `tags: Vec<String>`.
    // ---------------------------------------------------------------------

    /// #464-1: a tag containing a literal comma survives a save -> get
    /// round-trip unchanged.  This is the forensic evidence that the old
    /// `tags.join(",")` + `split(',')` pipeline was corrupting data.
    #[test]
    fn issue_464_tag_with_comma_round_trips_intact() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let mut original = vec![
            "rust, tokio".to_string(),           // literal comma — old code split this
            "key=value, with comma".to_string(), // another comma-bearing tag
            "no-comma".to_string(),
        ];
        let id = db
            .memory_save("project notes", &original)
            .expect("save must succeed");

        let got = db
            .memory_get(id)
            .expect("get must succeed")
            .expect("row must exist");

        // load_tags_for orders by tag, so got.tags comes back sorted.
        original.sort();
        assert_eq!(
            got.tags, original,
            "comma-bearing tags must round-trip intact; \
             before #464 'rust, tokio' would have come back as ['rust', 'tokio']"
        );

        // Specifically: the literal "rust, tokio" tag is present as ONE entry.
        assert!(
            got.tags.iter().any(|t| t == "rust, tokio"),
            "the literal comma-bearing tag must be preserved as a single tag, got: {:?}",
            got.tags
        );
        assert!(
            !got.tags.iter().any(|t| t == "rust"),
            "the comma must NOT have split the tag into 'rust' — got: {:?}",
            got.tags
        );
    }

    /// #464-2: `memory_search_by_tag` is exact-match and indexed — querying
    /// for "rust" must return only rows tagged exactly "rust", never rows
    /// whose tag merely contains "rust" as a substring.
    #[test]
    fn issue_464_search_by_tag_is_exact_match_not_substring() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id_rust = db.memory_save("about rust", &["rust".into()]).unwrap();
        // "rustaceans" shares the 'rust' substring — old LIKE-based query
        // would falsely match this.
        let _id_rustaceans = db
            .memory_save("about the community", &["rustaceans".into()])
            .unwrap();
        // Multi-tag row — should also match a query for "rust".
        let id_multi = db
            .memory_save("two tags", &["rust".into(), "tokio".into()])
            .unwrap();

        let hits = db
            .memory_search_by_tag("rust", 50)
            .expect("tag search must succeed");
        let hit_ids: Vec<i64> = hits.iter().map(|m| m.id).collect();

        assert!(
            hit_ids.contains(&id_rust),
            "exact-match row must be returned"
        );
        assert!(
            hit_ids.contains(&id_multi),
            "row carrying 'rust' alongside other tags must be returned"
        );
        assert_eq!(
            hit_ids.len(),
            2,
            "exact-match tag query must NOT match 'rustaceans' (substring); got ids: {hit_ids:?}",
        );

        // Sanity: searching for a tag nobody has returns no rows.
        let zero = db.memory_search_by_tag("nonexistent", 50).unwrap();
        assert!(zero.is_empty(), "unknown tag must return empty results");
    }

    /// #464-3: migration from a pre-v4 database preserves every well-formed
    /// tag in the legacy comma-joined column.  We build a v3-shaped
    /// database by hand (because production code now starts at v4), then
    /// reopen it through `MemoryDb::open` which triggers `migrate_v4_on`.
    #[test]
    fn issue_464_migration_preserves_legacy_comma_joined_data() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");

        // Step 1: build a legacy v3 schema by hand.  We deliberately do
        // NOT call MemoryDb::open here — that would jump straight to v4.
        {
            let raw = rusqlite::Connection::open(&db_path).unwrap();
            raw.execute_batch(
                r"
                CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
                CREATE TABLE archival_memory (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    content TEXT NOT NULL,
                    tags TEXT DEFAULT '',
                    created_at TEXT DEFAULT (datetime('now')),
                    updated_at TEXT DEFAULT (datetime('now'))
                );
                CREATE TABLE core_memory (
                    section TEXT PRIMARY KEY,
                    content TEXT NOT NULL,
                    updated_at TEXT DEFAULT (datetime('now'))
                );
                INSERT INTO core_memory (section, content) VALUES
                    ('persona', 'p'),
                    ('project_info', 'pi'),
                    ('user_preferences', 'up');
                CREATE VIRTUAL TABLE archival_memory_fts USING fts5(
                    content, tags, content=archival_memory, content_rowid=id
                );
                CREATE TRIGGER archival_memory_ai AFTER INSERT ON archival_memory BEGIN
                    INSERT INTO archival_memory_fts(rowid, content, tags)
                        VALUES (new.id, new.content, new.tags);
                END;
                INSERT INTO schema_version (version) VALUES (3);
                ",
            )
            .unwrap();
            // Insert three legacy rows with comma-joined tag strings.
            raw.execute(
                "INSERT INTO archival_memory (id, content, tags) VALUES (?1, ?2, ?3)",
                params![1_i64, "row one", "rust,tokio,async"],
            )
            .unwrap();
            raw.execute(
                "INSERT INTO archival_memory (id, content, tags) VALUES (?1, ?2, ?3)",
                params![2_i64, "row two", "  sqlite , fts  "], // whitespace around tags
            )
            .unwrap();
            // Empty tag string must produce zero junction rows, not [""].
            raw.execute(
                "INSERT INTO archival_memory (id, content, tags) VALUES (?1, ?2, ?3)",
                params![3_i64, "row three", ""],
            )
            .unwrap();
            drop(raw);
        }

        // Step 2: reopen via MemoryDb::open — this triggers migrate_v4_on.
        let db = MemoryDb::open(&db_path).expect("v4 migration must succeed");

        // Forensic evidence: junction-table rows exist for the well-formed
        // legacy data, and the legacy `tags` column is gone.
        let row_one = db.memory_get(1).unwrap().unwrap();
        assert_eq!(
            row_one.tags,
            vec!["async".to_string(), "rust".to_string(), "tokio".to_string()],
            "row 1 must migrate to three discrete tag rows; got {:?}",
            row_one.tags
        );

        let row_two = db.memory_get(2).unwrap().unwrap();
        assert_eq!(
            row_two.tags,
            vec!["fts".to_string(), "sqlite".to_string()],
            "whitespace around legacy tags must be trimmed during migration"
        );

        let row_three = db.memory_get(3).unwrap().unwrap();
        assert!(
            row_three.tags.is_empty(),
            "empty legacy tag string must migrate to zero junction rows; got {:?}",
            row_three.tags
        );

        // The legacy column must be gone: re-querying `tags` from
        // `archival_memory` should error with "no such column".
        let lock = db.conn.lock().unwrap();
        let err = lock
            .prepare("SELECT tags FROM archival_memory LIMIT 1")
            .expect_err("legacy tags column must have been dropped");
        let msg = err.to_string();
        assert!(
            msg.contains("no such column") || msg.contains("tags"),
            "expected 'no such column' error after v4 drop; got: {msg}"
        );
        drop(lock);

        // schema_version must now be 4.
        let v_lock = db.conn.lock().unwrap();
        let version: i64 = v_lock
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        drop(v_lock);
        assert_eq!(version, 4, "schema_version must be 4 after migration");
    }

    /// #464-4: `memory_save(content, &[])` writes only the content row;
    /// the read path returns `tags: Vec<String>` with `len() == 0`.
    /// Before the fix, the empty slice was joined to "" and split back to
    /// `[""]`, then filtered — wasted allocations and a leaky abstraction.
    /// Now the empty case writes zero junction rows by construction.
    #[test]
    fn issue_464_empty_tag_list_produces_no_junction_rows() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id = db
            .memory_save("untagged content", &[])
            .expect("save with empty tag list must succeed");

        let got = db.memory_get(id).unwrap().unwrap();
        assert!(
            got.tags.is_empty(),
            "memory with empty tag slice must read back with no tags; got {:?}",
            got.tags
        );

        // Direct forensic check: zero rows in archival_memory_tags for this id.
        let conn = db.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM archival_memory_tags WHERE memory_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(
            count, 0,
            "empty tag slice must write zero rows to the junction table; found {count}"
        );

        // And `memory_search_by_tag("")` must not match this row.
        let hits = db.memory_search_by_tag("", 10).unwrap();
        assert!(
            hits.iter().all(|m| m.id != id),
            "the empty-tag-list row must not appear in a tag search for empty string"
        );
    }

    /// #464-5 (bonus): `memory_delete` cascades into `archival_memory_tags`
    /// via the FK + `PRAGMA foreign_keys=ON`.  Without the pragma, the
    /// cascade is silently inert and stale tag rows accumulate forever.
    #[test]
    fn issue_464_delete_cascades_into_junction_table() {
        let dir = tempdir().unwrap();
        let db = MemoryDb::open(&dir.path().join("test.db")).unwrap();

        let id = db
            .memory_save("doomed", &["a".into(), "b".into(), "c".into()])
            .unwrap();

        let before_count: i64 = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM archival_memory_tags WHERE memory_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(before_count, 3, "three tag rows must exist pre-delete");

        assert!(db.memory_delete(id).unwrap());

        let after_count: i64 = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM archival_memory_tags WHERE memory_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            after_count, 0,
            "FK CASCADE must remove all tag rows when the memory is deleted; \
             found {after_count} orphans (indicates PRAGMA foreign_keys is off)"
        );
    }
}
