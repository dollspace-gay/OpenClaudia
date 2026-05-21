//! End-to-end tests for `vdd::VddSession` + `VddIteration` +
//! `AdversaryReview` struct shape + serde + `Finding` /
//! `FindingStatus` / `Severity` serde matrix.
//!
//! Sprint 100 of the verification effort (milestone). Sprint 54
//! covered the triage parser; sprint 55 covered confabulation
//! detection; this file pins the loop-orchestration data types
//! that the verifier emits — `VddSession` (full session),
//! `VddIteration` (per-loop) and `AdversaryReview`
//! (per-review-call).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::float_cmp)]

use chrono::Utc;
use openclaudia::config::VddMode;
use openclaudia::session::TokenUsage;
use openclaudia::vdd::{
    AdversaryReview, Finding, FindingStatus, Severity, StaticAnalysisResult, VddIteration,
    VddSession,
};

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn fresh_finding(id: &str, sev: Severity, status: FindingStatus) -> Finding {
    Finding {
        id: id.to_string(),
        severity: sev,
        cwe: Some("CWE-89".to_string()),
        description: "test finding".to_string(),
        file_path: Some("src/lib.rs".to_string()),
        line_range: Some((10, 20)),
        status,
        adversary_reasoning: "this is bad because…".to_string(),
        iteration: 1,
    }
}

fn fresh_review(iteration: u32, findings: Vec<Finding>) -> AdversaryReview {
    AdversaryReview {
        iteration,
        findings,
        raw_response: "model output text".to_string(),
        tokens_used: TokenUsage::default(),
        timestamp: Utc::now(),
    }
}

