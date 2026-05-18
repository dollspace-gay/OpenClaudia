//! [`StateStore`] — the clone-cheap handle every caller takes.
//!
//! Wraps `SessionState` in `Arc<RwLock<…>>` plus a
//! `tokio::sync::broadcast` channel so subscribers get notified of
//! each category-level change. Mutations go through
//! [`StateWriteGuard`], which emits an event on drop so it's
//! impossible to forget the notification.
//!
//! Locking rules — follow these or deadlock:
//!
//! 1. Never hold a read guard across an `.await`.
//! 2. Never hold a write guard across an `.await`.
//! 3. If you need multiple independent fields, clone them out and
//!    drop the guard before doing work.
//!
//! The write-guard-on-drop pattern means a panic during mutation
//! still emits the event (`Drop` runs on unwind). Subscribers that
//! care about atomicity should snapshot inside the handler.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tokio::sync::broadcast;

use super::categories::{EffortLevel, SessionId};
use super::SessionState;
use crate::modes::BehaviorMode;

/// Channel capacity for the broadcast of [`StateEvent`]. Must be a
/// power of two (tokio requirement). 64 is enough for the expected
/// event density (one per user turn roughly) without starving slow
/// subscribers into `RecvError::Lagged` on normal workloads.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Granular change events. Subscribers filter by variant — the
/// analytics sink only cares about `SessionSwitched` / `Cleared`,
/// the transcript writer only cares about `MessageAppended`, etc.
#[derive(Debug, Clone)]
pub enum StateEvent {
    /// `SessionState::identity.session_id` changed — a different
    /// session became active.
    SessionSwitched { from: SessionId, to: SessionId },
    /// `SessionState::conversation.messages` grew by at least one
    /// entry. Payload carries the role of the just-appended message
    /// so the transcript writer can skip redundant kind lookups.
    MessageAppended { role: String },
    /// `SessionState::conversation.behavior_mode` changed.
    ModeChanged { new: BehaviorMode },
    /// `SessionState::budgets.effort_level` changed.
    EffortChanged { new: EffortLevel },
    /// Any field inside [`super::PermissionsState`] changed.
    PermissionsMutated,
    /// `SessionState::conversation.messages` was emptied
    /// (matches `/clear`). Distinct from `SessionSwitched` —
    /// same session id, fresh history.
    Cleared,
}

/// Clone-cheap handle to the session state + an event channel.
///
/// `Arc<RwLock<…>>` — tests pass it around freely. The `events`
/// sender gets cloned on `subscribe()`; a subscriber that drops
/// stops receiving silently.
#[derive(Clone)]
pub struct StateStore {
    inner: Arc<RwLock<SessionState>>,
    events: broadcast::Sender<StateEvent>,
}

impl StateStore {
    /// Build a fresh store around `state`. Event channel starts
    /// empty — subscribers added later see only events after they
    /// subscribed (tokio broadcast semantic).
    #[must_use]
    pub fn new(state: SessionState) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(state)),
            events,
        }
    }

    /// Subscribe to state events. The returned receiver yields each
    /// future event in arrival order. When a slow subscriber falls
    /// more than `EVENT_CHANNEL_CAPACITY` behind, tokio's broadcast
    /// drops the oldest and returns `RecvError::Lagged(n)` from
    /// `recv()` — subscribers that can't tolerate drops should pair
    /// this with a full snapshot via [`Self::snapshot`].
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    /// Briefly take a read lock, clone the state, release. Useful
    /// for subscribers that want a full snapshot without holding the
    /// lock while they work. `None` if the lock was poisoned — a
    /// prior writer panicked mid-mutation; callers should treat the
    /// store as dead.
    #[must_use]
    pub fn snapshot(&self) -> Option<SessionState> {
        self.inner.read().ok().map(|g| g.clone())
    }

    /// Read accessor. Panics if the lock is poisoned — see
    /// `snapshot` for a non-panicking alternative. Use for short
    /// field reads; drop before any `.await`.
    pub fn read(&self) -> RwLockReadGuard<'_, SessionState> {
        self.inner.read().expect("state store lock poisoned")
    }

    /// Mutation guard. The returned guard dereferences to
    /// `&mut SessionState`; on drop it emits the accumulated events
    /// via the broadcast channel. Call [`StateWriteGuard::note`]
    /// from inside the scope to record what changed.
    #[must_use] 
    pub fn write(&self) -> StateWriteGuard<'_> {
        let inner = self.inner.write().expect("state store lock poisoned");
        StateWriteGuard {
            inner,
            events: &self.events,
            pending: Vec::new(),
        }
    }
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new(SessionState::default())
    }
}

