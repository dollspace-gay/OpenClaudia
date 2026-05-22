//! End-to-end tests for `config::adaptive_budget_for` step
//! function (low/medium/high) and `ThinkingConfig::effective_budget`
//! precedence (explicit > adaptive > `provider_default`).
//!
//! Sprint 178 of the verification effort. Sprint 112 covered
//! the per-provider transform behaviour; this file pins
//! the documented #599 step-function + 3-tier precedence
//! contract at the config layer.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::{adaptive_budget_for, ThinkingConfig};

// ───────────────────────────────────────────────────────────────────────────
// Section A — adaptive_budget_for step function
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn low_effort_returns_1024_anthropic_minimum() {
    // PINS #599: low → 1024 tokens (Anthropic minimum).
    assert_eq!(adaptive_budget_for("low"), 1024);
}

#[test]
fn medium_effort_returns_8000() {
    // PINS #599: medium → 8000 tokens (sane default).
    assert_eq!(adaptive_budget_for("medium"), 8000);
}

#[test]
fn med_alias_returns_same_as_medium() {
    // PINS DOC: "med" is an accepted alias for "medium".
    assert_eq!(adaptive_budget_for("med"), 8000);
    assert_eq!(adaptive_budget_for("med"), adaptive_budget_for("medium"));
}

#[test]
fn high_effort_returns_16000_ceiling() {
    // PINS #599: high → 16000 tokens (deep reasoning ceiling).
    assert_eq!(adaptive_budget_for("high"), 16000);
}

#[test]
fn unknown_effort_returns_zero_for_provider_fallback() {
    // PINS DOC: any other value (including empty) returns 0
    // which adapters interpret as "use provider default".
    assert_eq!(adaptive_budget_for(""), 0);
    assert_eq!(adaptive_budget_for("none"), 0);
    assert_eq!(adaptive_budget_for("maximum"), 0);
    assert_eq!(adaptive_budget_for("ultra"), 0);
    assert_eq!(adaptive_budget_for("garbage"), 0);
}

#[test]
fn effort_is_case_insensitive_for_each_tier() {
    // PINS DOC: input lowercased before match.
    assert_eq!(adaptive_budget_for("LOW"), 1024);
    assert_eq!(adaptive_budget_for("Low"), 1024);
    assert_eq!(adaptive_budget_for("LOW"), adaptive_budget_for("low"));
    assert_eq!(adaptive_budget_for("MEDIUM"), 8000);
    assert_eq!(adaptive_budget_for("HIGH"), 16000);
    assert_eq!(adaptive_budget_for("HiGh"), 16000);
}

#[test]
fn med_alias_is_case_insensitive() {
    assert_eq!(adaptive_budget_for("MED"), 8000);
    assert_eq!(adaptive_budget_for("Med"), 8000);
}

#[test]
fn step_function_is_monotonic_across_3_documented_tiers() {
    // PINS RELATIVE: low < medium < high — operator can
    // reason about cost increasing with effort.
    let low = adaptive_budget_for("low");
    let med = adaptive_budget_for("medium");
    let high = adaptive_budget_for("high");
    assert!(low < med, "low ({low}) MUST be < med ({med})");
    assert!(med < high, "med ({med}) MUST be < high ({high})");
}

