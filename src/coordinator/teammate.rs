//! Teammate lifecycle state.
//!
//! Phase 1 ships the types + color allocator + state-transition
//! rules. Phase 2 wires `spawn` / `join` via `subagent::run_subagent`.
//! Keeping the state machine out of the spawn path now means Phase 2
//! can reuse this module unchanged.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::subagent::AgentType;

/// Teammate id — opaque UUID-shaped string. Separate from
/// `SessionId` / `TaskId` so call sites can't confuse them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TeammateId(String);

impl TeammateId {
    /// Generate a fresh v4 UUID.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TeammateId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TeammateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Fixed 7-color palette for teammate display — matches Claude
/// Code's rainbow order so transcripts viewed in either harness
/// color-code identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Indigo,
    Violet,
}

impl AgentColor {
    /// Colors in display order. Allocation wraps around after the
    /// 7th teammate — two teammates sharing a color is acceptable
    /// since their id prefix is also shown.
    pub const PALETTE: &'static [Self] = &[
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Indigo,
        Self::Violet,
    ];

    /// Pick a color for the `n`th teammate. Round-robin through
    /// [`Self::PALETTE`].
    #[must_use]
    pub fn for_index(n: usize) -> Self {
        Self::PALETTE[n % Self::PALETTE.len()]
    }
}

/// Lifecycle state. Transitions are one-way:
/// `Spawning → Running → Idle → Dead` and `Running → Dead` directly.
#[derive(Debug, Clone)]
pub enum TeammateState {
    /// Task created but the subagent hasn't responded yet.
    Spawning,
    /// Actively processing prompts / tool calls.
    Running,
    /// Finished its assigned task; waiting for the next.
    Idle,
    /// Finished with an error or the coordinator killed it.
    Dead(String),
}

impl TeammateState {
    /// True when the teammate can still be given new work.
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        matches!(self, Self::Spawning | Self::Running | Self::Idle)
    }

    /// True only when the teammate is ready to accept another task.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Per-teammate bookkeeping the coordinator uses to route tasks
/// and aggregate results. Owns no `Arc` handles — those live on
/// [`super::Coordinator`] and are passed per-dispatch.
#[derive(Debug, Clone)]
pub struct Teammate {
    pub id: TeammateId,
    pub agent_type: AgentType,
    pub color: AgentColor,
    pub state: TeammateState,
    /// Subagent session id — feeds through to
    /// `tools::SessionIdGuard` (crosslink #518) so this teammate's
    /// task-list bucket stays isolated from other teammates.
    pub session_id: String,
    /// Absolute path to this teammate's JSONL transcript —
    /// leverages `crate::transcript` (crosslink #516) so it's
    /// resumable.
    pub transcript_path: PathBuf,
}

