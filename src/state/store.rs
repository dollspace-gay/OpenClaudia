//! [`StateStore`] — the clone-cheap handle every caller takes.
//!
//! Wraps `SessionState` in `Arc<tokio::sync::RwLock<…>>` plus a
//! `tokio::sync::broadcast` channel so subscribers get notified of
//! each category-level change. Mutations go through
//! [`StateWriteGuard`], which emits an event on drop so it's
//! impossible to forget the notification.
//!
//! ### Why `tokio::sync::RwLock` and not `std::sync::RwLock`
//!
//! Earlier revisions used `std::sync::RwLock` with documentation
//! warning callers never to hold the guard across `.await`. That
//! contract is unenforceable at compile time — a single mishandled
//! `.await` while holding the guard deadlocks the tokio runtime
//! (the worker thread parks waiting for itself) and there is no
//! warning. Switching to `tokio::sync::RwLock` makes guard
//! acquisition an `.await` point (`read().await`, `write().await`)
//! that yields cooperatively on contention. Guards may now be held
//! across other `.await`s without deadlocking the runtime — only
//! suspending the caller until the lock is free.
//!
//! Note that `tokio::sync::RwLock` is *not* poison-on-panic. A panic
//! mid-mutation releases the lock cleanly. The `Drop` impl on
//! [`StateWriteGuard`] still fires on unwind, so the event stream
//! stays coherent.
//!
//! Acquiring the write guard remains a fallible *async* operation
//! only in the sense that it `.await`s — once awaited it cannot
//! fail, so the public API returns the guard directly rather than
//! `Result`.

use std::sync::Arc;

use tokio::sync::{broadcast, RwLock, RwLockReadGuard, RwLockWriteGuard};

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
/// `Arc<tokio::sync::RwLock<…>>` — tests pass it around freely. The
/// `events` sender gets cloned on `subscribe()`; a subscriber that
/// drops stops receiving silently.
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
    /// lock while they work.
    ///
    /// `tokio::sync::RwLock` is not poison-on-panic, so this is
    /// infallible (the std-RwLock revision returned `Option` to
    /// signal a poisoned lock — that failure mode no longer exists).
    pub async fn snapshot(&self) -> SessionState {
        self.inner.read().await.clone()
    }

    /// Read accessor. Use for field reads; may be held across
    /// `.await` thanks to `tokio::sync::RwLock`'s cooperative
    /// semantics.
    pub async fn read(&self) -> RwLockReadGuard<'_, SessionState> {
        self.inner.read().await
    }

    /// Mutation guard. The returned guard dereferences to `&mut SessionState`.
    ///
    /// On drop it emits the accumulated events via the broadcast channel. Call
    /// [`StateWriteGuard::note`] from inside the scope to record what changed.
    ///
    /// Unlike the old `std::sync::RwLock` revision, this guard may
    /// safely span `.await` points — contention suspends the caller
    /// instead of deadlocking the runtime.
    pub async fn write(&self) -> StateWriteGuard<'_> {
        let inner = self.inner.write().await;
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

