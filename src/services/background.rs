//! Background job scheduling — crosslink #168 Phase 1.
//!
//! Implements a scheduling skeleton and one concrete background job:
//! **memory consolidation** (prune expired short-term entries and
//! deduplicate archival memories with identical content).
//!
//! ## Design
//!
//! - [`BackgroundJob`] is the trait every periodic job implements.
//!   A job receives an [`Arc<MemoryDb>`] (the only shared resource
//!   needed for Phase 1) and returns a [`JobOutcome`] describing what
//!   happened.
//! - [`JobScheduler`] holds a list of registered jobs plus a monotonic
//!   clock of when each last ran. Callers invoke [`JobScheduler::tick`]
//!   from whatever driving loop they own (e.g., the idle poller in
//!   the session layer). The scheduler is deliberately **synchronous and
//!   not tokio-aware** — the tick takes < 1 ms for typical databases,
//!   and async wrapping is Phase 2's concern.
//! - [`MemoryConsolidationJob`] is the only concrete job shipped in
//!   Phase 1. It:
//!   1. Prunes expired short-term sessions and activities via
//!      [`MemoryDb::cleanup_expired_short_term`].
//!   2. Deduplicates archival memories whose content is byte-for-byte
//!      identical (keeping the most recently updated copy).
//!
//! ## Phase 2 follow-up
//!
//! See crosslink issue filed alongside this change for:
//! - Auto-documentation maintenance (CLAUDE.md / MEMORY.md writers).
//! - Periodic agent summarization using the coordinator infrastructure.
//! - Async `tokio::spawn`-based dispatch loop so jobs run off the main
//!   thread without blocking the proxy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::memory::MemoryDb;

// ── Outcome ─────────────────────────────────────────────────────────────────

/// What a job accomplished during a single run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    /// Human-readable label identifying which job ran.
    pub job_name: &'static str,
    /// Number of records that were removed or merged.
    pub records_pruned: usize,
    /// Number of records that were deduplicated (merged into canonical).
    pub records_deduped: usize,
}

// ── Trait ────────────────────────────────────────────────────────────────────

/// A periodic background task that operates on the memory database.
///
/// Implementors must be `Send + Sync` so the scheduler can hold
/// them behind `Arc<dyn BackgroundJob>` and share them across thread
/// boundaries (Phase 2 will dispatch via `tokio::spawn`).
///
/// # Errors
///
/// The `run` method returns `anyhow::Result<JobOutcome>`. Transient
/// failures (lock contention, `SQLite` busy) should be surfaced as errors
/// so the scheduler can log them without crashing the host process.
pub trait BackgroundJob: Send + Sync {
    /// Name used in log output and [`JobOutcome::job_name`].
    fn name(&self) -> &'static str;

    /// Execute one pass of this job against `db`. Must finish in bounded
    /// time — the scheduler calls this synchronously on the tick thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the database operation fails.
    fn run(&self, db: &Arc<MemoryDb>) -> Result<JobOutcome>;
}

// ── Memory consolidation job ─────────────────────────────────────────────────

/// Prunes expired short-term memory and deduplicates identical archival entries.
///
/// Runs two passes:
/// 1. **Expiry pass** — delegates to [`MemoryDb::cleanup_expired_short_term`]
///    which deletes sessions and activities older than 48 hours.
/// 2. **Dedup pass** — loads all archival memories, groups them by exact
///    content string, and deletes all but the most-recently-updated copy
///    within each group.
///
/// The dedup pass uses a simple `HashMap<String, (id, updated_at)>` so
/// it is O(n) in memory-entry count. For very large databases (> 10 k
/// entries) a SQL-level dedup (`GROUP BY content HAVING COUNT(*) > 1`)
/// would be faster, but the current database size ceiling in practice is
/// a few hundred rows.
pub struct MemoryConsolidationJob;

impl BackgroundJob for MemoryConsolidationJob {
    fn name(&self) -> &'static str {
        "memory_consolidation"
    }

    fn run(&self, db: &Arc<MemoryDb>) -> Result<JobOutcome> {
        // Pass 1 — prune expired short-term entries.
        let (sessions_pruned, activities_pruned) = db.cleanup_expired_short_term()?;
        let records_pruned = sessions_pruned + activities_pruned;
        tracing::debug!(
            sessions_pruned,
            activities_pruned,
            "memory_consolidation: short-term prune complete"
        );

        // Pass 2 — deduplicate identical archival entries.
        let records_deduped = dedup_archival(db)?;
        tracing::debug!(
            records_deduped,
            "memory_consolidation: archival dedup complete"
        );

        Ok(JobOutcome {
            job_name: self.name(),
            records_pruned,
            records_deduped,
        })
    }
}

