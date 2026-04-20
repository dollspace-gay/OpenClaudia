use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

use super::*;

/// Some migrations (the real one at `stamp_transcript_schema_v1`)
/// resolve paths via `transcript::claude_config_home_dir()`, which
/// reads the `CLAUDE_CONFIG_HOME_DIR` env var. When the `transcript`
/// module's tests run in parallel they flip that var to different
/// temp dirs and race our `run_all` calls. This lock serializes every
/// test in this module with the same env-dependent surface so both
/// test suites stay green under `cargo test -- --test-threads=N`.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct TestContext {
    _claude_home: TempDir,
    _openclaudia_data: TempDir,
    ctx: MigrationContext,
}

impl TestContext {
    fn new() -> Self {
        let claude_home = TempDir::new().unwrap();
        let openclaudia_data = TempDir::new().unwrap();
        let ctx = MigrationContext::with_paths(
            claude_home.path().to_path_buf(),
            openclaudia_data.path().to_path_buf(),
        );
        Self {
            _claude_home: claude_home,
            _openclaudia_data: openclaudia_data,
            ctx,
        }
    }
}

struct FakeIdempotentMigration {
    id: &'static str,
    applied_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Migration for FakeIdempotentMigration {
    fn id(&self) -> &'static str {
        self.id
    }
    fn description(&self) -> &'static str {
        "fake idempotent migration for tests"
    }
    fn run(&self, _ctx: &MigrationContext) -> MigrationOutcome {
        self.applied_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        MigrationOutcome::Applied("ok".to_string())
    }
}

struct FakeOnceOnlyMigration {
    id: &'static str,
    applied_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Migration for FakeOnceOnlyMigration {
    fn id(&self) -> &'static str {
        self.id
    }
    fn description(&self) -> &'static str {
        "fake once-only migration for tests"
    }
    fn run_policy(&self) -> RunPolicy {
        RunPolicy::OnceOnly
    }
    fn run(&self, _ctx: &MigrationContext) -> MigrationOutcome {
        self.applied_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        MigrationOutcome::Applied("did the thing".to_string())
    }
}

/// Drive the runner against a hand-picked list of migrations without
/// going through the real `registry::all()`. Used by the framework
/// tests so we can assert ledger + policy behavior without depending
/// on whatever real migrations exist today.
fn run_fake(
    ctx: &MigrationContext,
    migrations: Vec<Box<dyn Migration>>,
) -> Vec<MigrationReport> {
    let mut ledger = CompletionLedger::load(&ctx.ledger_path());
    let mut out = Vec::new();
    for migration in migrations {
        let id = migration.id();
        let description = migration.description();
        if migration.run_policy() == RunPolicy::OnceOnly && ledger.contains(id) {
            out.push(MigrationReport {
                id,
                description,
                outcome: MigrationOutcome::Skipped,
            });
            continue;
        }
        let outcome = migration.run(ctx);
        if matches!(outcome, MigrationOutcome::Applied(_))
            && migration.run_policy() == RunPolicy::OnceOnly
        {
            ledger.mark(id);
        }
        out.push(MigrationReport {
            id,
            description,
            outcome,
        });
    }
    ledger.save(&ctx.ledger_path()).unwrap();
    out
}

#[test]
fn idempotent_runs_every_time() {
    let tc = TestContext::new();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for _ in 0..3 {
        let m: Vec<Box<dyn Migration>> = vec![Box::new(FakeIdempotentMigration {
            id: "idem-a",
            applied_counter: counter.clone(),
        })];
        run_fake(&tc.ctx, m);
    }
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn once_only_runs_exactly_once() {
    let tc = TestContext::new();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for _ in 0..5 {
        let m: Vec<Box<dyn Migration>> = vec![Box::new(FakeOnceOnlyMigration {
            id: "once-a",
            applied_counter: counter.clone(),
        })];
        run_fake(&tc.ctx, m);
    }
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn ledger_persists_across_processes() {
    let tc = TestContext::new();
    let ledger_path = tc.ctx.ledger_path();

    let mut ledger = CompletionLedger::load(&ledger_path);
    assert!(!ledger.contains("abc"));
    ledger.mark("abc");
    ledger.save(&ledger_path).unwrap();

    // Simulate a new process: drop the old ledger, re-load from disk.
    let fresh = CompletionLedger::load(&ledger_path);
    assert!(fresh.contains("abc"));
    assert!(!fresh.contains("xyz"));
}

#[test]
fn corrupt_ledger_treated_as_empty() {
    let tc = TestContext::new();
    let ledger_path = tc.ctx.ledger_path();
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    std::fs::write(&ledger_path, "{not valid json").unwrap();
    let ledger = CompletionLedger::load(&ledger_path);
    assert!(!ledger.contains("abc"));
}

#[test]
fn stamp_transcript_schema_v1_writes_marker() {
    let _lock = env_lock();
    let tc = TestContext::new();
    let reports = run_all(&tc.ctx);
    let marker = tc
        .ctx
        .claude_home
        .join("projects")
        .join(".schema-version.json");
    assert!(marker.exists(), "marker file not written");
    let text = std::fs::read_to_string(&marker).unwrap();
    assert!(text.contains("\"transcripts\""));
    assert!(text.contains('1'));
    assert!(reports
        .iter()
        .any(|r| r.id == "stamp-transcript-schema-v1"
            && matches!(r.outcome, MigrationOutcome::Applied(_))));
}

#[test]
fn stamp_transcript_schema_v1_is_idempotent() {
    let _lock = env_lock();
    let tc = TestContext::new();
    run_all(&tc.ctx);
    let reports = run_all(&tc.ctx); // second run
    let stamp = reports
        .iter()
        .find(|r| r.id == "stamp-transcript-schema-v1")
        .unwrap();
    assert!(matches!(stamp.outcome, MigrationOutcome::Skipped));
}

#[test]
fn context_from_env_is_constructible() {
    // Smoke test: the real constructor shouldn't panic even in
    // sandbox environments without a home dir.
    let _lock = env_lock();
    let ctx = MigrationContext::from_env();
    assert!(!ctx.claude_home.as_os_str().is_empty());
    assert!(!ctx.openclaudia_data.as_os_str().is_empty());
    // ledger_path() must always return a buildable path.
    let _: PathBuf = ctx.ledger_path();
}
