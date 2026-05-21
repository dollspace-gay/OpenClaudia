//! End-to-end tests for `ConfabulationTracker` rate semantics +
//! `finding_signature` / `weak_finding_signature` collision
//! properties.
//!
//! Sprint 55 of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::vdd::confabulation::{
    finding_signature, weak_finding_signature, ConfabulationTracker, FindingIdentity,
};
use openclaudia::vdd::{Finding, FindingStatus, Severity};

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn finding(
    severity: Severity,
    file: Option<&str>,
    cwe: Option<&str>,
    lines: Option<(usize, usize)>,
    description: &str,
) -> Finding {
    Finding {
        id: "fid".to_string(),
        severity,
        cwe: cwe.map(str::to_string),
        description: description.to_string(),
        file_path: file.map(str::to_string),
        line_range: lines,
        status: FindingStatus::Genuine,
        adversary_reasoning: String::new(),
        iteration: 1,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — ConfabulationTracker::record_iteration
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn record_iteration_with_only_genuine_findings_returns_rate_zero() {
    let mut tracker = ConfabulationTracker::new(0.75, 2);
    let rate = tracker.record_iteration(5, 0);
    assert_eq!(rate, Some(0.0), "5 genuine + 0 FP → rate=0.0");
}

#[test]
fn record_iteration_with_only_false_positives_returns_rate_one() {
    let mut tracker = ConfabulationTracker::new(0.75, 1);
    let rate = tracker.record_iteration(0, 5);
    assert_eq!(rate, Some(1.0), "0 genuine + 5 FP → rate=1.0");
}

#[test]
fn record_iteration_with_zero_findings_returns_none_clean_pass() {
    let mut tracker = ConfabulationTracker::new(0.75, 1);
    let rate = tracker.record_iteration(0, 0);
    assert!(
        rate.is_none(),
        "0+0 MUST yield None (clean pass — not a confabulation signal)"
    );
}

#[test]
fn record_iteration_with_mixed_findings_returns_correct_proportion() {
    let mut tracker = ConfabulationTracker::new(0.75, 1);
    let rate = tracker.record_iteration(2, 3);
    // 3 FP out of 5 total = 0.6.
    let r = rate.expect("non-zero total");
    assert!((r - 0.6).abs() < 1e-12, "2g+3fp MUST be 0.6; got {r}");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — should_terminate predicate
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn should_terminate_false_when_below_min_iterations() {
    let mut tracker = ConfabulationTracker::new(0.75, 3);
    tracker.record_iteration(0, 10); // 100% FP but only 1 iteration
    tracker.record_iteration(0, 10); // 100% FP but only 2 iterations
    assert!(
        !tracker.should_terminate(),
        "MUST NOT terminate before min_iterations even at 100% FP"
    );
}

#[test]
fn should_terminate_true_when_above_threshold_past_min() {
    let mut tracker = ConfabulationTracker::new(0.75, 2);
    tracker.record_iteration(2, 3); // 60% FP
    tracker.record_iteration(1, 5); // 83% FP → above 75% threshold
    assert!(tracker.should_terminate());
}

#[test]
fn should_terminate_false_when_below_threshold_past_min() {
    let mut tracker = ConfabulationTracker::new(0.75, 2);
    tracker.record_iteration(8, 2); // 20% FP
    tracker.record_iteration(7, 3); // 30% FP
    assert!(
        !tracker.should_terminate(),
        "below-threshold sequence MUST NOT terminate"
    );
}

#[test]
fn should_terminate_false_when_only_zero_finding_iterations() {
    let mut tracker = ConfabulationTracker::new(0.75, 1);
    for _ in 0..5 {
        tracker.record_iteration(0, 0);
    }
    assert!(
        !tracker.should_terminate(),
        "all clean passes MUST NOT terminate as confabulation"
    );
}

#[test]
fn should_terminate_uses_latest_rate_not_cumulative_average() {
    // Past-min, latest rate above threshold even if early
    // iterations were below.
    let mut tracker = ConfabulationTracker::new(0.75, 2);
    tracker.record_iteration(10, 0); // 0% FP
    tracker.record_iteration(1, 5); // 83% FP — latest above
    assert!(
        tracker.should_terminate(),
        "latest rate above threshold MUST terminate even if early iters were clean"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — finding_signature identity + invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn identical_findings_produce_identical_signatures() {
    let f1 = finding(
        Severity::High,
        Some("src/x.rs"),
        Some("CWE-79"),
        Some((10, 20)),
        "XSS in handler",
    );
    let mut f2 = f1.clone();
    // Same identity-tuple, different id field — must NOT
    // affect signature.
    f2.id = "different-id".to_string();
    assert_eq!(finding_signature(&f1), finding_signature(&f2));
}

#[test]
fn signature_is_case_insensitive_for_file_path_and_cwe() {
    let f1 = finding(
        Severity::High,
        Some("src/X.rs"),
        Some("CWE-79"),
        Some((10, 20)),
        "issue",
    );
    let f2 = finding(
        Severity::High,
        Some("src/x.rs"),
        Some("cwe-79"),
        Some((10, 20)),
        "issue",
    );
    assert_eq!(
        finding_signature(&f1),
        finding_signature(&f2),
        "case differences in file_path + cwe MUST NOT change signature"
    );
}

#[test]
fn signature_differs_when_severity_differs() {
    let f1 = finding(
        Severity::High,
        Some("src/x.rs"),
        Some("CWE-79"),
        Some((10, 20)),
        "issue",
    );
    let f2 = finding(
        Severity::Low,
        Some("src/x.rs"),
        Some("CWE-79"),
        Some((10, 20)),
        "issue",
    );
    assert_ne!(
        finding_signature(&f1),
        finding_signature(&f2),
        "severity change MUST change signature"
    );
}

#[test]
fn signature_differs_when_line_range_differs() {
    let f1 = finding(
        Severity::High,
        Some("f.rs"),
        Some("CWE-1"),
        Some((10, 20)),
        "x",
    );
    let f2 = finding(
        Severity::High,
        Some("f.rs"),
        Some("CWE-1"),
        Some((30, 40)),
        "x",
    );
    assert_ne!(finding_signature(&f1), finding_signature(&f2));
}

#[test]
fn signature_differs_when_file_path_differs() {
    let f1 = finding(
        Severity::High,
        Some("a.rs"),
        Some("CWE-1"),
        Some((1, 2)),
        "x",
    );
    let f2 = finding(
        Severity::High,
        Some("b.rs"),
        Some("CWE-1"),
        Some((1, 2)),
        "x",
    );
    assert_ne!(finding_signature(&f1), finding_signature(&f2));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — FindingIdentity::is_weak
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn is_weak_true_when_both_cwe_and_line_range_are_none() {
    let f = finding(Severity::High, Some("x.rs"), None, None, "desc");
    let id = FindingIdentity::from_finding(&f);
    assert!(id.is_weak(), "no cwe + no line_range MUST classify as weak");
}

#[test]
fn is_weak_false_when_cwe_present() {
    let f = finding(Severity::High, Some("x.rs"), Some("CWE-79"), None, "desc");
    let id = FindingIdentity::from_finding(&f);
    assert!(!id.is_weak(), "having a cwe MUST NOT classify as weak");
}

#[test]
fn is_weak_false_when_line_range_present() {
    let f = finding(Severity::High, Some("x.rs"), None, Some((1, 1)), "desc");
    let id = FindingIdentity::from_finding(&f);
    assert!(
        !id.is_weak(),
        "having a line_range MUST NOT classify as weak"
    );
}

#[test]
fn is_weak_false_when_both_present() {
    let f = finding(
        Severity::High,
        Some("x.rs"),
        Some("CWE-79"),
        Some((1, 1)),
        "desc",
    );
    let id = FindingIdentity::from_finding(&f);
    assert!(!id.is_weak());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — weak_finding_signature behaviour
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn weak_signature_collapses_minor_description_suffix_variations() {
    // First 32 bytes of lowercased description form the
    // weak hash. Two findings whose first 32 chars match
    // (regardless of trailing suffix) MUST produce the same
    // weak signature.
    let prefix = "memory leak in subscriber tickrr"; // 32 chars exactly
    let f1 = finding(
        Severity::High,
        Some("a.rs"),
        None,
        None,
        prefix, // 32-char prefix matches.
    );
    let f2 = finding(
        Severity::High,
        Some("a.rs"),
        None,
        None,
        &format!("{prefix} - extra trailing context here (re-reported)"),
    );
    assert_eq!(
        weak_finding_signature(&f1),
        weak_finding_signature(&f2),
        "weak signature MUST collapse matching 32-byte prefixes"
    );
}

#[test]
fn weak_signature_differs_when_first_32_bytes_differ() {
    let f1 = finding(
        Severity::High,
        Some("a.rs"),
        None,
        None,
        "AAAAAAAAAA pattern 1",
    );
    let f2 = finding(
        Severity::High,
        Some("a.rs"),
        None,
        None,
        "BBBBBBBBBB pattern 2",
    );
    assert_ne!(weak_finding_signature(&f1), weak_finding_signature(&f2));
}

#[test]
fn weak_signature_is_case_insensitive_in_description() {
    let f1 = finding(
        Severity::High,
        Some("a.rs"),
        None,
        None,
        "Memory Leak In Worker",
    );
    let f2 = finding(
        Severity::High,
        Some("a.rs"),
        None,
        None,
        "memory leak in worker",
    );
    assert_eq!(
        weak_finding_signature(&f1),
        weak_finding_signature(&f2),
        "weak signature MUST be case-insensitive in description"
    );
}

#[test]
fn weak_signature_handles_multibyte_chars_at_prefix_boundary() {
    // 4-byte emoji "🎉" placed near the 32-byte prefix
    // boundary MUST NOT split mid-codepoint — the impl walks
    // char_indices to stay on UTF-8 boundaries.
    let with_emoji =
        "issue \u{1F389}\u{1F389}\u{1F389}\u{1F389}\u{1F389}\u{1F389}\u{1F389}\u{1F389}";
    let f = finding(Severity::High, Some("a.rs"), None, None, with_emoji);
    // Must not panic, must return some hash.
    let _ = weak_finding_signature(&f);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — strong + weak signatures are domain-separated
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn strong_and_weak_signatures_use_different_domains() {
    // Even on the SAME finding, the strong and weak hashes
    // should differ — the "strong" / "weak" domain-separator
    // prefixes ensure no accidental cross-bucket collision.
    let f = finding(Severity::High, Some("a.rs"), None, None, "issue");
    assert_ne!(
        finding_signature(&f),
        weak_finding_signature(&f),
        "strong + weak hashes MUST use distinct domains to avoid \
         cross-bucket collisions"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — FindingIdentity::from_finding round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn identity_from_finding_then_signature_matches_helper() {
    let f = finding(
        Severity::Critical,
        Some("src/auth.rs"),
        Some("CWE-287"),
        Some((100, 110)),
        "auth bypass",
    );
    let id = FindingIdentity::from_finding(&f);
    assert_eq!(id.signature(), finding_signature(&f));
    assert_eq!(id.weak_signature(), weak_finding_signature(&f));
}

#[test]
fn identity_round_trips_every_field_into_clone_eq() {
    let f = finding(
        Severity::Medium,
        Some("x.rs"),
        Some("CWE-1"),
        Some((5, 9)),
        "desc",
    );
    let id_1 = FindingIdentity::from_finding(&f);
    let id_2 = id_1.clone();
    assert_eq!(id_1, id_2, "Clone MUST produce equal value");
}