/// Remove duplicate archival memory entries that share identical content.
/// Keeps the entry with the latest `updated_at` timestamp; deletes the rest.
/// Returns the count of deleted rows.
fn dedup_archival(db: &Arc<MemoryDb>) -> Result<usize> {
    use std::collections::HashMap;

    // (content → (canonical_id, canonical_updated_at, [duplicate_ids]))
    // We build the map in one list pass to avoid N+1 queries.
    let all = db.memory_list(usize::MAX)?;

    // Group: content → (best_id, best_updated_at, all_ids_in_group)
    let mut groups: HashMap<String, (i64, String, Vec<i64>)> = HashMap::new();
    for entry in all {
        let rec = groups
            .entry(entry.content.clone())
            .or_insert_with(|| (entry.id, entry.updated_at.clone(), vec![entry.id]));
        // Track all ids so we can delete the non-canonical ones.
        if !rec.2.contains(&entry.id) {
            rec.2.push(entry.id);
        }
        // Promote to canonical if this entry is newer.
        if entry.updated_at > rec.1 {
            rec.0 = entry.id;
            rec.1.clone_from(&entry.updated_at);
        }
    }

    let mut deleted = 0_usize;
    for (_content, (canonical_id, _ts, all_ids)) in groups {
        for dup_id in all_ids {
            if dup_id != canonical_id && db.memory_delete(dup_id)? {
                deleted += 1;
            }
        }
    }
    Ok(deleted)
}

// ── Scheduler ────────────────────────────────────────────────────────────────

/// Entry in the scheduler's job table.
struct ScheduledJob {
    job: Arc<dyn BackgroundJob>,
    interval: Duration,
    last_run: Option<Instant>,
}

/// Runs registered [`BackgroundJob`]s on a time-based schedule.
///
/// The scheduler is **synchronous** — callers drive it by calling
/// [`tick`][`JobScheduler::tick`] from their own event / idle loop.
/// This keeps the implementation free of `tokio` dependencies so it
/// compiles in unit-test harnesses that don't start a runtime.
///
/// ```rust
/// use std::sync::Arc;
/// use std::time::Duration;
/// use openclaudia::services::background::{JobScheduler, MemoryConsolidationJob};
/// use openclaudia::memory::MemoryDb;
///
/// let db = Arc::new(MemoryDb::open_for_project(std::path::Path::new("/tmp")).unwrap());
/// let mut sched = JobScheduler::new(Arc::clone(&db));
/// sched.register(Arc::new(MemoryConsolidationJob), ONE_HOUR);
/// // Call `sched.tick()` from your idle loop; it only runs jobs whose
/// // interval has elapsed.
/// let outcomes = sched.tick();
/// ```
pub struct JobScheduler {
    db: Arc<MemoryDb>,
    jobs: Vec<ScheduledJob>,
}

impl JobScheduler {
    /// Create a new scheduler backed by `db`.
    #[must_use]
    pub const fn new(db: Arc<MemoryDb>) -> Self {
        Self {
            db,
            jobs: Vec::new(),
        }
    }

    /// Register a job to run at most once per `interval`.
    /// Jobs are checked in registration order; all due jobs run per
    /// [`tick`][`JobScheduler::tick`] call.
    pub fn register(&mut self, job: Arc<dyn BackgroundJob>, interval: Duration) {
        self.jobs.push(ScheduledJob {
            job,
            interval,
            last_run: None,
        });
    }

    /// Run every job whose interval has elapsed since its last run.
    ///
    /// Jobs that error are logged at `warn` level; their `last_run`
    /// timestamp is still updated so a persistently failing job doesn't
    /// spin-loop on every tick. Returns the outcomes of successful runs.
    pub fn tick(&mut self) -> Vec<JobOutcome> {
        let now = Instant::now();
        let mut outcomes = Vec::new();

        for entry in &mut self.jobs {
            let due = match entry.last_run {
                None => true,
                Some(last) => now.duration_since(last) >= entry.interval,
            };
            if !due {
                continue;
            }

            entry.last_run = Some(now);

            match entry.job.run(&self.db) {
                Ok(outcome) => {
                    tracing::info!(
                        job = outcome.job_name,
                        records_pruned = outcome.records_pruned,
                        records_deduped = outcome.records_deduped,
                        "background job completed"
                    );
                    outcomes.push(outcome);
                }
                Err(err) => {
                    tracing::warn!(
                        job = entry.job.name(),
                        error = %err,
                        "background job failed — will retry after interval"
                    );
                }
            }
        }

        outcomes
    }

