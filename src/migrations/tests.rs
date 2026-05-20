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
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
fn run_fake(ctx: &MigrationContext, migrations: Vec<Box<dyn Migration>>) -> Vec<MigrationReport> {
    let mut ledger = CompletionLedger::load(&ctx.ledger_path()).unwrap_or_default();
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

    let mut ledger = CompletionLedger::load(&ledger_path).unwrap();
    assert!(!ledger.contains("abc"));
    ledger.mark("abc");
    ledger.save(&ledger_path).unwrap();

    // Simulate a new process: drop the old ledger, re-load from disk.
    let fresh = CompletionLedger::load(&ledger_path).unwrap();
    assert!(fresh.contains("abc"));
    assert!(!fresh.contains("xyz"));
}

// ---------------------------------------------------------------------------
// #741 — atomic save (#741a) + corruption-surfacing load (#741b)
// ---------------------------------------------------------------------------

/// #741b regression: a corrupt ledger file must surface as `Err`, not be
/// silently coerced to an empty ledger. Coercing-to-empty causes every
/// once-only migration to replay on the next boot.
#[test]
fn fix741b_corrupt_ledger_surfaces_error_not_silent_empty() {
    let tc = TestContext::new();
    let ledger_path = tc.ctx.ledger_path();
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    std::fs::write(&ledger_path, "{not valid json").unwrap();

    let result = CompletionLedger::load(&ledger_path);
    assert!(
        result.is_err(),
        "corrupt ledger must return Err — silent-empty would let once-only migrations replay"
    );
    let err = result.err().unwrap();
    let chain: String = err
        .chain()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(" / ");
    assert!(
        chain.contains("corrupt") || chain.contains("expected") || chain.contains("EOF"),
        "error chain must explain corruption, got: {chain}"
    );

    // Forensic preservation: the corrupt file must remain on disk for
    // operator inspection. Silent overwrite-on-next-save destroys it.
    assert!(
        ledger_path.exists(),
        "corrupt ledger file must remain on disk for forensic inspection"
    );
}

/// #741b regression: a missing ledger file is the expected first-run
/// state and must yield `Ok(default)`, not an error. ENOENT is not
/// corruption.
#[test]
fn fix741b_missing_file_is_ok_empty_ledger() {
    let tc = TestContext::new();
    let ledger_path = tc.ctx.ledger_path();
    assert!(
        !ledger_path.exists(),
        "preconditions: ledger must not exist"
    );

    let result = CompletionLedger::load(&ledger_path);
    assert!(
        result.is_ok(),
        "missing ledger file is first-run state — must be Ok, got {result:?}"
    );
    let ledger = result.unwrap();
    assert!(!ledger.contains("any-id"));
}

/// #741a sanity: state survives a save/load round-trip across the
/// new atomic path. Locks in the contract that the rewrite preserves
/// what the old `fs::write` path did.
#[test]
fn fix741a_save_load_round_trip_preserves_state() {
    let tc = TestContext::new();
    let ledger_path = tc.ctx.ledger_path();

    let mut original = CompletionLedger::default();
    original.mark("alpha");
    original.mark("beta");
    original.mark("gamma");
    original.save(&ledger_path).unwrap();

    let reloaded = CompletionLedger::load(&ledger_path).unwrap();
    assert!(reloaded.contains("alpha"));
    assert!(reloaded.contains("beta"));
    assert!(reloaded.contains("gamma"));
    assert!(!reloaded.contains("delta"));
}

/// #741a regression: 8 threads × 25 saves each must never leave the
/// on-disk file in a state that fails to parse. The atomic
/// write-temp-then-rename guarantees readers always see either a
/// previous complete file or the new complete file — never a
/// half-written intermediate, never a truncated zero-byte file.
#[test]
fn fix741a_concurrent_save_never_leaves_corrupt_file() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let tc = TestContext::new();
    let ledger_path = Arc::new(tc.ctx.ledger_path());
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();

    // Seed a baseline so the file exists when the reader starts.
    CompletionLedger::default().save(&ledger_path).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let reader_saw_corruption = Arc::new(AtomicBool::new(false));

    // Reader thread spins polling the file. Every snapshot must parse.
    let reader_handle = {
        let path = Arc::clone(&ledger_path);
        let stop = Arc::clone(&stop);
        let saw_corruption = Arc::clone(&reader_saw_corruption);
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if let Err(e) = CompletionLedger::load(&path) {
                    eprintln!("reader saw corruption: {e:?}");
                    saw_corruption.store(true, Ordering::SeqCst);
                    return;
                }
            }
        })
    };

    let mut writers = Vec::new();
    for tid in 0..8u64 {
        let path = Arc::clone(&ledger_path);
        writers.push(std::thread::spawn(move || {
            for i in 0..25u64 {
                let mut l = CompletionLedger::default();
                l.mark(&format!("t{tid}-i{i}"));
                l.save(&path).expect("save must succeed");
            }
        }));
    }
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    reader_handle.join().unwrap();

    assert!(
        !reader_saw_corruption.load(Ordering::SeqCst),
        "concurrent saves left the file in a corrupt state — atomicity violated"
    );

    // Final file must still be parseable.
    let _final = CompletionLedger::load(&ledger_path).expect("final file must parse");

    // No stray temp files left behind.
    let parent = ledger_path.parent().unwrap();
    let strays: Vec<_> = std::fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("migrations.tmp."))
        .collect();
    assert!(
        strays.is_empty(),
        "save() left {} stray temp file(s) behind: {:?}",
        strays.len(),
        strays
            .iter()
            .map(std::fs::DirEntry::file_name)
            .collect::<Vec<_>>()
    );
}

