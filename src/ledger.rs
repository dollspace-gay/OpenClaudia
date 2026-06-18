//! Authoritative observation ledger for grounded agent decisions.
//!
//! Chat history, memory, and compaction summaries are useful navigation
//! aids, but they are not facts. `RealityLedger` records observations from
//! authoritative boundaries such as the user, filesystem, commands, git, and
//! verifiers. The decision gate can then require model actions to cite ledger
//! IDs instead of relying on provider chat history.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObsId(Uuid);

impl ObsId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ObsId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ObsId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for ObsId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: ObsId,
    pub ts: DateTime<Utc>,
    pub kind: ObservationKind,
    pub authority: Authority,
}

impl Observation {
    #[must_use]
    pub fn new(authority: Authority, kind: ObservationKind) -> Self {
        Self {
            id: ObsId::new(),
            ts: Utc::now(),
            kind,
            authority,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    User,
    Tool,
    Filesystem,
    Command,
    Git,
    Policy,
    Verifier,
    /// Model summaries are retained for navigation, but never for proof.
    ModelSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObservationKind {
    UserTask {
        content: String,
    },
    FileRead {
        path: String,
        sha256: String,
        start_line: usize,
        end_line: usize,
        excerpt: String,
    },
    CommandRun {
        cwd: String,
        argv: Vec<String>,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    DiffObserved {
        files: Vec<String>,
        patch: String,
    },
    ToolResult {
        tool: String,
        result: serde_json::Value,
    },
    PolicyDecision {
        allowed: bool,
        reason: String,
    },
    Verification {
        passed: bool,
        command: Option<String>,
        findings: Vec<String>,
    },
    Summary {
        text: String,
        source_obs: Vec<ObsId>,
    },
}

impl ObservationKind {
    #[must_use]
    pub fn compact_label(&self) -> String {
        match self {
            Self::UserTask { content } => format!("user_task {}", first_line(content)),
            Self::FileRead {
                path,
                sha256,
                start_line,
                end_line,
                ..
            } => {
                format!("file {path} sha256={sha256} lines {start_line}-{end_line}")
            }
            Self::CommandRun {
                argv, exit_code, ..
            } => {
                format!("command {:?} exit={exit_code}", argv)
            }
            Self::DiffObserved { files, patch } => {
                format!("diff {} files {} bytes", files.len(), patch.len())
            }
            Self::ToolResult { tool, .. } => format!("tool_result {tool}"),
            Self::PolicyDecision { allowed, reason } => {
                format!("policy allowed={allowed} {}", first_line(reason))
            }
            Self::Verification {
                passed,
                command,
                findings,
            } => {
                let command = command.as_deref().unwrap_or("<none>");
                format!(
                    "verification passed={passed} command={command} findings={}",
                    findings.len()
                )
            }
            Self::Summary { text, source_obs } => {
                format!("summary sources={} {}", source_obs.len(), first_line(text))
            }
        }
    }

    #[must_use]
    pub fn touched_files(&self) -> Vec<&str> {
        match self {
            Self::FileRead { path, .. } => vec![path.as_str()],
            Self::DiffObserved { files, .. } => files.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationIndexEntry {
    pub id: ObsId,
    pub ts: DateTime<Utc>,
    pub authority: Authority,
    pub stale: bool,
    pub label: String,
}

#[derive(Debug, Clone)]
struct ObservationRecord {
    observation: Observation,
    stale: bool,
}

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("sqlite ledger operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("ledger observation serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("duplicate observation id {0}")]
    DuplicateObservation(ObsId),
}

pub struct RealityLedger {
    records: HashMap<ObsId, ObservationRecord>,
    conn: Option<Connection>,
}

impl Default for RealityLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl RealityLedger {
    #[must_use]
    pub fn new() -> Self {
        Self {
            records: HashMap::new(),
            conn: None,
        }
    }

    /// Open a SQLite-backed ledger and load existing observations into memory.
    ///
    /// The full observation JSON is retained in SQLite. Compact prompt packets
    /// should pass indexes or selected hydrated observations to the model, but
    /// compaction must not delete rows from this table.
    ///
    /// # Errors
    ///
    /// Returns an error when SQLite cannot be opened, schema initialization
    /// fails, or any existing observation row cannot be deserialized.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let conn = Connection::open(path)?;
        initialize_schema(&conn)?;

        let records = {
            let mut stmt = conn.prepare(
                "SELECT observation_json, stale FROM reality_observations ORDER BY ts ASC",
            )?;
            let mut rows = stmt.query([])?;
            let mut records = HashMap::new();
            while let Some(row) = rows.next()? {
                let json: String = row.get(0)?;
                let stale: i64 = row.get(1)?;
                let observation: Observation = serde_json::from_str(&json)?;
                records.insert(
                    observation.id,
                    ObservationRecord {
                        observation,
                        stale: stale != 0,
                    },
                );
            }
            records
        };

        Ok(Self {
            records,
            conn: Some(conn),
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: ObsId) -> Option<&Observation> {
        self.records.get(&id).map(|record| &record.observation)
    }

    #[must_use]
    pub fn is_stale(&self, id: ObsId) -> bool {
        self.records.get(&id).is_some_and(|record| record.stale)
    }

    #[must_use]
    pub fn is_authoritative(&self, id: ObsId) -> bool {
        self.records.get(&id).is_some_and(|record| {
            !record.stale && record.observation.authority != Authority::ModelSummary
        })
    }

    /// Append a fully-formed observation.
    ///
    /// # Errors
    ///
    /// Returns an error if the observation id already exists or persistence
    /// fails.
    pub fn append_observation(&mut self, observation: Observation) -> Result<ObsId, LedgerError> {
        let id = observation.id;
        if self.records.contains_key(&id) {
            return Err(LedgerError::DuplicateObservation(id));
        }
        let record = ObservationRecord {
            observation,
            stale: false,
        };
        self.persist_record(&record)?;
        self.records.insert(id, record);
        Ok(id)
    }

    /// Append a new observation with the current timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    pub fn append(
        &mut self,
        authority: Authority,
        kind: ObservationKind,
    ) -> Result<ObsId, LedgerError> {
        self.append_observation(Observation::new(authority, kind))
    }

    /// Record the user's task as the root task specification evidence.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    pub fn observe_user_task(&mut self, content: impl Into<String>) -> Result<ObsId, LedgerError> {
        self.append(
            Authority::User,
            ObservationKind::UserTask {
                content: content.into(),
            },
        )
    }

    /// Record a file read. `sha256` is computed over `full_contents`, while
    /// `excerpt` is the slice that was actually shown to the model.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    pub fn observe_file_read(
        &mut self,
        path: impl Into<String>,
        full_contents: &str,
        start_line: usize,
        end_line: usize,
        excerpt: impl Into<String>,
    ) -> Result<ObsId, LedgerError> {
        self.append(
            Authority::Filesystem,
            ObservationKind::FileRead {
                path: path.into(),
                sha256: sha256_hex(full_contents.as_bytes()),
                start_line,
                end_line,
                excerpt: excerpt.into(),
            },
        )
    }

    /// Record a command result.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    pub fn observe_command_run(
        &mut self,
        cwd: impl Into<String>,
        argv: Vec<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Result<ObsId, LedgerError> {
        self.append(
            Authority::Command,
            ObservationKind::CommandRun {
                cwd: cwd.into(),
                argv,
                exit_code,
                stdout: stdout.into(),
                stderr: stderr.into(),
            },
        )
    }

    /// Record a diff and stale prior file reads for every touched path.
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    pub fn observe_diff(
        &mut self,
        files: Vec<String>,
        patch: impl Into<String>,
    ) -> Result<ObsId, LedgerError> {
        let observation = Observation::new(
            Authority::Git,
            ObservationKind::DiffObserved {
                files,
                patch: patch.into(),
            },
        );
        let id = observation.id;
        if self.records.contains_key(&id) {
            return Err(LedgerError::DuplicateObservation(id));
        }

        let touched = observation.kind.touched_files();
        let touched: HashSet<&str> = touched.into_iter().collect();
        let stale_ids = self
            .records
            .iter()
            .filter_map(|(existing_id, record)| match &record.observation.kind {
                ObservationKind::FileRead { path, .. }
                    if touched.contains(path.as_str()) && !record.stale =>
                {
                    Some(*existing_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let record = ObservationRecord {
            observation,
            stale: false,
        };

        if let Some(conn) = self.conn.as_mut() {
            let tx = conn.transaction()?;
            insert_record(&tx, &record)?;
            for stale_id in &stale_ids {
                tx.execute(
                    "UPDATE reality_observations SET stale = 1 WHERE id = ?1",
                    params![stale_id.to_string()],
                )?;
            }
            tx.commit()?;
        }

        self.records.insert(id, record);
        for stale_id in stale_ids {
            if let Some(record) = self.records.get_mut(&stale_id) {
                record.stale = true;
            }
        }
        Ok(id)
    }

    /// Mark file-read observations for `path` stale.
    ///
    /// This is the primitive write/edit paths should call after mutating a
    /// file. A stale read can still be inspected for history, but cannot be
    /// used as authoritative evidence for a new decision.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite persistence fails.
    pub fn mark_file_observations_stale(&mut self, path: &str) -> Result<Vec<ObsId>, LedgerError> {
        let stale_ids = self
            .records
            .iter()
            .filter_map(|(id, record)| match &record.observation.kind {
                ObservationKind::FileRead {
                    path: observed_path,
                    ..
                } if observed_path == path && !record.stale => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();

        if let Some(conn) = self.conn.as_mut() {
            let tx = conn.transaction()?;
            for id in &stale_ids {
                tx.execute(
                    "UPDATE reality_observations SET stale = 1 WHERE id = ?1",
                    params![id.to_string()],
                )?;
            }
            tx.commit()?;
        }

        for id in &stale_ids {
            if let Some(record) = self.records.get_mut(id) {
                record.stale = true;
            }
        }
        Ok(stale_ids)
    }

    /// Return a compact, chronological observation index for prompt packets.
    #[must_use]
    pub fn observation_index(&self, limit: usize) -> Vec<ObservationIndexEntry> {
        let mut records = self.records.values().collect::<Vec<_>>();
        records.sort_by_key(|record| record.observation.ts);
        if limit > 0 && records.len() > limit {
            records.drain(0..records.len() - limit);
        }
        records
            .into_iter()
            .map(|record| ObservationIndexEntry {
                id: record.observation.id,
                ts: record.observation.ts,
                authority: record.observation.authority,
                stale: record.stale,
                label: record.observation.kind.compact_label(),
            })
            .collect()
    }

    fn persist_record(&mut self, record: &ObservationRecord) -> Result<(), LedgerError> {
        if let Some(conn) = self.conn.as_ref() {
            insert_record(conn, record)?;
        }
        Ok(())
    }
}

fn initialize_schema(conn: &Connection) -> Result<(), LedgerError> {
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reality_observations (
            id TEXT PRIMARY KEY NOT NULL,
            ts TEXT NOT NULL,
            authority TEXT NOT NULL,
            stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
            observation_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_reality_observations_ts
            ON reality_observations(ts);
        CREATE INDEX IF NOT EXISTS idx_reality_observations_authority
            ON reality_observations(authority);",
    )?;
    Ok(())
}

fn insert_record(conn: &Connection, record: &ObservationRecord) -> Result<(), LedgerError> {
    let observation = &record.observation;
    let json = serde_json::to_string(observation)?;
    conn.execute(
        "INSERT INTO reality_observations (id, ts, authority, stale, observation_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation.id.to_string(),
            observation.ts.to_rfc3339(),
            format!("{:?}", observation.authority),
            i64::from(record.stale),
            json
        ],
    )?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn first_line(text: &str) -> String {
    const MAX: usize = 120;
    let line = text.lines().next().unwrap_or_default();
    if line.len() <= MAX {
        return line.to_string();
    }
    format!("{}...", &line[..MAX])
}
