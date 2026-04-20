//! Leader permission bridge.
//!
//! Parallel teammates would otherwise race to prompt the user —
//! N simultaneous `[y/n/a/d]?` dialogs collide. The bridge queues
//! incoming permission requests per-teammate and serves them in
//! arrival order so the user sees exactly one prompt at a time.
//! An "always-allow for this run" cache makes `a` replies
//! per-teammate so one teammate can't widen permissions for
//! another.
//!
//! Phase 1 ships the queue data structures + tests. Phase 3 wires
//! the bridge into the event loop as the sole receiver of teammate
//! `PermissionRequest` events.

use std::collections::{HashSet, VecDeque};

use super::teammate::TeammateId;

/// A permission request from a specific teammate, awaiting the
/// leader's decision. The reply channel lets the teammate's task
/// thread resume once the user decides.
pub struct QueuedPermission {
    pub teammate: TeammateId,
    pub tool_name: String,
    pub tool_args: String,
}

impl std::fmt::Debug for QueuedPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueuedPermission")
            .field("teammate", &self.teammate)
            .field("tool_name", &self.tool_name)
            .field("tool_args_len", &self.tool_args.len())
            .finish()
    }
}

/// Permission bridge state. Pure data — the async machinery that
/// actually serves the queue (receive PermissionRequest → push →
/// pop when user replies) lands in Phase 3.
#[derive(Debug, Default)]
pub struct LeaderPermissionBridge {
    /// FIFO of pending prompts.
    pending: VecDeque<QueuedPermission>,
    /// Pairs of (teammate, tool) the user has always-allowed for
    /// this run. Keyed pair → matches CC's "per-teammate `a`
    /// doesn't leak across teammates" behavior.
    always_allowed: HashSet<(TeammateId, String)>,
}

impl LeaderPermissionBridge {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// True when nothing is queued and no prior teammate has an
    /// always-allow cache entry. Used by the idle-state check in
    /// `Coordinator::teammates` tests.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.always_allowed.is_empty()
    }

    /// How many requests are waiting for a decision.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Enqueue a new request. Preserves arrival order.
    pub fn enqueue(&mut self, request: QueuedPermission) {
        self.pending.push_back(request);
    }

    /// Pop the head of the queue. `None` when empty.
    pub fn dequeue(&mut self) -> Option<QueuedPermission> {
        self.pending.pop_front()
    }

    /// Record an "always allow" decision. The pair
    /// `(teammate_id, tool_name)` is marked so future requests
    /// from that teammate for that tool bypass the queue entirely.
    pub fn always_allow(&mut self, teammate: TeammateId, tool_name: impl Into<String>) {
        self.always_allowed.insert((teammate, tool_name.into()));
    }

    /// Check the always-allow cache. True → the request should
    /// skip enqueuing and resolve immediately as `Allow`.
    #[must_use]
    pub fn is_always_allowed(&self, teammate: &TeammateId, tool_name: &str) -> bool {
        // The HashSet requires owned-key lookup; we keep the
        // lookup allocation-free by comparing inside a `iter().any`.
        self.always_allowed
            .iter()
            .any(|(t, tool)| t == teammate && tool == tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(tm: &TeammateId, tool: &str) -> QueuedPermission {
        QueuedPermission {
            teammate: tm.clone(),
            tool_name: tool.into(),
            tool_args: "{}".into(),
        }
    }

    #[test]
    fn fresh_bridge_is_idle() {
        let bridge = LeaderPermissionBridge::new();
        assert!(bridge.is_idle());
        assert_eq!(bridge.pending_count(), 0);
    }

    #[test]
    fn enqueue_preserves_arrival_order() {
        let mut bridge = LeaderPermissionBridge::new();
        let t1 = TeammateId::new();
        let t2 = TeammateId::new();
        bridge.enqueue(make_request(&t1, "bash"));
        bridge.enqueue(make_request(&t2, "write_file"));
        bridge.enqueue(make_request(&t1, "edit_file"));
        assert_eq!(bridge.pending_count(), 3);

        let first = bridge.dequeue().unwrap();
        assert_eq!(first.teammate, t1);
        assert_eq!(first.tool_name, "bash");

        let second = bridge.dequeue().unwrap();
        assert_eq!(second.teammate, t2);

        let third = bridge.dequeue().unwrap();
        assert_eq!(third.teammate, t1);
        assert_eq!(third.tool_name, "edit_file");

        assert!(bridge.dequeue().is_none());
    }

    #[test]
    fn always_allow_is_per_teammate() {
        let mut bridge = LeaderPermissionBridge::new();
        let t1 = TeammateId::new();
        let t2 = TeammateId::new();
        bridge.always_allow(t1.clone(), "bash");

        // t1 + bash hits the cache; t1 + edit_file does not; t2 +
        // bash does NOT — decisions are per-teammate to match CC.
        assert!(bridge.is_always_allowed(&t1, "bash"));
        assert!(!bridge.is_always_allowed(&t1, "edit_file"));
        assert!(!bridge.is_always_allowed(&t2, "bash"));
    }

    #[test]
    fn always_allow_tracks_distinct_tools() {
        let mut bridge = LeaderPermissionBridge::new();
        let tm = TeammateId::new();
        bridge.always_allow(tm.clone(), "bash");
        bridge.always_allow(tm.clone(), "write_file");
        assert!(bridge.is_always_allowed(&tm, "bash"));
        assert!(bridge.is_always_allowed(&tm, "write_file"));
        assert!(!bridge.is_always_allowed(&tm, "edit_file"));
    }

    #[test]
    fn is_idle_reflects_cache_entries_too() {
        let mut bridge = LeaderPermissionBridge::new();
        bridge.always_allow(TeammateId::new(), "bash");
        // Pending is empty but cache isn't — not idle. Matches the
        // semantic used by the default-coordinator-is-empty test
        // in mod.rs.
        assert!(!bridge.is_idle());
    }
}
