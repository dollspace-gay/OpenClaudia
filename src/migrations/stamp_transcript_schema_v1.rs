//! Stamp `schema_version: 1` onto the transcripts directory.
//!
//! Rationale: the transcript JSONL format shipped in commit b117e0a
//! has no on-disk version marker. The first time we change the
//! [`crate::transcript::SerializedMessage`] envelope — renamed field,
//! new required field, etc. — a future migration needs to know whether
//! the user's existing transcripts are v1 (old format) or v2 (new
//! format). Without a baseline marker, that migration would have to
//! sniff every transcript line to guess.
//!
//! This migration writes `<claude_home>/projects/.schema-version.json`
//! once, containing `{"transcripts": 1}`. It's idempotent: re-running
//! when the marker already exists is a no-op.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Migration, MigrationContext, MigrationOutcome, RunPolicy};

/// Current transcript schema version. Bump in lockstep with the
/// envelope — and add a migration that upgrades the on-disk v1 data to
/// the new format.
const CURRENT_TRANSCRIPT_SCHEMA: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct SchemaMarker {
    #[serde(default)]
    transcripts: Option<u32>,
}

pub struct StampTranscriptSchemaV1;

impl StampTranscriptSchemaV1 {
    fn marker_path(ctx: &MigrationContext) -> PathBuf {
        ctx.claude_home
            .join("projects")
            .join(".schema-version.json")
    }
}

impl Migration for StampTranscriptSchemaV1 {
    fn id(&self) -> &'static str {
        "stamp-transcript-schema-v1"
    }

    fn description(&self) -> &'static str {
        "Write initial transcript schema-version marker (v1)"
    }

    fn run_policy(&self) -> RunPolicy {
        // Idempotent: the write is conditional on the marker being
        // absent or stale. Doesn't need ledger bookkeeping.
        RunPolicy::Idempotent
    }

    fn run(&self, ctx: &MigrationContext) -> MigrationOutcome {
        let path = Self::marker_path(ctx);

        // Fast path: marker already exists and is at or ahead of the
        // current version → nothing to do. We treat a higher version as
        // "newer build wrote it, don't clobber" — this keeps downgrades
        // safe for users who pin to an older release after upgrading.
        //
        // crosslink #734: distinguish "no marker" (Ok(None) → run
        // migration) from "marker is unreadable" (Err → surface as
        // failure). Treating the latter as "absent" would silently
        // overwrite a marker we couldn't even inspect, and the previous
        // `if let Ok(Some(_))` swallowed permission-denied errors so the
        // operator never saw the underlying problem.
        match super::read_json_if_exists(&path) {
            Ok(Some(value)) => {
                if let Ok(marker) = serde_json::from_value::<SchemaMarker>(value) {
                    if marker.transcripts.unwrap_or(0) >= CURRENT_TRANSCRIPT_SCHEMA {
                        return MigrationOutcome::Skipped;
                    }
                }
                // Marker present but malformed/older → fall through to
                // (re)write it with the current version.
            }
            Ok(None) => {
                // No marker yet — first run on this machine; fall through.
            }
            Err(err) => {
                return MigrationOutcome::Failed(err);
            }
        }

        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return MigrationOutcome::Failed(err.into());
            }
        }
        let marker = json!({ "transcripts": CURRENT_TRANSCRIPT_SCHEMA });
        let Ok(text) = serde_json::to_string_pretty(&marker) else {
            // Serializing a 2-field JSON object never fails in practice;
            // if it does we'd rather skip than pretend we applied.
            return MigrationOutcome::Skipped;
        };
        match std::fs::write(&path, text) {
            Ok(()) => MigrationOutcome::Applied(format!(
                "wrote {} (transcripts: v{CURRENT_TRANSCRIPT_SCHEMA})",
                path.display()
            )),
            Err(err) => MigrationOutcome::Failed(err.into()),
        }
    }
}