#[test]
fn step_function_has_no_continuous_scale_between_documented_values() {
    // PINS STEP: there is NO interpolation between low/med/high.
    // "medium-low" or "medium-high" return 0 (unknown).
    assert_eq!(adaptive_budget_for("medium-low"), 0);
    assert_eq!(adaptive_budget_for("medium-high"), 0);
    assert_eq!(adaptive_budget_for("low-medium"), 0);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — ThinkingConfig defaults
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn default_thinking_enabled_is_true() {
    // PINS #599 DEFAULT: thinking enabled by default for
    // supported providers.
    let cfg = ThinkingConfig::default();
    assert!(cfg.enabled, "thinking MUST default to enabled");
}

#[test]
fn default_budget_tokens_is_none() {
    let cfg = ThinkingConfig::default();
    assert!(cfg.budget_tokens.is_none());
}

#[test]
fn default_preserve_across_turns_is_false() {
    let cfg = ThinkingConfig::default();
    assert!(!cfg.preserve_across_turns);
}

#[test]
fn default_reasoning_effort_is_none() {
    let cfg = ThinkingConfig::default();
    assert!(cfg.reasoning_effort.is_none());
}

#[test]
fn default_adaptive_is_true_cc_parity() {
    // PINS #599: adaptive default = true (CC parity).
    let cfg = ThinkingConfig::default();
    assert!(cfg.adaptive, "adaptive MUST default to true");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — effective_budget precedence (explicit > adaptive > default)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn explicit_budget_tokens_wins_over_adaptive() {
    let cfg = ThinkingConfig {
        enabled: true,
        budget_tokens: Some(9999),
        preserve_across_turns: false,
        reasoning_effort: Some("high".to_string()),
        adaptive: true,
    };
    // Explicit 9999 wins over adaptive's high=16000.
    assert_eq!(cfg.effective_budget(5000), 9999);
}

#[test]
fn explicit_budget_wins_over_provider_default() {
    let cfg = ThinkingConfig {
        budget_tokens: Some(7777),
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 7777);
}

#[test]
fn explicit_zero_budget_still_wins_over_default() {
    // PINS DOC: Some(0) is an EXPLICIT value, not absent.
    let cfg = ThinkingConfig {
        budget_tokens: Some(0),
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 0);
}

#[test]
fn adaptive_high_used_when_no_explicit_budget() {
    let cfg = ThinkingConfig {
        enabled: true,
        budget_tokens: None,
        preserve_across_turns: false,
        reasoning_effort: Some("high".to_string()),
        adaptive: true,
    };
    // Adaptive high = 16000 wins over provider_default 5000.
    assert_eq!(cfg.effective_budget(5000), 16000);
}

#[test]
fn adaptive_medium_used_when_no_explicit_budget() {
    let cfg = ThinkingConfig {
        budget_tokens: None,
        reasoning_effort: Some("medium".to_string()),
        adaptive: true,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 8000);
}

#[test]
fn adaptive_low_used_when_no_explicit_budget() {
    let cfg = ThinkingConfig {
        budget_tokens: None,
        reasoning_effort: Some("low".to_string()),
        adaptive: true,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 1024);
}

#[test]
fn provider_default_used_when_adaptive_disabled() {
    // PINS CONTRACT: adaptive=false → effort ignored, fall
    // straight to provider_default.
    let cfg = ThinkingConfig {
        budget_tokens: None,
        reasoning_effort: Some("high".to_string()),
        adaptive: false,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 5000);
}

#[test]
fn provider_default_used_when_no_effort_and_adaptive_on() {
    // PINS CONTRACT: adaptive=true but no effort set → default.
    let cfg = ThinkingConfig {
        budget_tokens: None,
        reasoning_effort: None,
        adaptive: true,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 5000);
}

#[test]
fn provider_default_used_when_effort_is_unknown_tier() {
    // PINS CONTRACT: adaptive_budget_for returns 0 for
    // unknown effort → falls through to provider_default.
    let cfg = ThinkingConfig {
        budget_tokens: None,
        reasoning_effort: Some("garbage".to_string()),
        adaptive: true,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(5000), 5000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Provider_default values
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn provider_default_zero_returns_zero() {
    let cfg = ThinkingConfig {
        budget_tokens: None,
        adaptive: false,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(0), 0);
}

#[test]
fn provider_default_huge_returns_huge() {
    let cfg = ThinkingConfig {
        budget_tokens: None,
        adaptive: false,
        ..ThinkingConfig::default()
    };
    assert_eq!(cfg.effective_budget(u32::MAX), u32::MAX);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Idempotency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn effective_budget_is_pure_function_no_caching() {
    let cfg = ThinkingConfig {
        budget_tokens: Some(1234),
        ..ThinkingConfig::default()
    };
    for _ in 0..5 {
        assert_eq!(cfg.effective_budget(5000), 1234);
    }
}
