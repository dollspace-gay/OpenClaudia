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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
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
        let colors: Vec<_> =
            (0..AgentColor::PALETTE.len()).map(AgentColor::for_index).collect();
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