fn fresh_iteration(number: u32, genuine: u32, false_positives: u32) -> VddIteration {
    VddIteration {
        number,
        builder_response: "implementation here".to_string(),
        static_analysis: Vec::<StaticAnalysisResult>::new(),
        adversary_review: fresh_review(number, Vec::new()),
        genuine_count: genuine,
        false_positive_count: false_positives,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — Severity serde (UPPERCASE)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn severity_serializes_as_uppercase_strings() {
    for (variant, expected) in &[
        (Severity::Critical, "CRITICAL"),
        (Severity::High, "HIGH"),
        (Severity::Medium, "MEDIUM"),
        (Severity::Low, "LOW"),
        (Severity::Info, "INFO"),
    ] {
        let json = serde_json::to_string(variant).expect("ser");
        assert_eq!(json.trim_matches('"'), *expected);
    }
}

#[test]
fn severity_deserializes_from_uppercase_strings() {
    for (input, expected) in &[
        ("\"CRITICAL\"", Severity::Critical),
        ("\"HIGH\"", Severity::High),
        ("\"MEDIUM\"", Severity::Medium),
        ("\"LOW\"", Severity::Low),
        ("\"INFO\"", Severity::Info),
    ] {
        let parsed: Severity = serde_json::from_str(input).expect("de");
        assert_eq!(parsed, *expected);
    }
}

#[test]
fn severity_round_trips() {
    for v in &[
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
        Severity::Info,
    ] {
        let json = serde_json::to_string(v).expect("ser");
        let back: Severity = serde_json::from_str(&json).expect("de");
        assert_eq!(back, *v);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — FindingStatus serde (snake_case)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finding_status_serializes_as_snake_case() {
    for (variant, expected) in &[
        (FindingStatus::Genuine, "genuine"),
        (FindingStatus::FalsePositive, "false_positive"),
        (FindingStatus::Disputed, "disputed"),
    ] {
        let json = serde_json::to_string(variant).expect("ser");
        assert_eq!(json.trim_matches('"'), *expected);
    }
}

#[test]
fn finding_status_deserializes_from_snake_case() {
    for (input, expected) in &[
        ("\"genuine\"", FindingStatus::Genuine),
        ("\"false_positive\"", FindingStatus::FalsePositive),
        ("\"disputed\"", FindingStatus::Disputed),
    ] {
        let parsed: FindingStatus = serde_json::from_str(input).expect("de");
        assert_eq!(parsed, *expected);
    }
}

#[test]
fn finding_status_round_trips() {
    for v in &[
        FindingStatus::Genuine,
        FindingStatus::FalsePositive,
        FindingStatus::Disputed,
    ] {
        let json = serde_json::to_string(v).expect("ser");
        let back: FindingStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(back, *v);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Finding serde round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finding_full_shape_round_trips() {
    let original = fresh_finding("F-1", Severity::High, FindingStatus::Genuine);
    let json = serde_json::to_string(&original).expect("ser");
    let back: Finding = serde_json::from_str(&json).expect("de");
    assert_eq!(back.id, original.id);
    assert_eq!(back.severity, original.severity);
    assert_eq!(back.cwe, original.cwe);
    assert_eq!(back.description, original.description);
    assert_eq!(back.file_path, original.file_path);
    assert_eq!(back.line_range, original.line_range);
    assert_eq!(back.status, original.status);
    assert_eq!(back.adversary_reasoning, original.adversary_reasoning);
    assert_eq!(back.iteration, original.iteration);
}

#[test]
fn finding_with_none_optional_fields_round_trips() {
    let original = Finding {
        id: "F-2".to_string(),
        severity: Severity::Low,
        cwe: None,
        description: "minimal".to_string(),
        file_path: None,
        line_range: None,
        status: FindingStatus::Disputed,
        adversary_reasoning: "x".to_string(),
        iteration: 5,
    };
    let json = serde_json::to_string(&original).expect("ser");
    let back: Finding = serde_json::from_str(&json).expect("de");
    assert!(back.cwe.is_none());
    assert!(back.file_path.is_none());
    assert!(back.line_range.is_none());
}

#[test]
fn finding_line_range_round_trips_as_tuple() {
    let f = fresh_finding("F", Severity::Medium, FindingStatus::Genuine);
    let json = serde_json::to_string(&f).expect("ser");
    let back: Finding = serde_json::from_str(&json).expect("de");
    assert_eq!(back.line_range, Some((10, 20)));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — AdversaryReview shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn adversary_review_carries_iteration_findings_response_tokens_timestamp() {
    let finding = fresh_finding("F-A", Severity::Critical, FindingStatus::Genuine);
    let review = fresh_review(3, vec![finding]);
    assert_eq!(review.iteration, 3);
    assert_eq!(review.findings.len(), 1);
    assert_eq!(review.findings[0].id, "F-A");
    assert_eq!(review.raw_response, "model output text");
    // TokenUsage doesn't impl PartialEq; compare totals field-wise.
    assert_eq!(review.tokens_used.input_tokens, 0);
    assert_eq!(review.tokens_used.output_tokens, 0);
}

#[test]
fn adversary_review_serializes_to_json_with_documented_fields() {
    let review = fresh_review(1, Vec::new());
    let json = serde_json::to_string(&review).expect("ser");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("re-parse");
    assert!(parsed.get("iteration").is_some());
    assert!(parsed.get("findings").is_some());
    assert!(parsed.get("raw_response").is_some());
    assert!(parsed.get("tokens_used").is_some());
    assert!(parsed.get("timestamp").is_some());
}

#[test]
fn adversary_review_clone_preserves_all_fields() {
    let review = fresh_review(
        7,
        vec![fresh_finding("F", Severity::High, FindingStatus::Genuine)],
    );
    let cloned = review.clone();
    assert_eq!(cloned.iteration, review.iteration);
    assert_eq!(cloned.findings.len(), review.findings.len());
    assert_eq!(cloned.raw_response, review.raw_response);
    assert_eq!(cloned.timestamp, review.timestamp);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — VddIteration shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn vdd_iteration_carries_builder_response_static_analysis_review_counts() {
    let iter = fresh_iteration(2, 3, 1);
    assert_eq!(iter.number, 2);
    assert_eq!(iter.genuine_count, 3);
    assert_eq!(iter.false_positive_count, 1);
    assert_eq!(iter.builder_response, "implementation here");
    assert!(iter.static_analysis.is_empty());
}

#[test]
fn vdd_iteration_clone_preserves_all_fields() {
    let original = fresh_iteration(5, 2, 0);
    let cloned = original.clone();
    assert_eq!(cloned.number, original.number);
    assert_eq!(cloned.genuine_count, original.genuine_count);
    assert_eq!(cloned.false_positive_count, original.false_positive_count);
}

#[test]
fn vdd_iteration_serializes_to_json() {
    let iter = fresh_iteration(1, 5, 2);
    let json = serde_json::to_string(&iter).expect("ser");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("re-parse");
    assert_eq!(parsed["number"], 1);
    assert_eq!(parsed["genuine_count"], 5);
    assert_eq!(parsed["false_positive_count"], 2);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — VddSession shape (struct-literal since new is pub(crate))
// ───────────────────────────────────────────────────────────────────────────

fn fresh_session(id: &str) -> VddSession {
    VddSession {
        id: id.to_string(),
        mode: VddMode::default(),
        iterations: Vec::new(),
        total_findings: 0,
        total_genuine: 0,
        total_false_positives: 0,
        false_positive_rate: 0.0,
        converged: false,
        termination_reason: None,
        builder_tokens: TokenUsage::default(),
        adversary_tokens: TokenUsage::default(),
        started_at: Utc::now(),
        ended_at: None,
    }
}

#[test]
fn vdd_session_initial_state_has_zero_counts_and_not_converged() {
    let s = fresh_session("session-1");
    assert_eq!(s.id, "session-1");
    assert_eq!(s.total_findings, 0);
    assert_eq!(s.total_genuine, 0);
    assert_eq!(s.total_false_positives, 0);
    assert_eq!(s.false_positive_rate, 0.0);
    assert!(!s.converged);
    assert!(s.termination_reason.is_none());
    assert!(s.iterations.is_empty());
    assert!(s.ended_at.is_none());
}

#[test]
fn vdd_session_carries_builder_and_adversary_token_usage_independently() {
    let mut s = fresh_session("s");
    s.builder_tokens = TokenUsage {
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    s.adversary_tokens = TokenUsage {
        input_tokens: 2000,
        output_tokens: 1500,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    assert_eq!(s.builder_tokens.input_tokens, 1000);
    assert_eq!(s.adversary_tokens.input_tokens, 2000);
    // Independent counters; total_tokens differs.
    assert_ne!(s.builder_tokens.total(), s.adversary_tokens.total());
}

#[test]
fn vdd_session_converged_with_termination_reason_serializes() {
    let mut s = fresh_session("s");
    s.converged = true;
    s.termination_reason = Some("zero genuine findings".to_string());
    s.ended_at = Some(Utc::now());
    let json = serde_json::to_string(&s).expect("ser");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("re-parse");
    assert_eq!(parsed["converged"], true);
    assert_eq!(parsed["termination_reason"], "zero genuine findings");
    assert!(parsed.get("ended_at").is_some());
}

#[test]
fn vdd_session_false_positive_rate_is_finite() {
    let s = fresh_session("s");
    assert!(s.false_positive_rate.is_finite());
}

#[test]
fn vdd_session_iterations_field_holds_vdd_iteration_vec() {
    let mut s = fresh_session("s");
    s.iterations.push(fresh_iteration(1, 2, 1));
    s.iterations.push(fresh_iteration(2, 0, 0));
    assert_eq!(s.iterations.len(), 2);
    assert_eq!(s.iterations[0].number, 1);
    assert_eq!(s.iterations[1].number, 2);
}

#[test]
fn vdd_session_clone_preserves_all_fields() {
    let s = fresh_session("clone-test");
    let cloned = s.clone();
    assert_eq!(cloned.id, s.id);
    assert_eq!(cloned.total_findings, s.total_findings);
    assert_eq!(cloned.converged, s.converged);
    assert_eq!(cloned.builder_tokens.total(), s.builder_tokens.total());
}

#[test]
fn vdd_session_serializes_to_json_with_documented_fields() {
    let s = fresh_session("s");
    let json = serde_json::to_string(&s).expect("ser");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("re-parse");
    for field in &[
        "id",
        "mode",
        "iterations",
        "total_findings",
        "total_genuine",
        "total_false_positives",
        "false_positive_rate",
        "converged",
        "termination_reason",
        "builder_tokens",
        "adversary_tokens",
        "started_at",
        "ended_at",
    ] {
        assert!(
            parsed.get(field).is_some(),
            "field {field:?} MUST be present in serialized output; got {json:?}"
        );
    }
}