impl Teammate {
    /// Build a fresh teammate in `Spawning` state. Colors rotate
    /// through the fixed palette; caller supplies the ordinal.
    #[must_use]
    pub fn new(
        agent_type: AgentType,
        ordinal: usize,
        session_id: impl Into<String>,
        transcript_path: PathBuf,
    ) -> Self {
        Self {
            id: TeammateId::new(),
            agent_type,
            color: AgentColor::for_index(ordinal),
            state: TeammateState::Spawning,
            session_id: session_id.into(),
            transcript_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_exhausts_before_repeating() {
        let colors: Vec<_> = (0..AgentColor::PALETTE.len())
            .map(AgentColor::for_index)
            .collect();
        // All 7 must be distinct.
        let unique: std::collections::HashSet<_> = colors.iter().copied().collect();
        assert_eq!(unique.len(), AgentColor::PALETTE.len());
    }

    #[test]
    fn palette_wraps_after_seven() {
        let first = AgentColor::for_index(0);
        let eighth = AgentColor::for_index(7);
        // 8th teammate reuses the first slot — documented behavior.
        assert_eq!(first, eighth);
    }

    #[test]
    fn state_transitions_match_availability_semantics() {
        assert!(TeammateState::Spawning.is_alive());
        assert!(!TeammateState::Spawning.is_available());

        assert!(TeammateState::Running.is_alive());
        assert!(!TeammateState::Running.is_available());

        assert!(TeammateState::Idle.is_alive());
        assert!(TeammateState::Idle.is_available());

        let dead = TeammateState::Dead("crashed".into());
        assert!(!dead.is_alive());
        assert!(!dead.is_available());
    }

    #[test]
    fn teammate_ids_are_unique() {
        let a = TeammateId::new();
        let b = TeammateId::new();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 36);
    }

    #[test]
    fn teammate_new_starts_in_spawning() {
        let tm = Teammate::new(
            AgentType::Explore,
            0,
            "session-123",
            PathBuf::from("/tmp/t.jsonl"),
        );
        assert_eq!(tm.color, AgentColor::Red);
        assert!(matches!(tm.state, TeammateState::Spawning));
        assert!(!tm.state.is_available());
    }
}

/// Phase 2 spec pins — #532 behavioral contracts for [`Teammate`] /
/// [`AgentColor`] / [`TeammateState`].
///
/// B3 pins the struct field invariants set at construction.
/// B4 pins the palette cycling behavior.
/// Tests must not be weakened to accommodate a future refactor —
/// file a gap issue instead.
#[cfg(test)]
mod phase2_spec_pins {
    use super::*;
    use crate::subagent::AgentType;

    // ── B3: Teammate struct field invariants ─────────────────────────

    /// B3a: `session_id` and `transcript_path` are stored exactly as
    /// supplied (#532 B3 field table).
    #[test]
    fn b3_fields_stored_as_supplied() {
        let session = "ses-abc-123";
        let path = PathBuf::from("/var/log/teammate.jsonl");
        let tm = Teammate::new(AgentType::Plan, 1, session, path.clone());

        assert_eq!(tm.session_id, session, "session_id must be stored verbatim");
        assert_eq!(
            tm.transcript_path, path,
            "transcript_path must be stored verbatim",
        );
    }

    /// B3b: `agent_type` is stored as supplied (#532 B3).
    #[test]
    fn b3_agent_type_stored_as_supplied() {
        let tm = Teammate::new(AgentType::GeneralPurpose, 0, "s", PathBuf::from("/t"));
        assert!(
            matches!(tm.agent_type, AgentType::GeneralPurpose),
            "agent_type must round-trip through Teammate::new",
        );
    }

    /// B3c: id is a UUID v4 string (36 chars, 4 hyphens) — never
    /// empty, never the same across two calls (#532 B3).
    #[test]
    fn b3_teammate_id_is_unique_uuid() {
        let a = Teammate::new(AgentType::Explore, 0, "s1", PathBuf::from("/a"));
        let b = Teammate::new(AgentType::Explore, 0, "s2", PathBuf::from("/b"));

        assert_ne!(a.id, b.id, "each Teammate must receive a unique id");
        // UUID v4 canonical text form is always 36 characters.
        assert_eq!(
            a.id.as_str().len(),
            36,
            "TeammateId must be 36-char UUID string",
        );
        assert_eq!(
            a.id.as_str().chars().filter(|&c| c == '-').count(),
            4,
            "UUID must contain exactly 4 hyphens",
        );
    }

    /// B3d: initial state is Spawning — not Running, not Idle
    /// (#532 B3 lifecycle table).
    #[test]
    fn b3_initial_state_is_spawning_not_running_or_idle() {
        let tm = Teammate::new(AgentType::Guide, 3, "s", PathBuf::from("/t"));
        assert!(
            matches!(tm.state, TeammateState::Spawning),
            "Teammate::new must produce TeammateState::Spawning",
        );
        assert!(tm.state.is_alive(), "Spawning must be alive");
        assert!(!tm.state.is_available(), "Spawning must not be available");
    }