    /// How many jobs are registered.
    #[must_use]
    pub const fn job_count(&self) -> usize {
        self.jobs.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// One hour; used by scheduler tests that need a "very long" interval.
    const ONE_HOUR: Duration = Duration::from_hours(1);

    fn make_db(tmp: &TempDir) -> Arc<MemoryDb> {
        Arc::new(MemoryDb::open_for_project(tmp.path()).unwrap())
    }

    // ── BackgroundJob trait ──────────────────────────────────────────────────

    /// The trait object is constructible and callable without a concrete type
    /// in scope — required for the scheduler's `Arc<dyn BackgroundJob>` storage.
    #[test]
    fn background_job_trait_is_object_safe() {
        // `accepts_job` takes a bare `&dyn BackgroundJob`.  If the trait were
        // not object-safe (e.g., a generic associated type or `Self` return)
        // this function would fail to compile.  The body is empty because the
        // assertion is purely a compile-time one: reaching this line without a
        // compiler error proves object safety.
        fn accepts_job(_job: &dyn BackgroundJob) {
            // Compile-time proof only — no runtime assertion needed.
        }
        accepts_job(&MemoryConsolidationJob);
    }

    /// `BackgroundJob` implementors must be `Send + Sync` (required for the
    /// `Arc<dyn BackgroundJob>` stored by the scheduler).
    #[test]
    fn background_job_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryConsolidationJob>();
    }

    // ── MemoryConsolidationJob ───────────────────────────────────────────────