/// Mutation guard returned from [`StateStore::write`]. Record
/// changes via [`Self::note`] — the guard flushes every noted event
/// to subscribers when it drops. Drop runs on panic too, so the
/// event stream stays coherent even if a mutation handler aborts
/// mid-way.
pub struct StateWriteGuard<'a> {
    inner: RwLockWriteGuard<'a, SessionState>,
    events: &'a broadcast::Sender<StateEvent>,
    pending: Vec<StateEvent>,
}

impl StateWriteGuard<'_> {
    /// Record an event to flush on drop. The guard accumulates
    /// rather than emitting inline so a single logical mutation
    /// that touches multiple fields (e.g. `/clear` wiping messages
    /// AND resetting budgets) emits one batch.
    pub fn note(&mut self, event: StateEvent) {
        self.pending.push(event);
    }
}

impl std::ops::Deref for StateWriteGuard<'_> {
    type Target = SessionState;
    fn deref(&self) -> &SessionState {
        &self.inner
    }
}

impl std::ops::DerefMut for StateWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut SessionState {
        &mut self.inner
    }
}

impl Drop for StateWriteGuard<'_> {
    fn drop(&mut self) {
        for event in self.pending.drain(..) {
            // send() fails when there are zero subscribers — that's
            // fine, the event just has no audience. Don't log — a
            // typical CLI run has no subscribers and we'd spam.
            let _ = self.events.send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn write_guard_flushes_noted_events_on_drop() {
        let store = StateStore::default();
        let mut rx = store.subscribe();

        {
            let mut guard = store.write();
            guard
                .conversation
                .messages
                .push(json!({"role": "user", "content": "hi"}));
            guard.note(StateEvent::MessageAppended {
                role: "user".into(),
            });
            // guard drops here → event flushes.
        }

        let event = rx.recv().await.expect("event flushed");
        assert!(matches!(event, StateEvent::MessageAppended { role } if role == "user"));
    }

    #[tokio::test]
    async fn multiple_notes_emit_in_order() {
        let store = StateStore::default();
        let mut rx = store.subscribe();

        {
            let mut guard = store.write();
            guard.note(StateEvent::EffortChanged {
                new: EffortLevel::High,
            });
            guard.note(StateEvent::PermissionsMutated);
        }

        match rx.recv().await.unwrap() {
            StateEvent::EffortChanged { .. } => {}
            other => panic!("expected EffortChanged first, got {other:?}"),
        }
        match rx.recv().await.unwrap() {
            StateEvent::PermissionsMutated => {}
            other => panic!("expected PermissionsMutated second, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_subscribers_still_succeeds() {
        // send() returns Err with zero subscribers — guard must
        // swallow it rather than panic. Regression guard for the
        // common case where no one has called subscribe() yet.
        let store = StateStore::default();
        {
            let mut guard = store.write();
            guard.note(StateEvent::Cleared);
        }
        // No assertion — just not panicking is the contract.
    }

    #[tokio::test]
    async fn snapshot_clones_state() {
        let store = StateStore::default();
        store.write().budgets.effort_level = EffortLevel::High;

        let snap = store.snapshot().unwrap();
        assert_eq!(snap.budgets.effort_level, EffortLevel::High);

        // Subsequent writes don't affect the snapshot.
        store.write().budgets.effort_level = EffortLevel::Low;
        assert_eq!(snap.budgets.effort_level, EffortLevel::High);
    }

    #[tokio::test]
    async fn store_is_clone_shared_state() {
        let a = StateStore::default();
        let b = a.clone();

        a.write()
            .conversation
            .messages
            .push(json!({"role": "user"}));

        // b sees the same mutation — Arc semantics.
        assert_eq!(b.read().conversation.messages.len(), 1);
    }

    #[tokio::test]
    async fn subscribers_after_write_miss_prior_events() {
        // Documents the tokio broadcast semantic — late subscribers
        // do NOT see backlogged events, only future ones. If a
        // subscriber needs full history it must call snapshot()
        // first then subscribe.
        let store = StateStore::default();
        {
            let mut guard = store.write();
            guard.note(StateEvent::Cleared);
        }

        // Late subscriber.
        let mut rx = store.subscribe();
        {
            let mut guard = store.write();
            guard.note(StateEvent::PermissionsMutated);
        }

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, StateEvent::PermissionsMutated));
    }
}
