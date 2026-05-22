//! End-to-end tests for `services::EnterprisePolicy` —
//! `check_model` allowlist semantics, `check_request_tokens`
//! per-request boundary, `check_session_tokens` per-session
//! boundary. Pins boundary cases (exactly-cap, off-by-one,
//! empty allowlist, single-item allowlist).
//!
//! Sprint 199 of the verification effort. Sprint 113
//! covered the policy surface broadly; this file pins each
//! check_* method's exact boundary behaviour.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::services::{EnterprisePolicy, PolicyError};
use std::collections::HashSet;

fn empty_policy() -> EnterprisePolicy {
    EnterprisePolicy::default()
}

fn allowlist(models: &[&str]) -> EnterprisePolicy {
    EnterprisePolicy {
        model_allowlist: models.iter().map(|m| (*m).to_string()).collect(),
        ..EnterprisePolicy::default()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — check_model with empty allowlist
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_model_with_empty_allowlist_accepts_every_model() {
    // PINS DOC: empty allowlist = "no allowlist" = allow all.
    let p = empty_policy();
    assert!(p.check_model("gpt-4o").is_ok());
    assert!(p.check_model("claude-opus-4").is_ok());
    assert!(p.check_model("any-model").is_ok());
    assert!(p.check_model("xyz").is_ok());
}

#[test]
fn check_model_with_empty_allowlist_accepts_empty_string() {
    // PINS: empty model string still passes when no allowlist set.
    let p = empty_policy();
    assert!(p.check_model("").is_ok());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — check_model with single-item allowlist
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_model_single_item_allowlist_accepts_exact_match() {
    let p = allowlist(&["claude-opus-4-7"]);
    assert!(p.check_model("claude-opus-4-7").is_ok());
}

#[test]
fn check_model_single_item_allowlist_rejects_non_match() {
    let p = allowlist(&["claude-opus-4-7"]);
    let outcome = p.check_model("gpt-4o");
    assert!(matches!(outcome, Err(PolicyError::ModelDenied { .. })));
}

#[test]
fn check_model_match_is_case_sensitive() {
    // PINS DOC: HashSet::contains is byte-exact (case-sensitive).
    let p = allowlist(&["claude-opus-4-7"]);
    let outcome = p.check_model("Claude-Opus-4-7");
    assert!(
        outcome.is_err(),
        "case-mismatch MUST be rejected (HashSet exact match)"
    );
}

#[test]
fn check_model_match_does_not_trim_whitespace() {
    let p = allowlist(&["gpt-4o"]);
    let outcome = p.check_model(" gpt-4o ");
    assert!(
        outcome.is_err(),
        "leading/trailing whitespace MUST mismatch"
    );
}

#[test]
fn check_model_denied_error_carries_the_requested_name() {
    let p = allowlist(&["only-this"]);
    let err = p.check_model("requested-model-marker-199").unwrap_err();
    match err {
        PolicyError::ModelDenied { model } => {
            assert_eq!(model, "requested-model-marker-199");
        }
        other => panic!("expected ModelDenied; got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — check_model with multi-item allowlist
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_model_multi_item_allowlist_accepts_any_listed() {
    let p = allowlist(&["a", "b", "c"]);
    for m in ["a", "b", "c"] {
        assert!(
            p.check_model(m).is_ok(),
            "{m:?} MUST be accepted from allowlist"
        );
    }
}

#[test]
fn check_model_multi_item_allowlist_rejects_unlisted() {
    let p = allowlist(&["a", "b", "c"]);
    let outcome = p.check_model("d");
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — check_request_tokens boundary
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_request_tokens_with_no_cap_accepts_any_value() {
    let p = empty_policy();
    assert!(p.check_request_tokens(0).is_ok());
    assert!(p.check_request_tokens(usize::MAX).is_ok());
}

#[test]
fn check_request_tokens_at_cap_accepted_strict_greater_than() {
    // PINS BOUND: predicate is `estimated > cap`, so exactly = cap
    // is allowed.
    let mut p = empty_policy();
    p.max_request_tokens = Some(1000);
    assert!(
        p.check_request_tokens(1000).is_ok(),
        "exactly cap MUST be accepted"
    );
}

#[test]
fn check_request_tokens_one_over_cap_rejected() {
    let mut p = empty_policy();
    p.max_request_tokens = Some(1000);
    let outcome = p.check_request_tokens(1001);
    assert!(matches!(outcome, Err(PolicyError::TokenCapExceeded { .. })));
}

#[test]
fn check_request_tokens_error_carries_estimated_and_cap() {
    let mut p = empty_policy();
    p.max_request_tokens = Some(500);
    let err = p.check_request_tokens(750).unwrap_err();
    match err {
        PolicyError::TokenCapExceeded {
            estimated,
            cap,
            scope,
        } => {
            assert_eq!(estimated, 750);
            assert_eq!(cap, 500);
            assert_eq!(scope, "request");
        }
        other => panic!("expected TokenCapExceeded; got {other:?}"),
    }
}

#[test]
fn check_request_tokens_error_scope_is_request_not_session() {
    let mut p = empty_policy();
    p.max_request_tokens = Some(100);
    let err = p.check_request_tokens(200).unwrap_err();
    if let PolicyError::TokenCapExceeded { scope, .. } = err {
        assert_eq!(scope, "request");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — check_session_tokens boundary
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_session_tokens_with_no_cap_accepts_any_value() {
    let p = empty_policy();
    assert!(p.check_session_tokens(usize::MAX).is_ok());
}

#[test]
fn check_session_tokens_at_cap_accepted_strict_greater_than() {
    let mut p = empty_policy();
    p.max_session_tokens = Some(10_000);
    assert!(p.check_session_tokens(10_000).is_ok());
}

#[test]
fn check_session_tokens_one_over_cap_rejected() {
    let mut p = empty_policy();
    p.max_session_tokens = Some(10_000);
    let outcome = p.check_session_tokens(10_001);
    assert!(outcome.is_err());
}

#[test]
fn check_session_tokens_error_scope_is_session_not_request() {
    let mut p = empty_policy();
    p.max_session_tokens = Some(100);
    let err = p.check_session_tokens(200).unwrap_err();
    if let PolicyError::TokenCapExceeded { scope, .. } = err {
        assert_eq!(scope, "session");
    }
}

#[test]
fn check_session_tokens_error_carries_estimated_and_cap() {
    let mut p = empty_policy();
    p.max_session_tokens = Some(5000);
    let err = p.check_session_tokens(7500).unwrap_err();
    match err {
        PolicyError::TokenCapExceeded { estimated, cap, .. } => {
            assert_eq!(estimated, 7500);
            assert_eq!(cap, 5000);
        }
        other => panic!("expected TokenCapExceeded; got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Cross-method independence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_request_and_session_caps_are_independent() {
    // PINS: request cap 100, session cap 1000 — caller can exceed
    // request without tripping session check.
    let mut p = empty_policy();
    p.max_request_tokens = Some(100);
    p.max_session_tokens = Some(1000);
    // 500 tokens fails request (>100) but passes session (<=1000).
    assert!(p.check_request_tokens(500).is_err());
    assert!(p.check_session_tokens(500).is_ok());
}

#[test]
fn check_model_independent_of_token_caps() {
    let mut p = allowlist(&["x"]);
    p.max_request_tokens = Some(100);
    // Model OK, tokens not checked here.
    assert!(p.check_model("x").is_ok());
    // Model NOT in allowlist — but check_request_tokens is fine.
    assert!(p.check_request_tokens(50).is_ok());
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Serde round-trip for the policy struct
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn enterprise_policy_default_serde_round_trip() {
    let original = EnterprisePolicy::default();
    let json = serde_json::to_value(&original).expect("ser");
    let back: EnterprisePolicy = serde_json::from_value(json).expect("de");
    assert_eq!(original.max_request_tokens, back.max_request_tokens);
    assert_eq!(original.max_session_tokens, back.max_session_tokens);
    assert_eq!(original.model_allowlist.len(), back.model_allowlist.len());
}

#[test]
fn enterprise_policy_with_full_config_round_trips() {
    let mut al = HashSet::new();
    al.insert("claude-opus-4".to_string());
    al.insert("gpt-4o".to_string());
    let original = EnterprisePolicy {
        max_request_tokens: Some(10_000),
        max_session_tokens: Some(100_000),
        model_allowlist: al,
        ..EnterprisePolicy::default()
    };

    let json = serde_json::to_value(&original).expect("ser");
    let back: EnterprisePolicy = serde_json::from_value(json).expect("de");

    assert_eq!(back.max_request_tokens, Some(10_000));
    assert_eq!(back.max_session_tokens, Some(100_000));
    assert_eq!(back.model_allowlist.len(), 2);
    assert!(back.model_allowlist.contains("claude-opus-4"));
}