/// Mutation guard returned from [`StateStore::write`].
///
/// Record changes via [`Self::note`] — the guard flushes every noted event to
/// subscribers when it drops. Drop runs on panic too, so the event stream stays
/// coherent even if a mutation handler aborts mid-way.
///
/// `Drop` is synchronous (Rust limitation), but tokio's broadcast
/// `Sender::send` is a non-blocking sync call — no async context is
/// required for the flush.
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
        // crosslink #884: skip the flush during panic unwinding.
        // A panicking mutation handler typically left the SessionState
        // in a half-mutated shape; broadcasting the partial event
        // stream to subscribers would make them act on torn state. We
        // drop the queued events on the floor — the panic itself is
        // the operator-visible signal and the runtime will tear down
        // the task. Outside of panics, behaviour is unchanged.
        if std::thread::panicking() {
            self.pending.clear();
            return;
        }
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
            let mut guard = store.write().await;
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
            let mut guard = store.write().await;
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
            let mut guard = store.write().await;
            guard.note(StateEvent::Cleared);
        }
        // No assertion — just not panicking is the contract.
    }

    #[tokio::test]
    async fn snapshot_clones_state() {
        let store = StateStore::default();
        store.write().await.budgets.effort_level = EffortLevel::High;

        let snap = store.snapshot().await;
        assert_eq!(snap.budgets.effort_level, EffortLevel::High);

        // Subsequent writes don't affect the snapshot.
        store.write().await.budgets.effort_level = EffortLevel::Low;
        assert_eq!(snap.budgets.effort_level, EffortLevel::High);
    }

    #[tokio::test]
    async fn store_is_clone_shared_state() {
        let a = StateStore::default();
        let b = a.clone();

        a.write()
            .await
            .conversation
            .messages
            .push(json!({"role": "user"}));

        // b sees the same mutation — Arc semantics.
        assert_eq!(b.read().await.conversation.messages.len(), 1);
    }

    #[tokio::test]
    async fn subscribers_after_write_miss_prior_events() {
        // Documents the tokio broadcast semantic — late subscribers
        // do NOT see backlogged events, only future ones. If a
        // subscriber needs full history it must call snapshot()
        // first then subscribe.
        let store = StateStore::default();
        {
            let mut guard = store.write().await;
            guard.note(StateEvent::Cleared);
        }

        // Late subscriber.
        let mut rx = store.subscribe();
        {
            let mut guard = store.write().await;
            guard.note(StateEvent::PermissionsMutated);
        }

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, StateEvent::PermissionsMutated));
    }

    // ---------- Deadlock-under-contention regression tests (#722) ----------
    //
    // These exist specifically to prove the bug the issue describes
    // is gone: holding a guard across `.await` while another task
    // contends for the lock used to deadlock the single-threaded
    // runtime under `std::sync::RwLock`. With `tokio::sync::RwLock`
    // the second task suspends until the first releases, so the test
    // completes within a finite timeout instead of hanging.

    #[tokio::test(flavor = "current_thread")]
    async fn write_guard_held_across_await_does_not_deadlock_runtime() {
        use std::time::Duration;
        use tokio::time::timeout;

        let store = StateStore::default();
        let other = store.clone();

        // Task A: acquire write, yield to runtime while holding it,
        // then mutate and drop. With std::sync::RwLock on a
        // single-thread runtime, the `.await` here would let task B
        // run, B would block forever on `write()`, and B blocks A
        // from being polled again — classic deadlock.
        let task_a = tokio::spawn(async move {
            let mut guard = store.write().await;
            tokio::task::yield_now().await;
            guard.budgets.effort_level = EffortLevel::High;
            guard.note(StateEvent::EffortChanged {
                new: EffortLevel::High,
            });
            // drop here releases the lock, B proceeds.
        });

        // Task B: wait briefly so A wins the lock first, then
        // attempt to acquire write. Under tokio::sync::RwLock B
        // suspends cooperatively until A drops.
        let task_b = tokio::spawn(async move {
            tokio::task::yield_now().await;
            let mut guard = other.write().await;
            guard.budgets.effort_level = EffortLevel::Low;
        });

        // 5s is generous; the test runs in milliseconds when fixed.
        timeout(Duration::from_secs(5), async {
            task_a.await.unwrap();
            task_b.await.unwrap();
        })
        .await
        .expect("deadlock — guard across .await blocked the runtime");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn many_readers_one_writer_makes_progress() {
        // Hammer the lock with 16 concurrent readers and one writer
        // on a single-thread runtime. Under std::sync::RwLock the
        // writer would starve or the readers would deadlock if any
        // of them yielded mid-guard; under tokio::sync::RwLock the
        // scheduler serializes them and everyone completes.
        use std::time::Duration;
        use tokio::time::timeout;

        let store = StateStore::default();
        let mut handles = Vec::new();

        for i in 0..16 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                let g = s.read().await;
                // Hold guard across an await — this is the exact
                // pattern that deadlocks under std::sync::RwLock on
                // a current-thread runtime.
                tokio::task::yield_now().await;
                let _ = (i, g.identity.session_id.clone());
            }));
        }

        let writer = {
            let s = store.clone();
            tokio::spawn(async move {
                let mut g = s.write().await;
                g.budgets.effort_level = EffortLevel::Medium;
            })
        };

        timeout(Duration::from_secs(5), async {
            for h in handles {
                h.await.unwrap();
            }
            writer.await.unwrap();
        })
        .await
        .expect("reader/writer contention deadlocked the runtime");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_then_await_then_write_serializes_cleanly() {
        // Three writers in flight at once, each yielding mid-guard.
        // Must complete in order without deadlock and the broadcast
        // channel must observe all three notes.
        use std::time::Duration;
        use tokio::time::timeout;

        let store = StateStore::default();
        let mut rx = store.subscribe();

        let mut handles = Vec::new();
        for level in [EffortLevel::Low, EffortLevel::Medium, EffortLevel::High] {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                let mut g = s.write().await;
                tokio::task::yield_now().await;
                g.budgets.effort_level = level;
                g.note(StateEvent::EffortChanged { new: level });
            }));
        }

        timeout(Duration::from_secs(5), async {
            for h in handles {
                h.await.unwrap();
            }
        })
        .await
        .expect("serialized writers deadlocked");

        // All three EffortChanged events flushed.
        let mut seen = 0usize;
        for _ in 0..3 {
            match rx.recv().await.unwrap() {
                StateEvent::EffortChanged { .. } => seen += 1,
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert_eq!(seen, 3);
    }
}
