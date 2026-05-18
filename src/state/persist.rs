//! On-disk serialization shape for [`super::SessionState`].
//!
//! Today's code ships two divergent on-disk shapes:
//!
//! - `src/tui/app.rs :: TuiSession` — used by the ratatui TUI.
//! - `src/cli/repl/mod.rs :: ChatSession` — used by the chat REPL.
//!
//! They carry the same fields with different serde layouts.
//! Load-saved-in-TUI-resume-in-REPL is not round-trip safe. The
//! [`SessionStateV1`] struct here is the new single source of truth;
//! Phase 5 of the migration (see `docs/designs/510-session-state.md`)
//! retires both back-compat shims once every caller reads / writes
//! through this module.
//!
//! This module is intentionally thin — a `SessionStateV1` is
//! equivalent to a [`super::SessionState`] plus a schema version
//! tag. The serde layout is what serde derives by default; no
//! custom adapters. Future schema bumps ship their own `V2` struct
//! + a `From<V1> for V2` impl + an entry in the migrations framework.

use serde::{Deserialize, Serialize};

use super::SessionState;

/// Schema version 1 — matches [`super::SessionState`] field-for-field.
/// Shipping the version tag from day one gives future migrations a
/// sentinel to dispatch on (see crosslink #506 migrations framework).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStateV1 {
    /// Schema version number. Always `1` for this type — a future
    /// `SessionStateV2` would have `version: 2` and its own struct.
    pub version: u32,
    /// The actual payload.
    #[serde(flatten)]
    pub state: SessionState,
}

impl SessionStateV1 {
    /// The value of `version` this type corresponds to. Callers
    /// that read on-disk files compare the decoded `version` field
    /// against this before deserializing the rest — a mismatch
    /// triggers the migration path.
    pub const CURRENT_VERSION: u32 = 1;

    /// Wrap a `SessionState` in the versioned envelope.
    #[must_use]
    pub const fn wrap(state: SessionState) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            state,
        }
    }

    /// Unwrap, discarding the version tag. Use only after checking
    /// `version == CURRENT_VERSION`; for older versions, route
    /// through the migrations framework first.
    #[must_use]
    pub fn into_state(self) -> SessionState {
        self.state
    }
}

/// Persist errors — short enum so callers don't need to understand
/// serde / `std::io` error hierarchies.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "schema version {found} is newer than the max supported ({supported}); upgrade your harness"
    )]
    FutureSchema { found: u32, supported: u32 },
}

/// Encode a [`SessionState`] as pretty-printed JSON ready to write.
/// Pretty-printing so `git diff`s on committed-state dumps stay
/// readable — the cost is negligible at session-save frequency.
///
/// # Errors
///
/// Returns `PersistError::Json` if serialization fails (should be
/// impossible for the current struct; wired for future additions).
pub fn encode(state: &SessionState) -> Result<String, PersistError> {
    let wrapped = SessionStateV1::wrap(state.clone());
    Ok(serde_json::to_string_pretty(&wrapped)?)
}

/// Decode a JSON string written by [`encode`]. Checks the version
/// tag first; future schemas that outrank `CURRENT_VERSION` return
/// `FutureSchema` so a newer harness doesn't clobber a downgrade
/// user's file on save.
///
/// # Errors
///
/// Returns `PersistError::Json` on malformed JSON, or
/// `PersistError::FutureSchema` when the on-disk version is newer
/// than this binary understands.
pub fn decode(raw: &str) -> Result<SessionState, PersistError> {
    // Peek at the version tag before deserializing the full shape —
    // lets us give a precise error without tripping on unknown fields.
    let peek: VersionPeek = serde_json::from_str(raw)?;
    if peek.version > SessionStateV1::CURRENT_VERSION {
        return Err(PersistError::FutureSchema {
            found: peek.version,
            supported: SessionStateV1::CURRENT_VERSION,
        });
    }
    let v1: SessionStateV1 = serde_json::from_str(raw)?;
    Ok(v1.into_state())
}

#[derive(Deserialize)]
struct VersionPeek {
    #[serde(default = "default_version")]
    version: u32,
}

const fn default_version() -> u32 {
    // A file written BEFORE the version tag existed (TuiSession /
    // ChatSession legacy shape) decodes to version 0. That steers
    // callers through the migrations framework when we ship Phase 5.
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn encode_round_trips() {
        let state = SessionState::new(PathBuf::from("/tmp/x"));
        let encoded = encode(&state).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.identity.session_id, state.identity.session_id);
        assert_eq!(decoded.identity.cwd, state.identity.cwd);
    }

    #[test]
    fn encoded_payload_carries_version_tag() {
        let state = SessionState::default();
        let encoded = encode(&state).unwrap();
        assert!(
            encoded.contains("\"version\""),
            "encoded payload should include the version tag: {encoded}"
        );
        assert!(encoded.contains("\"version\": 1"));
    }

    #[test]
    fn future_schema_is_rejected() {
        // Simulate a file written by a newer harness version.
        let payload = serde_json::json!({
            "version": 999,
            "identity": {
                "session_id": "x",
                "original_cwd": "/x",
                "cwd": "/x",
                "project_root": "/x",
                "session_project_dir": "/x"
            },
            "conversation": {},
            "ui": {},
            "modes": {},
            "permissions": {},
            "budgets": {},
            "transcript": {}
        })
        .to_string();

        match decode(&payload) {
            Err(PersistError::FutureSchema { found, supported }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, 1);
            }
            other => panic!("expected FutureSchema, got {other:?}"),
        }
    }

    #[test]
    fn missing_version_decodes_as_zero() {
        // A blob without the `version` tag is the legacy shape —
        // Phase 5's migration path lives on the version=0 branch.
        let payload = serde_json::json!({
            "identity": {
                "session_id": "legacy-id",
                "original_cwd": "/x",
                "cwd": "/x",
                "project_root": "/x",
                "session_project_dir": "/x"
            },
            "conversation": {},
            "ui": {},
            "modes": {},
            "permissions": {},
            "budgets": {},
            "transcript": {}
        })
        .to_string();
        let peek: VersionPeek = serde_json::from_str(&payload).unwrap();
        assert_eq!(peek.version, 0);
    }

    #[test]
    fn malformed_json_is_a_json_error() {
        let err = decode("{not valid").unwrap_err();
        assert!(matches!(err, PersistError::Json(_)));
    }
}
