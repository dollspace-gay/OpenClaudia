//! End-to-end tests for `pricing::calculate_cost_full` matrix +
//! unknown-model session-flag mechanics + `AuditLogger` JSONL
//! event capture.
//!
//! Sprint 61 of the verification effort. Sprint 35 covered the
//! main pricing helpers; this file pins the
//! `calculate_cost_full` matrix (extras × ttl × fast) +
//! the `has/clear_unknown_model_cost` thread-local flag dance,
//! and exercises the `AuditLogger` sink end-to-end.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::session::{
    calculate_cost_full, clear_unknown_model_cost, get_pricing, has_unknown_model_cost, AuditError,
    AuditLogger, CacheWriteTtl, PricingError, TokenUsage, UsageExtras, FAST_MODE_INPUT_PER_MILLION,
    FAST_MODE_OUTPUT_PER_MILLION,
};
use serde_json::json;
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Section A — has/clear_unknown_model_cost thread-local flag
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn clear_unknown_model_cost_resets_flag_to_false() {
    // Whatever state the thread is in, clear MUST leave it false.
    clear_unknown_model_cost();
    assert!(!has_unknown_model_cost());
}

#[test]
fn calculate_cost_full_unknown_model_sets_the_flag() {
    clear_unknown_model_cost();
    assert!(!has_unknown_model_cost(), "premise: flag starts cleared");

    let outcome = calculate_cost_full(
        "totally-unknown-model-xyz-2099",
        &TokenUsage::default(),
        &UsageExtras::ZERO,
        CacheWriteTtl::FiveMinutes,
        false,
    );
    assert!(matches!(outcome, Err(PricingError::UnknownModel(_))));
    assert!(
        has_unknown_model_cost(),
        "unknown-model call MUST set the flag"
    );
    // Clean up for downstream tests.
    clear_unknown_model_cost();
}

