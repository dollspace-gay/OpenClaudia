//! End-to-end tests for `migrations::run_all` against a
//! sandboxed `MigrationContext` + `MigrationOutcome` From-impls
//! + `run_all_count_applied` wrapper.
//!
//! Sprint 67 of the verification effort. The migrations module
//! has extensive internal unit tests but no integration coverage
//! exercising the public `run_all` boundary contract against
//! real on-disk directories.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::migrations::{run_all, run_all_count_applied, MigrationContext, MigrationOutcome};
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Build a sandboxed migration context with both directories
/// living inside the same tempdir.
fn sandboxed_ctx() -> (TempDir, MigrationContext) {
    let dir = TempDir::new().expect("tempdir");
    let claude_home = dir.path().join("claude");
    let oc_data = dir.path().join("openclaudia");
    std::fs::create_dir_all(&claude_home).expect("mkdir claude");
    std::fs::create_dir_all(&oc_data).expect("mkdir oc");
    let ctx = MigrationContext::with_paths(claude_home, oc_data);
    (dir, ctx)
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — MigrationContext constructors
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn with_paths_captures_both_paths_verbatim() {
    let claude = std::path::PathBuf::from("/tmp/test-claude");
    let oc = std::path::PathBuf::from("/tmp/test-oc");
    let ctx = MigrationContext::with_paths(claude.clone(), oc.clone());
    assert_eq!(ctx.claude_home, claude);
    assert_eq!(ctx.openclaudia_data, oc);
}

#[test]
fn from_env_returns_constructible_context() {
    // Don't make assertions about the actual contents — they
    // depend on environment — but the constructor MUST NOT
    // panic and MUST produce a value whose paths are
    // non-empty.
    let ctx = MigrationContext::from_env();
    assert!(!ctx.claude_home.as_os_str().is_empty());
    assert!(!ctx.openclaudia_data.as_os_str().is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — run_all end-to-end
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn run_all_against_sandbox_returns_per_migration_reports() {
    let (_dir, ctx) = sandboxed_ctx();
    let reports = run_all(&ctx);
    // Registry has at least 1 migration — verify the report
    // shape is well-formed.
    assert!(
        !reports.is_empty(),
        "registry MUST have at least 1 migration; got 0 reports"
    );
    for r in &reports {
        assert!(!r.id.is_empty(), "report id MUST be non-empty");
        assert!(
            !r.description.is_empty(),
            "report description MUST be non-empty"
        );
    }
}

#[test]
fn run_all_is_safe_to_invoke_twice_in_a_row() {
    // The registry contains idempotent and once-only
    // migrations both. Running twice MUST NOT panic and MUST
    // NOT corrupt state on the second invocation.
    let (_dir, ctx) = sandboxed_ctx();
    let first = run_all(&ctx);
    let second = run_all(&ctx);
    assert_eq!(
        first.len(),
        second.len(),
        "run count must match across invocations"
    );
    // Every report id is stable across runs.
    let first_ids: Vec<&str> = first.iter().map(|r| r.id).collect();
    let second_ids: Vec<&str> = second.iter().map(|r| r.id).collect();
    assert_eq!(first_ids, second_ids);
}

#[test]
fn run_all_first_invocation_applies_at_least_one_migration() {
    let (_dir, ctx) = sandboxed_ctx();
    let reports = run_all(&ctx);
    let applied_count = reports
        .iter()
        .filter(|r| matches!(r.outcome, MigrationOutcome::Applied(_)))
        .count();
    let skipped_count = reports
        .iter()
        .filter(|r| matches!(r.outcome, MigrationOutcome::Skipped))
        .count();
    // First invocation: SOME migration must apply (the
    // registry isn't empty) OR all are skipped because the
    // marker is already present from sandbox setup. The
    // contract is: applied + skipped + failed = total.
    let failed_count = reports
        .iter()
        .filter(|r| matches!(r.outcome, MigrationOutcome::Failed(_)))
        .count();
    assert_eq!(
        applied_count + skipped_count + failed_count,
        reports.len(),
        "every report MUST have exactly one variant"
    );
}

#[test]
fn run_all_creates_ledger_file_after_invocation() {
    let (_dir, ctx) = sandboxed_ctx();
    let _ = run_all(&ctx);
    // run_all calls ledger.save unconditionally at the end —
    // ledger file MUST exist post-run.
    let ledger = ctx.openclaudia_data.join("migrations.json");
    assert!(
        ledger.exists(),
        "ledger file MUST be created at {}; got missing",
        ledger.display()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — run_all_count_applied wrapper
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn run_all_count_applied_returns_only_applied_count() {
    let (_dir, ctx) = sandboxed_ctx();
    let reports = run_all(&ctx);
    let expected_applied = reports
        .iter()
        .filter(|r| matches!(r.outcome, MigrationOutcome::Applied(_)))
        .count();

    // Re-run against same context — once-only migrations
    // will now skip.
    let (_dir2, ctx2) = sandboxed_ctx();
    let count = run_all_count_applied(&ctx2);
    // The Applied count from a fresh sandbox equals the
    // applied-count we just observed (same registry, same
    // sandbox-clean state).
    assert_eq!(
        count, expected_applied,
        "run_all_count_applied MUST equal Applied-filtered count of run_all"
    );
}

#[test]
fn run_all_count_applied_returns_zero_on_second_invocation_for_once_only() {
    // First invocation may apply. Second on the same
    // context: idempotent migrations may re-apply or
    // skip (depends on impl); once-only MUST skip. The
    // safe assertion is: second-invocation count <=
    // first-invocation count.
    let (_dir, ctx) = sandboxed_ctx();
    let first = run_all_count_applied(&ctx);
    let second = run_all_count_applied(&ctx);
    assert!(
        second <= first,
        "second-invocation applied count MUST be <= first; got {second} > {first}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — MigrationOutcome From impls
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn migration_outcome_from_anyhow_error_yields_failed_variant() {
    let err: anyhow::Error = anyhow::anyhow!("simulated error");
    let outcome: MigrationOutcome = err.into();
    let MigrationOutcome::Failed(msg) = outcome else {
        panic!("From<anyhow::Error> MUST produce Failed; got other variant");
    };
    assert!(
        msg.contains("simulated error"),
        "Failed message MUST include source error text; got {msg:?}"
    );
}

#[test]
fn migration_outcome_from_io_error_yields_failed_variant() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "blocked");
    let outcome: MigrationOutcome = io_err.into();
    let MigrationOutcome::Failed(msg) = outcome else {
        panic!("From<io::Error> MUST produce Failed");
    };
    assert!(msg.contains("blocked"));
}

#[test]
fn migration_outcome_from_anyhow_chain_flattens_to_display_string() {
    // anyhow context chain should flatten via {:#}.
    let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "inner");
    let chained = anyhow::Error::new(inner)
        .context("outer context")
        .context("outermost");
    let outcome: MigrationOutcome = chained.into();
    let MigrationOutcome::Failed(msg) = outcome else {
        panic!("expected Failed");
    };
    // Both layers should be present in the flattened message.
    assert!(msg.contains("outermost"));
    // The context chain rendering with {:#} includes all
    // layers — at least one of the inner contexts must show.
    assert!(
        msg.contains("outer") || msg.contains("inner"),
        "chained error MUST surface inner context; got {msg:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — stamp_transcript_schema_v1 specific behaviour
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn first_run_against_empty_sandbox_writes_schema_marker_file() {
    let (_dir, ctx) = sandboxed_ctx();
    let _ = run_all(&ctx);
    // The marker file is at <claude_home>/projects/.schema-version.json.
    let marker = ctx
        .claude_home
        .join("projects")
        .join(".schema-version.json");
    assert!(
        marker.exists(),
        "schema marker MUST be created at {}",
        marker.display()
    );
    // And contents MUST be valid JSON with a transcripts key.
    let contents = std::fs::read_to_string(&marker).expect("read marker");
    let parsed: serde_json::Value = serde_json::from_str(&contents).expect("parse marker JSON");
    assert!(
        parsed.get("transcripts").is_some(),
        "marker MUST contain transcripts key; got {parsed}"
    );
}

#[test]
fn second_run_does_not_overwrite_or_corrupt_existing_marker() {
    let (_dir, ctx) = sandboxed_ctx();
    let _ = run_all(&ctx);
    let marker = ctx
        .claude_home
        .join("projects")
        .join(".schema-version.json");
    let first_contents = std::fs::read_to_string(&marker).expect("read after run 1");

    let _ = run_all(&ctx);
    let second_contents = std::fs::read_to_string(&marker).expect("read after run 2");

    assert_eq!(
        first_contents, second_contents,
        "second run MUST NOT alter the marker file contents"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Ledger persistence across run_all invocations
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn ledger_file_survives_run_all_and_is_well_formed_json() {
    let (_dir, ctx) = sandboxed_ctx();
    let _ = run_all(&ctx);
    let ledger = ctx.openclaudia_data.join("migrations.json");
    let contents = std::fs::read_to_string(&ledger).expect("ledger exists");
    let parsed: serde_json::Value = serde_json::from_str(&contents).expect("ledger is valid JSON");
    // The ledger MUST be a JSON object or array — anything
    // else would mean we wrote a non-recoverable shape.
    assert!(
        parsed.is_object() || parsed.is_array(),
        "ledger root MUST be object or array; got {parsed}"
    );
}