    #[test]
    fn consolidation_job_on_empty_db_succeeds() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let job = MemoryConsolidationJob;
        let outcome = job.run(&db).expect("run on empty db must not fail");
        assert_eq!(outcome.job_name, "memory_consolidation");
        assert_eq!(outcome.records_pruned, 0);
        assert_eq!(outcome.records_deduped, 0);
    }

    #[test]
    fn consolidation_job_name_is_stable() {
        let job = MemoryConsolidationJob;
        assert_eq!(job.name(), "memory_consolidation");
    }

    #[test]
    fn consolidation_prunes_expired_sessions() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);

        // Insert a session with a very old ended_at so it's beyond the
        // 48-hour expiry window.  We inject it via raw SQL since the
        // public API always sets ended_at = datetime('now').
        db.execute_raw(
            "INSERT INTO recent_sessions \
             (session_id, summary, files_modified, issues_worked, started_at, ended_at) \
             VALUES ('old-sess', 'old summary', '', '', \
             datetime('now', '-72 hours'), datetime('now', '-72 hours'))",
        )
        .unwrap();

        let stats_before = db.get_recent_sessions(100).unwrap();
        // The expired session falls outside the query window — confirming
        // the session is genuinely old and will be pruned.
        assert!(
            stats_before.is_empty(),
            "expired session must not appear in get_recent_sessions"
        );

        let outcome = MemoryConsolidationJob.run(&db).unwrap();
        // 1 session + 0 activities pruned.
        assert_eq!(outcome.records_pruned, 1);
    }

    #[test]
    fn consolidation_deduplicates_archival_entries() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);

        // Insert three entries: two with identical content, one unique.
        let id_a = db
            .memory_save("duplicate content", &["tag".to_string()])
            .unwrap();
        let id_b = db
            .memory_save("duplicate content", &["tag".to_string()])
            .unwrap();
        let id_unique = db.memory_save("unique content", &[]).unwrap();

        let outcome = MemoryConsolidationJob.run(&db).unwrap();
        assert_eq!(outcome.records_deduped, 1, "one duplicate must be removed");

        // The canonical entry survives; the other duplicate is gone.
        // 2 survive: one from the dup group + the unique entry.
        let survivor_count = [id_a, id_b, id_unique]
            .iter()
            .filter_map(|&id| db.memory_get(id).unwrap())
            .count();
        assert_eq!(survivor_count, 2);
    }

    #[test]
    fn consolidation_keeps_most_recently_updated_duplicate() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);

        // Insert two rows with the same content but distinct timestamps so
        // we can assert which one survives. `datetime('now')` has 1-second
        // resolution; using raw SQL with explicit offsets guarantees the gap
        // without relying on wall-clock ticks.
        // Crosslink #464 dropped the `tags` column from archival_memory in
        // favour of the `archival_memory_tags` junction table.  These raw
        // inserts therefore name only the columns that still exist on the
        // base table; tag assignment is exercised by the dedicated #464
        // tests in memory.rs.
        db.execute_raw(
            "INSERT INTO archival_memory (content, created_at, updated_at) \
             VALUES ('same content', \
             datetime('now', '-10 seconds'), datetime('now', '-10 seconds'))",
        )
        .unwrap();
        // Capture the id just inserted.
        let all_before = db.memory_list(10).unwrap();
        let id_older = all_before
            .iter()
            .find(|e| e.content == "same content")
            .unwrap()
            .id;

        // Insert the newer duplicate with a strictly later timestamp.
        db.execute_raw(
            "INSERT INTO archival_memory (content, created_at, updated_at) \
             VALUES ('same content', \
             datetime('now'), datetime('now'))",
        )
        .unwrap();
        let all_after = db.memory_list(10).unwrap();
        let id_newer = all_after
            .iter()
            .find(|e| e.content == "same content" && e.id != id_older)
            .unwrap()
            .id;

        let outcome = MemoryConsolidationJob.run(&db).unwrap();
        assert_eq!(outcome.records_deduped, 1);

        // The older entry must be gone; the newer one must survive.
        assert!(
            db.memory_get(id_older).unwrap().is_none(),
            "older dup removed"
        );
        assert!(db.memory_get(id_newer).unwrap().is_some(), "newer dup kept");
    }

    #[test]
    fn consolidation_leaves_unique_entries_intact() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);

        let id1 = db.memory_save("alpha", &[]).unwrap();
        let id2 = db.memory_save("beta", &[]).unwrap();
        let id3 = db.memory_save("gamma", &[]).unwrap();

        let outcome = MemoryConsolidationJob.run(&db).unwrap();
        assert_eq!(outcome.records_deduped, 0);

        assert!(db.memory_get(id1).unwrap().is_some());
        assert!(db.memory_get(id2).unwrap().is_some());
        assert!(db.memory_get(id3).unwrap().is_some());
    }

    // ── JobOutcome ───────────────────────────────────────────────────────────

    #[test]
    fn job_outcome_equality_and_debug() {
        let a = JobOutcome {
            job_name: "x",
            records_pruned: 1,
            records_deduped: 2,
        };
        let b = a.clone();
        assert_eq!(a, b);
        // Debug must not panic.
        let _ = format!("{a:?}");
    }

    // ── JobScheduler ─────────────────────────────────────────────────────────

    #[test]
    fn scheduler_registers_jobs() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let mut sched = JobScheduler::new(Arc::clone(&db));
        assert_eq!(sched.job_count(), 0);
        sched.register(Arc::new(MemoryConsolidationJob), Duration::from_secs(1));
        assert_eq!(sched.job_count(), 1);
    }

    #[test]
    fn scheduler_runs_job_on_first_tick() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let mut sched = JobScheduler::new(Arc::clone(&db));
        sched.register(Arc::new(MemoryConsolidationJob), ONE_HOUR);
        // First tick: job has never run, so it's always due.
        let outcomes = sched.tick();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].job_name, "memory_consolidation");
    }

    #[test]
    fn scheduler_skips_job_before_interval_elapses() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let mut sched = JobScheduler::new(Arc::clone(&db));
        // Very long interval — job won't be due on the second tick.
        sched.register(Arc::new(MemoryConsolidationJob), ONE_HOUR);
        let first = sched.tick();
        assert_eq!(first.len(), 1, "first tick must run the job");

        let second = sched.tick();
        assert!(
            second.is_empty(),
            "second tick must skip job (interval not elapsed)"
        );
    }

    #[test]
    fn scheduler_runs_multiple_jobs_independently() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let mut sched = JobScheduler::new(Arc::clone(&db));
        sched.register(Arc::new(MemoryConsolidationJob), ONE_HOUR);
        sched.register(Arc::new(MemoryConsolidationJob), ONE_HOUR);
        let outcomes = sched.tick();
        assert_eq!(outcomes.len(), 2, "both jobs must run on first tick");
    }

    #[test]
    fn scheduler_with_zero_interval_always_runs() {
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let mut sched = JobScheduler::new(Arc::clone(&db));
        // Zero interval → every tick is due.
        sched.register(Arc::new(MemoryConsolidationJob), Duration::ZERO);
        let first = sched.tick();
        let second = sched.tick();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1, "zero interval means always due");
    }

    // ── Custom job implementation ────────────────────────────────────────────

    /// Verify that user-defined jobs implementing the trait integrate cleanly
    /// with the scheduler — this is the contract third-party callers depend on.
    #[test]
    fn custom_job_integrates_with_scheduler() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingJob {
            runs: Arc<AtomicUsize>,
        }

        impl BackgroundJob for CountingJob {
            fn name(&self) -> &'static str {
                "counting"
            }

            fn run(&self, _db: &Arc<MemoryDb>) -> Result<JobOutcome> {
                self.runs.fetch_add(1, Ordering::SeqCst);
                Ok(JobOutcome {
                    job_name: self.name(),
                    records_pruned: 0,
                    records_deduped: 0,
                })
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let tmp = TempDir::new().unwrap();
        let db = make_db(&tmp);
        let mut sched = JobScheduler::new(Arc::clone(&db));
        sched.register(
            Arc::new(CountingJob {
                runs: Arc::clone(&counter),
            }),
            Duration::ZERO,
        );

        sched.tick();
        sched.tick();
        sched.tick();
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
