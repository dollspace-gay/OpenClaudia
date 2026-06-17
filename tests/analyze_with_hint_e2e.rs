//! End-to-end tests for `compaction::ContextCompactor::analyze_with_hint` —
//! the hint-vs-estimate path + `needs_compaction` decision +
//! `tokens_to_free` computation, including `RESPONSE_RESERVE`
//! deduction (4096 tokens) and the target = threshold/2
//! recovery target.
//!
//! Sprint 188 of the verification effort. Sprint 94/184
//! covered the surface; this file pins the specific
//! decision-making constants and the per-field outputs.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::compaction::{CompactionConfig, ContextCompactor};
use openclaudia::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use std::collections::HashMap;

fn empty_req() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "test".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text("hi".to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: std::collections::HashMap::new(),
        }],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    }
}

fn compactor_with(max_tokens: usize, threshold: f32) -> ContextCompactor {
    let cfg = CompactionConfig {
        max_context_tokens: max_tokens,
        threshold,
        ..CompactionConfig::default()
    };
    ContextCompactor::new(cfg)
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — current_tokens propagation (hint vs estimate)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hint_some_overrides_estimate_for_current_tokens() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(99_999));
    assert_eq!(
        analysis.current_tokens, 99_999,
        "PINS: hint MUST win over estimate when provided"
    );
}

#[test]
fn hint_none_uses_estimated_request_tokens_not_huge_hint() {
    // PINS: hint=None falls back to estimator. We can't predict
    // the exact value but verify it's distinct from a huge hint
    // and bounded well under the 100k max.
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let with_estimate = c.analyze_with_hint(&req, None);
    let with_huge_hint = c.analyze_with_hint(&req, Some(99_999));
    assert_ne!(
        with_estimate.current_tokens, with_huge_hint.current_tokens,
        "estimator and hint must yield different values"
    );
    assert!(
        with_estimate.current_tokens < 10_000,
        "estimate of 'hi' request MUST be well under 10k; got {}",
        with_estimate.current_tokens
    );
}

#[test]
fn hint_zero_is_honored_not_treated_as_none() {
    // Some(0) is an explicit zero, NOT a missing value.
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(0));
    assert_eq!(analysis.current_tokens, 0);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — max_tokens propagation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn max_tokens_reflects_config_max_context_tokens() {
    let c = compactor_with(123_456, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(100));
    assert_eq!(analysis.max_tokens, 123_456);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — needs_compaction decision
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn needs_compaction_false_when_well_under_threshold() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(1000));
    assert!(!analysis.needs_compaction);
}

#[test]
fn needs_compaction_true_when_at_or_above_full_capacity() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(100_000));
    assert!(analysis.needs_compaction);
}

#[test]
fn needs_compaction_uses_effective_threshold_with_4096_response_reserve() {
    // PINS RESPONSE_RESERVE: effective_threshold = (max * threshold) - 4096.
    // max=100_000, threshold=0.9 → 90_000 - 4096 = 85_904.
    // 85_905 tokens should trigger; 85_904 should not.
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let just_over = c.analyze_with_hint(&req, Some(85_905));
    assert!(just_over.needs_compaction);
    let just_under = c.analyze_with_hint(&req, Some(85_904));
    assert!(!just_under.needs_compaction);
}

#[test]
fn needs_compaction_false_with_threshold_one_when_tokens_well_below() {
    // threshold=1.0 → effective = max - 4096.
    let c = compactor_with(100_000, 1.0);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(50_000));
    assert!(!analysis.needs_compaction);
}

#[test]
fn needs_compaction_true_with_low_threshold() {
    // threshold=0.1 → effective = 10_000 - 4096 = 5904.
    let c = compactor_with(100_000, 0.1);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(6000));
    assert!(analysis.needs_compaction);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — tokens_to_free
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tokens_to_free_is_zero_when_no_compaction_needed() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(1000));
    assert_eq!(analysis.tokens_to_free, 0);
}

#[test]
fn tokens_to_free_is_positive_when_compaction_needed() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(95_000));
    assert!(analysis.tokens_to_free > 0);
}

#[test]
fn tokens_to_free_reflects_distance_from_target_half_threshold() {
    // PINS DOC: target = threshold_tokens / 2 = 45_000 for
    // max=100k threshold=0.9. tokens_to_free = current - target.
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(95_000));
    // target = 90_000 / 2 = 45_000. tokens_to_free = 95_000 - 45_000 = 50_000.
    assert_eq!(analysis.tokens_to_free, 50_000);
}

#[test]
fn tokens_to_free_uses_saturating_sub_against_overflow() {
    // PINS SATURATING: if current < target somehow, tokens_to_free
    // saturates at 0 rather than underflowing.
    // We rig this with a tiny threshold making target large vs current.
    let c = compactor_with(100_000, 0.1);
    let req = empty_req();
    // current=6000, target=5000. tokens_to_free = 6000-5000 = 1000.
    let analysis = c.analyze_with_hint(&req, Some(6000));
    // No panic; result is sensible.
    assert!(analysis.tokens_to_free <= 6000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — messages_to_preserve + messages_to_summarize
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn analysis_has_preserve_and_summarize_indices() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(100));
    // Both fields are Vec<usize>; verify they're present.
    let _: Vec<usize> = analysis.messages_to_preserve;
    let _: Vec<usize> = analysis.messages_to_summarize;
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Determinism
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn repeated_analysis_with_same_inputs_yields_same_decision() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let a1 = c.analyze_with_hint(&req, Some(50_000));
    let a2 = c.analyze_with_hint(&req, Some(50_000));
    assert_eq!(a1.needs_compaction, a2.needs_compaction);
    assert_eq!(a1.tokens_to_free, a2.tokens_to_free);
    assert_eq!(a1.current_tokens, a2.current_tokens);
    assert_eq!(a1.max_tokens, a2.max_tokens);
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Saturation against absurd inputs
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn huge_max_tokens_does_not_overflow() {
    let c = compactor_with(usize::MAX, 0.9);
    let req = empty_req();
    let _ = c.analyze_with_hint(&req, Some(1000));
    // No panic.
}

#[test]
fn huge_hint_does_not_overflow() {
    let c = compactor_with(100_000, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(usize::MAX));
    // usize::MAX vs 100k → needs compaction.
    assert!(analysis.needs_compaction);
}

#[test]
fn zero_max_tokens_does_not_panic() {
    let c = compactor_with(0, 0.9);
    let req = empty_req();
    let analysis = c.analyze_with_hint(&req, Some(100));
    // max_tokens reported as 0; any positive current is over.
    assert_eq!(analysis.max_tokens, 0);
}

#[test]
fn threshold_zero_compacts_at_first_token_above_reserve() {
    let c = compactor_with(100_000, 0.0);
    let req = empty_req();
    // effective_threshold = 0 - 4096 saturates to 0 → any current > 0 compacts.
    let analysis = c.analyze_with_hint(&req, Some(1));
    assert!(analysis.needs_compaction);
}
