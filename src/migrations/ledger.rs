//! Completion ledger for once-only migrations.
//!
//! Stored as a small JSON file at `~/.local/share/openclaudia/migrations.json`
//! with the shape `{"applied": ["migration-id-1", "migration-id-2"]}`.
//! Corruption or missing files yield an empty ledger — we never fail a
//! boot because this file was unreadable.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// On-disk representation — a sorted set keeps the file stable across
/// runs (so `git diff` stays quiet for users who check this in).
#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default)]
    applied: BTreeSet<String>,
}

/// In-memory completion ledger. Cheap to construct — load once per
/// startup, mutate during migration run, save once at the end.
#[derive(Debug, Default)]
pub struct CompletionLedger {
    applied: BTreeSet<String>,
}

impl CompletionLedger {
    /// Load the ledger from `path`. Missing / unparseable files yield
    /// an empty ledger — callers see this as "no migrations have run
    /// yet", which is the safe default.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<LedgerFile>(&text) {
            Ok(f) => Self { applied: f.applied },
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "migration ledger unreadable — treating as empty"
                );
                Self::default()
            }
        }
    }

    /// True if `id` has already been marked complete.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.applied.contains(id)
    }

    /// Record `id` as complete. Idempotent.
    pub fn mark(&mut self, id: &str) {
        self.applied.insert(id.to_string());
    }

    /// Persist the ledger to `path`. Creates the parent directory if
    /// needed. Uses pretty-printed JSON so manual inspection is easy.
    ///
    /// # Errors
    ///
    /// Returns an error if the filesystem is inaccessible.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = LedgerFile {
            applied: self.applied.clone(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}