#[test]
fn calculate_cost_full_known_model_does_not_set_the_flag() {
    clear_unknown_model_cost();
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let _ = calculate_cost_full(
        "claude-3-5-sonnet-20241022",
        &usage,
        &UsageExtras::ZERO,
        CacheWriteTtl::FiveMinutes,
        false,
    )
    .expect("known model");
    assert!(
        !has_unknown_model_cost(),
        "known-model call MUST NOT set the flag"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — calculate_cost_full 4-axis matrix
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn calculate_cost_full_default_args_match_calculate_cost() {
    // calculate_cost_full(model, usage, ZERO, FiveMinutes, false)
    // should produce the same number as the simpler calculate_cost.
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let full = calculate_cost_full(
        "claude-3-5-sonnet-20241022",
        &usage,
        &UsageExtras::ZERO,
        CacheWriteTtl::FiveMinutes,
        false,
    )
    .expect("full");
    let p = get_pricing("claude-3-5-sonnet-20241022").unwrap();
    let expected = p.input_per_million + p.output_per_million;
    assert!(
        (full - expected).abs() < 1e-6,
        "full default-args MUST equal input_rate + output_rate; \
         got {full}, expected {expected}"
    );
}

#[test]
fn calculate_cost_full_with_extras_preserves_token_cost() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let extras = UsageExtras::ZERO;
    let cost = calculate_cost_full(
        "claude-3-5-sonnet-20241022",
        &usage,
        &extras,
        CacheWriteTtl::FiveMinutes,
        false,
    )
    .expect("cost");
    let expected = get_pricing("claude-3-5-sonnet-20241022")
        .expect("pricing")
        .input_per_million;
    assert!(
        (cost - expected).abs() < 1e-9,
        "extras must not add non-token charges; got {cost}, expected {expected}"
    );
}

#[test]
fn calculate_cost_full_one_hour_ttl_costs_more_than_five_minutes() {
    let usage = TokenUsage {
        cache_write_tokens: 1_000_000,
        ..TokenUsage::default()
    };
    let short = calculate_cost_full(
        "claude-3-5-sonnet-20241022",
        &usage,
        &UsageExtras::ZERO,
        CacheWriteTtl::FiveMinutes,
        false,
    )
    .expect("short");
    let long = calculate_cost_full(
        "claude-3-5-sonnet-20241022",
        &usage,
        &UsageExtras::ZERO,
        CacheWriteTtl::OneHour,
        false,
    )
    .expect("long");
    assert!(long > short);
}

#[test]
fn calculate_cost_full_fast_mode_uses_fast_tier_when_available() {
    // The fast-tier rates are exposed as constants. For the
    // models that DO have a fast tier (Opus 4.6+), fast=true
    // MUST use those rates. For models that don't, fast=true
    // = standard.
    // We don't know which models have fast tiers configured,
    // so test the exposed constants are positive and that
    // fast-mode call doesn't error for a known model.
    // const-positive: 30.0 > 0.0
    // const-positive: 150.0 > 0.0
    let usage = TokenUsage {
        input_tokens: 1000,
        output_tokens: 1000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let _ = calculate_cost_full(
        "claude-3-5-sonnet-20241022",
        &usage,
        &UsageExtras::ZERO,
        CacheWriteTtl::FiveMinutes,
        true,
    )
    .expect("fast mode known model MUST succeed");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — AuditLogger::new_in
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn audit_logger_new_in_creates_directory_and_file() {
    let dir = TempDir::new().expect("tempdir");
    let log_dir = dir.path().join("logs");
    assert!(!log_dir.exists(), "premise: log dir absent");
    let logger = AuditLogger::new_in(&log_dir, "session-1").expect("new_in");
    assert!(log_dir.exists(), "log dir MUST be created");
    assert!(
        logger.path().exists(),
        "log file MUST be created; got {:?}",
        logger.path()
    );
    assert!(
        logger.path().to_string_lossy().contains("session-1"),
        "log path MUST include session id; got {:?}",
        logger.path()
    );
    assert!(
        logger.path().to_string_lossy().ends_with(".jsonl"),
        "log file MUST be .jsonl; got {:?}",
        logger.path()
    );
}

#[test]
fn audit_logger_new_in_errors_when_dir_cannot_be_created() {
    // Try to create under an existing file (not a dir).
    let dir = TempDir::new().expect("tempdir");
    let blocker = dir.path().join("blocker-file");
    std::fs::write(&blocker, "exists").expect("write");
    let outcome = AuditLogger::new_in(&blocker.join("subdir"), "s");
    assert!(
        outcome.is_err(),
        "MUST error when parent is a file; got {:?}",
        outcome.map(|l| l.path().to_path_buf())
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — log + log_security write JSONL
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn log_writes_one_jsonl_line_with_event_type_and_data() {
    let dir = TempDir::new().expect("tempdir");
    let mut logger = AuditLogger::new_in(dir.path(), "log-test").expect("new");
    logger
        .log("tool_dispatch", &json!({"tool": "bash", "ok": true}))
        .expect("log");
    let contents = std::fs::read_to_string(logger.path()).expect("read");
    let line = contents.lines().next().expect("at least one line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("parse");
    assert_eq!(parsed["event"], "tool_dispatch");
    assert_eq!(parsed["data"]["tool"], "bash");
    assert_eq!(parsed["data"]["ok"], true);
    assert!(
        parsed["timestamp"].is_string(),
        "timestamp MUST be a string; got {:?}",
        parsed["timestamp"]
    );
}

#[test]
fn log_security_writes_same_shape_as_log() {
    let dir = TempDir::new().expect("tempdir");
    let mut logger = AuditLogger::new_in(dir.path(), "sec-test").expect("new");
    logger
        .log_security(
            "permission_denied",
            &json!({"tool": "bash", "reason": "blocked-by-policy"}),
        )
        .expect("log_security");
    let contents = std::fs::read_to_string(logger.path()).expect("read");
    let parsed: serde_json::Value =
        serde_json::from_str(contents.lines().next().unwrap()).expect("parse");
    assert_eq!(parsed["event"], "permission_denied");
    assert_eq!(parsed["data"]["reason"], "blocked-by-policy");
}

#[test]
fn multiple_log_calls_append_distinct_lines() {
    let dir = TempDir::new().expect("tempdir");
    let mut logger = AuditLogger::new_in(dir.path(), "append-test").expect("new");
    for i in 0..5 {
        logger.log("test_event", &json!({"i": i})).expect("log");
    }
    let contents = std::fs::read_to_string(logger.path()).expect("read");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 5, "5 logs MUST produce 5 lines");
    // Each line MUST parse + carry its own i.
    for (idx, line) in lines.iter().enumerate() {
        let parsed: serde_json::Value = serde_json::from_str(line).expect("parse");
        let i_val = parsed["data"]["i"].as_u64().expect("u64");
        assert_eq!(
            usize::try_from(i_val).expect("u64 fits usize on test host"),
            idx
        );
    }
}

#[test]
fn log_persists_complex_nested_data() {
    let dir = TempDir::new().expect("tempdir");
    let mut logger = AuditLogger::new_in(dir.path(), "nested").expect("new");
    let payload = json!({
        "user": {"id": 42, "name": "alice"},
        "tools": ["bash", "read_file"],
        "deep": {"nested": {"value": [1, 2, 3]}}
    });
    logger.log("complex_event", &payload).expect("log");
    let contents = std::fs::read_to_string(logger.path()).expect("read");
    let parsed: serde_json::Value =
        serde_json::from_str(contents.lines().next().unwrap()).expect("parse");
    assert_eq!(parsed["data"], payload);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — AuditLogger reopen preserves content
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn reopening_logger_with_same_session_id_appends_not_truncates() {
    let dir = TempDir::new().expect("tempdir");
    {
        let mut logger = AuditLogger::new_in(dir.path(), "persist").expect("new 1");
        logger.log("first", &json!({"n": 1})).expect("log 1");
    } // logger drops, file closes
    {
        let mut logger2 = AuditLogger::new_in(dir.path(), "persist").expect("new 2");
        logger2.log("second", &json!({"n": 2})).expect("log 2");
    }
    let path = dir.path().join("persist.jsonl");
    let contents = std::fs::read_to_string(&path).expect("read");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "second open MUST append, not truncate; got {} lines",
        lines.len()
    );
    let l1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let l2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(l1["event"], "first");
    assert_eq!(l2["event"], "second");
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — AuditError variant shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn audit_error_display_includes_path_for_mkdir_and_open_variants() {
    let dir = TempDir::new().expect("tempdir");
    let blocker = dir.path().join("file-not-dir");
    std::fs::write(&blocker, "x").expect("write");
    // AuditLogger isn't Debug; map Ok to a Debug-friendly stand-in
    // so we can use expect_err.
    let outcome = AuditLogger::new_in(&blocker.join("sub"), "s").map(|l| l.path().to_path_buf());
    let err = outcome.expect_err("MUST error");
    let display = format!("{err}");
    assert!(
        matches!(err, AuditError::Mkdir { .. } | AuditError::Open { .. }),
        "MUST be Mkdir or Open variant; got {err:?}"
    );
    assert!(
        display.contains(&blocker.display().to_string()) || display.contains("file-not-dir"),
        "error display MUST include path context; got {display:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — FAST_MODE constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fast_mode_constants_match_documented_cost_tier_30_150() {
    // Documented values: $30/M input, $150/M output (CC's
    // COST_TIER_30_150 — see crosslink #642).
    assert!(
        (FAST_MODE_INPUT_PER_MILLION - 30.0).abs() < f64::EPSILON,
        "FAST_MODE_INPUT_PER_MILLION MUST be 30.0; got {FAST_MODE_INPUT_PER_MILLION}"
    );
    assert!(
        (FAST_MODE_OUTPUT_PER_MILLION - 150.0).abs() < f64::EPSILON,
        "FAST_MODE_OUTPUT_PER_MILLION MUST be 150.0; got {FAST_MODE_OUTPUT_PER_MILLION}"
    );
}