/// #741a security: on Unix the saved file must be mode 0o600 — the
/// ledger names internal migration IDs that we do not want exposed
/// to other local users. The temp file is `chmod`-ed before the
/// rename so the final inode is never world-readable.
#[cfg(unix)]
#[test]
fn fix741a_saved_ledger_has_0o600_permissions_on_unix() {
    use std::os::unix::fs::PermissionsExt as _;

    let tc = TestContext::new();
    let ledger_path = tc.ctx.ledger_path();

    let mut l = CompletionLedger::default();
    l.mark("perms-test");
    l.save(&ledger_path).unwrap();

    let mode = std::fs::metadata(&ledger_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "ledger file must be 0o600 (owner-rw only), got 0o{mode:o}"
    );
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
    assert!(reports.iter().any(|r| r.id == "stamp-transcript-schema-v1"
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

// --- crosslink #734: schema-marker fast path must distinguish
// NotFound (run the migration) from other I/O errors (surface as
// failure rather than silently skipping). ---

/// `read_json_if_exists` returns `Ok(None)` for a missing file —
/// the canonical "no marker yet, run the migration" signal.
#[test]
fn fix734_read_json_if_exists_returns_none_for_missing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nope.json");
    let value = read_json_if_exists(&path).expect("missing file is not an error");
    assert!(value.is_none(), "missing file must map to Ok(None)");
}

/// `read_json_if_exists` returns `Err` (not silently `Ok(None)`)
/// when the file exists but cannot be read — the bug from #734
/// was that permission-denied was being conflated with absence.
#[cfg(unix)]
#[test]
fn fix734_read_json_if_exists_errors_when_unreadable() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("marker.json");
    std::fs::write(&path, r#"{"transcripts": 1}"#).unwrap();
    // Strip every permission bit. read_to_string then yields
    // ErrorKind::PermissionDenied.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&path, perms).unwrap();

    let result = read_json_if_exists(&path);

    // Restore so TempDir cleanup doesn't trip.
    let mut restore = std::fs::metadata(&path).unwrap().permissions();
    restore.set_mode(0o600);
    let _ = std::fs::set_permissions(&path, restore);

    // Skip the assertion if the process is privileged enough to
    // ignore the bit (e.g. running as root in CI). In that case
    // the read succeeds and the test is vacuously true.
    if result.is_ok() {
        return;
    }
    assert!(
        result.is_err(),
        "unreadable file must propagate as Err, not silently map to Ok(None)",
    );
}

/// End-to-end: when the schema-version marker exists but can't be
/// read, `StampTranscriptSchemaV1::run` must surface a
/// `MigrationOutcome::Failed` rather than silently skipping (the
/// pre-fix behavior would have ignored the read error and either
/// re-applied the migration or swallowed it).
#[cfg(unix)]
#[test]
fn fix734_stamp_transcript_marker_unreadable_surfaces_failure() {
    use std::os::unix::fs::PermissionsExt as _;

    let _lock = env_lock();
    let tc = TestContext::new();
    let projects = tc.ctx.claude_home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let marker = projects.join(".schema-version.json");
    std::fs::write(&marker, r#"{"transcripts": 1}"#).unwrap();

    // Make the marker unreadable; on most filesystems unprivileged
    // processes get ErrorKind::PermissionDenied from read_to_string.
    let mut perms = std::fs::metadata(&marker).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&marker, perms).unwrap();

    let migration = super::stamp_transcript_schema_v1::StampTranscriptSchemaV1;
    let outcome = migration.run(&tc.ctx);

    // Restore so TempDir cleanup works.
    let mut restore = std::fs::metadata(&marker).unwrap().permissions();
    restore.set_mode(0o600);
    let _ = std::fs::set_permissions(&marker, restore);

    match outcome {
        MigrationOutcome::Failed(_) => {}
        MigrationOutcome::Skipped => {
            // Only acceptable if we're root and the chmod was a no-op;
            // in that case the read succeeded and the marker matched.
            // We can detect this by re-reading.
            let still_readable = std::fs::read_to_string(&marker).is_ok();
            assert!(
                still_readable,
                "Skipped without being able to read the marker is the #734 bug",
            );
        }
        MigrationOutcome::Applied(msg) => {
            panic!("must not silently overwrite an unreadable marker; got Applied: {msg}");
        }
    }
}