    /// B3e: only Idle satisfies `is_available`; all other alive states
    /// do not (#532 B3 `is_available` contract).
    #[test]
    fn b3_is_available_only_for_idle() {
        assert!(!TeammateState::Spawning.is_available());
        assert!(!TeammateState::Running.is_available());
        assert!(TeammateState::Idle.is_available());
        assert!(!TeammateState::Dead("reason".into()).is_available());
    }

    /// B3f: Dead is the only state where `is_alive` returns false
    /// (#532 B3 `is_alive` contract).
    #[test]
    fn b3_is_alive_false_only_for_dead() {
        assert!(TeammateState::Spawning.is_alive());
        assert!(TeammateState::Running.is_alive());
        assert!(TeammateState::Idle.is_alive());
        assert!(!TeammateState::Dead(String::new()).is_alive());
    }

    // ── B4: AgentColor palette cycling ──────────────────────────────

    /// B4a: explicit slot-by-slot mapping for all 7 palette positions
    /// (#532 B4 contract table).
    #[test]
    fn b4_palette_slot_by_slot() {
        assert_eq!(AgentColor::for_index(0), AgentColor::Red);
        assert_eq!(AgentColor::for_index(1), AgentColor::Orange);
        assert_eq!(AgentColor::for_index(2), AgentColor::Yellow);
        assert_eq!(AgentColor::for_index(3), AgentColor::Green);
        assert_eq!(AgentColor::for_index(4), AgentColor::Blue);
        assert_eq!(AgentColor::for_index(5), AgentColor::Indigo);
        assert_eq!(AgentColor::for_index(6), AgentColor::Violet);
    }

    /// B4b: palette length is exactly 7 (#532 B4 invariant).
    #[test]
    fn b4_palette_len_is_seven() {
        assert_eq!(AgentColor::PALETTE.len(), 7);
    }

    /// B4c: `for_index(n` % 7) == `for_index(n)` for representative
    /// values (#532 B4 invariant).
    #[test]
    fn b4_for_index_modular_identity() {
        for n in [0usize, 7, 14, 100, 1_000_007] {
            assert_eq!(
                AgentColor::for_index(n),
                AgentColor::for_index(n % 7),
                "for_index({n}) != for_index({n} % 7)",
            );
        }
    }

    /// B4d: `usize::MAX` does not panic (#532 B4 no-OOB invariant).
    #[test]
    fn b4_usize_max_does_not_panic() {
        // Just calling it is the assertion — no panic == pass.
        let _ = AgentColor::for_index(usize::MAX);
    }

    /// B4e: `AgentColor` serializes to lowercase strings per serde attr
    /// (#532 B4 serde round-trip).
    #[test]
    fn b4_serde_round_trip_lowercase() {
        let cases = [
            (AgentColor::Red, "\"red\""),
            (AgentColor::Orange, "\"orange\""),
            (AgentColor::Yellow, "\"yellow\""),
            (AgentColor::Green, "\"green\""),
            (AgentColor::Blue, "\"blue\""),
            (AgentColor::Indigo, "\"indigo\""),
            (AgentColor::Violet, "\"violet\""),
        ];
        for (color, expected_json) in cases {
            let serialized = serde_json::to_string(&color).expect("AgentColor must serialize");
            assert_eq!(
                serialized, expected_json,
                "AgentColor::{color:?} must serialize to {expected_json}",
            );
            let round: AgentColor =
                serde_json::from_str(&serialized).expect("AgentColor must deserialize");
            assert_eq!(round, color, "round-trip failed for {color:?}");
        }
    }

    /// B4f: color assigned via `Teammate::new` matches `for_index(ordinal)`
    /// (#532 B3 field table: color set via `AgentColor::for_index`).
    #[test]
    fn b4_teammate_color_matches_for_index() {
        for ordinal in 0..14usize {
            let tm = Teammate::new(AgentType::Explore, ordinal, "s", PathBuf::from("/t"));
            assert_eq!(
                tm.color,
                AgentColor::for_index(ordinal),
                "ordinal {ordinal}: Teammate color must equal AgentColor::for_index(ordinal)",
            );
        }
    }
}
