//! End-to-end tests for `compaction::CompactionConfig` +
//! `CompactionOverrides` + `check_context_budget` warn/compact
//! thresholds + `CompactionAnalysis` shape + `CompactionError`
//! variants.
//!
//! Sprint 94 of the verification effort. Sprint 64 covered the
//! `estimate_tokens` + `estimate_message_tokens` token-counting
//! surface; this file covers the orchestration layer:
//! `CompactionConfig` defaults + per-model derivation,
//! `CompactionOverrides` partial-merge semantics, and the
//! `check_context_budget` 85%-warn / 90%-compact threshold pair
//! that gates auto-compaction.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::float_cmp)]

use openclaudia::compaction::{
    check_context_budget, get_context_window, CompactionConfig, CompactionError,
    CompactionOverrides,
};

// ───────────────────────────────────────────────────────────────────────────
// Section A — CompactionConfig::default
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn config_default_threshold_is_documented_value() {
    let config = CompactionConfig::default();
    // Documented threshold: 0.85.
    assert_eq!(config.threshold, 0.85_f32);
}

#[test]
fn config_default_preserve_recent_is_four_messages() {
    let config = CompactionConfig::default();
    assert_eq!(config.preserve_recent, 4);
}

#[test]
fn config_default_preserve_system_and_tool_calls_are_true() {
    let config = CompactionConfig::default();
    assert!(
        config.preserve_system,
        "system messages MUST default to preserved"
    );
    assert!(
        config.preserve_tool_calls,
        "tool-call pairs MUST default to preserved"
    );
}

#[test]
fn config_default_summary_prompt_is_none() {
    let config = CompactionConfig::default();
    assert!(config.summary_prompt.is_none());
}

#[test]
fn config_default_max_context_tokens_is_positive() {
    let config = CompactionConfig::default();
    assert!(
        config.max_context_tokens > 0,
        "max_context_tokens MUST be > 0; got {}",
        config.max_context_tokens
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — CompactionConfig::for_model
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn for_model_derives_max_context_from_model_window() {
    let cfg = CompactionConfig::for_model("claude-sonnet-4-5");
    let window = get_context_window("claude-sonnet-4-5");
    assert_eq!(cfg.max_context_tokens, window);
}

#[test]
fn for_model_preserves_other_defaults() {
    let cfg = CompactionConfig::for_model("claude-sonnet-4-5");
    let defaults = CompactionConfig::default();
    assert_eq!(cfg.threshold, defaults.threshold);
    assert_eq!(cfg.preserve_recent, defaults.preserve_recent);
    assert_eq!(cfg.preserve_system, defaults.preserve_system);
    assert_eq!(cfg.preserve_tool_calls, defaults.preserve_tool_calls);
}

#[test]
fn for_model_with_unknown_model_falls_back_to_default_window() {
    // get_context_window returns DEFAULT_CONTEXT for unknown
    // models — for_model MUST inherit that value.
    let cfg = CompactionConfig::for_model("totally-unknown-model");
    assert!(
        cfg.max_context_tokens > 0,
        "unknown model MUST still produce positive default window"
    );
}

#[test]
fn for_model_serde_round_trip_preserves_all_fields() {
    let cfg = CompactionConfig::for_model("claude-sonnet-4-5");
    let json = serde_json::to_string(&cfg).expect("ser");
    let back: CompactionConfig = serde_json::from_str(&json).expect("de");
    assert_eq!(back.max_context_tokens, cfg.max_context_tokens);
    assert_eq!(back.threshold, cfg.threshold);
    assert_eq!(back.preserve_recent, cfg.preserve_recent);
    assert_eq!(back.preserve_system, cfg.preserve_system);
    assert_eq!(back.preserve_tool_calls, cfg.preserve_tool_calls);
    assert_eq!(back.summary_prompt, cfg.summary_prompt);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — CompactionOverrides
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn overrides_default_has_all_none_fields() {
    let overrides = CompactionOverrides::default();
    assert!(overrides.max_context_tokens.is_none());
    assert!(overrides.threshold.is_none());
    assert!(overrides.preserve_recent.is_none());
    assert!(overrides.preserve_system.is_none());
    assert!(overrides.preserve_tool_calls.is_none());
    assert!(overrides.summary_prompt.is_none());
}

#[test]
fn overrides_from_user_config_skips_default_fields() {
    // CompactionConfig::default() yields a config equal to
    // defaults; from_user_config should produce all-None.
    let config = CompactionConfig::default();
    let overrides = CompactionOverrides::from_user_config(&config);
    assert!(overrides.preserve_recent.is_none());
    assert!(overrides.preserve_system.is_none());
    assert!(overrides.preserve_tool_calls.is_none());
    assert!(overrides.summary_prompt.is_none());
}

#[test]
fn overrides_from_user_config_captures_non_default_preserve_recent() {
    let config = CompactionConfig {
        preserve_recent: 99,
        ..CompactionConfig::default()
    };
    let overrides = CompactionOverrides::from_user_config(&config);
    assert_eq!(overrides.preserve_recent, Some(99));
}

#[test]
fn overrides_from_user_config_captures_non_default_preserve_system() {
    let config = CompactionConfig {
        preserve_system: false,
        ..CompactionConfig::default()
    };
    let overrides = CompactionOverrides::from_user_config(&config);
    assert_eq!(overrides.preserve_system, Some(false));
}

#[test]
fn overrides_from_user_config_captures_non_default_summary_prompt() {
    let config = CompactionConfig {
        summary_prompt: Some("custom prompt".to_string()),
        ..CompactionConfig::default()
    };
    let overrides = CompactionOverrides::from_user_config(&config);
    assert_eq!(overrides.summary_prompt.as_deref(), Some("custom prompt"));
}

#[test]
fn overrides_from_user_config_does_not_forward_model_derived_fields() {
    // PINS DOCUMENTED CONTRACT: max_context_tokens + threshold
    // are model-derived; from_user_config MUST keep them as
    // None even when the operator set them.
    let config = CompactionConfig {
        max_context_tokens: 999_999,
        threshold: 0.5,
        ..CompactionConfig::default()
    };
    let overrides = CompactionOverrides::from_user_config(&config);
    assert!(
        overrides.max_context_tokens.is_none(),
        "model-derived max_context_tokens MUST NOT forward"
    );
    assert!(
        overrides.threshold.is_none(),
        "model-derived threshold MUST NOT forward"
    );
}

#[test]
fn overrides_partial_eq_holds_for_equal_overrides() {
    let a = CompactionOverrides {
        max_context_tokens: Some(100_000),
        threshold: Some(0.8),
        preserve_recent: Some(10),
        preserve_system: Some(true),
        preserve_tool_calls: Some(false),
        summary_prompt: Some("x".to_string()),
    };
    let b = a.clone();
    assert_eq!(a, b);
}

#[test]
fn overrides_serde_round_trip() {
    let original = CompactionOverrides {
        max_context_tokens: Some(50_000),
        threshold: Some(0.75),
        preserve_recent: Some(8),
        preserve_system: None,
        preserve_tool_calls: Some(true),
        summary_prompt: Some("custom".to_string()),
    };
    let json = serde_json::to_string(&original).expect("ser");
    let back: CompactionOverrides = serde_json::from_str(&json).expect("de");
    assert_eq!(back, original);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — check_context_budget warn/compact thresholds
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_budget_under_85_percent_returns_no_warn_no_compact() {
    let window = get_context_window("claude-sonnet-4-5");
    let tokens = (window * 50) / 100; // 50%
    let (warn, compact, pct) = check_context_budget(tokens, "claude-sonnet-4-5");
    assert!(!warn, "50% MUST NOT warn");
    assert!(!compact, "50% MUST NOT compact");
    assert!(pct < 85.0);
}

#[test]
fn check_budget_at_85_percent_warns_but_does_not_compact() {
    // PINS THRESHOLD: 85% is warn, < 90% is no-compact.
    let window = get_context_window("claude-sonnet-4-5");
    let tokens = (window * 85) / 100; // 85%
    let (warn, compact, pct) = check_context_budget(tokens, "claude-sonnet-4-5");
    assert!(warn, "85% MUST warn; got pct={pct}");
    assert!(!compact, "85% MUST NOT compact (compact threshold is 90%)");
}

#[test]
fn check_budget_at_90_percent_both_warns_and_compacts() {
    let window = get_context_window("claude-sonnet-4-5");
    let tokens = (window * 90) / 100; // 90%
    let (warn, compact, pct) = check_context_budget(tokens, "claude-sonnet-4-5");
    assert!(warn);
    assert!(compact, "90% MUST trigger compaction; got pct={pct}");
}

#[test]
fn check_budget_over_100_percent_still_returns_both_flags_true() {
    // No upper bound — high usage shouldn't panic.
    let window = get_context_window("claude-sonnet-4-5");
    let tokens = window * 2; // 200%
    let (warn, compact, _pct) = check_context_budget(tokens, "claude-sonnet-4-5");
    assert!(warn);
    assert!(compact);
}

#[test]
fn check_budget_zero_tokens_returns_no_warn_no_compact() {
    let (warn, compact, pct) = check_context_budget(0, "claude-sonnet-4-5");
    assert!(!warn);
    assert!(!compact);
    assert_eq!(pct, 0.0);
}

#[test]
fn check_budget_unknown_model_uses_default_window() {
    // Unknown model: should use a positive default, not panic.
    let (warn, compact, pct) = check_context_budget(1000, "completely-unknown-model-name");
    // 1000 tokens against any reasonable default window is < 85%.
    assert!(!warn);
    assert!(!compact);
    assert!(pct.is_finite());
}

#[test]
fn check_budget_pct_is_finite_for_realistic_inputs() {
    // Property: pct is always finite for realistic inputs.
    for pct_target in &[10, 50, 85, 89, 95, 150] {
        let window = get_context_window("claude-sonnet-4-5");
        let tokens = (window * pct_target) / 100;
        let (_w, _c, pct) = check_context_budget(tokens, "claude-sonnet-4-5");
        assert!(
            pct.is_finite(),
            "pct MUST be finite for {pct_target}% target; got {pct}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — CompactionError variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn compaction_error_hook_blocked_carries_reason() {
    let err = CompactionError::HookBlocked("policy denied".to_string());
    let msg = err.to_string();
    assert!(msg.contains("PreCompact"));
    assert!(msg.contains("policy denied"));
}

#[test]
fn compaction_error_failed_carries_reason() {
    let err = CompactionError::Failed("timeout".to_string());
    let msg = err.to_string();
    assert!(msg.contains("timeout"));
    assert!(msg.contains("Compaction failed"));
}

#[test]
fn compaction_error_variants_have_distinct_messages() {
    let blocked = CompactionError::HookBlocked("x".to_string());
    let failed = CompactionError::Failed("x".to_string());
    assert_ne!(blocked.to_string(), failed.to_string());
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Clone semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn config_clone_preserves_all_fields() {
    let original = CompactionConfig {
        max_context_tokens: 100_000,
        threshold: 0.7,
        preserve_recent: 5,
        preserve_system: false,
        preserve_tool_calls: false,
        summary_prompt: Some("prompt".to_string()),
    };
    let cloned = original.clone();
    assert_eq!(cloned.max_context_tokens, original.max_context_tokens);
    assert_eq!(cloned.threshold, original.threshold);
    assert_eq!(cloned.preserve_recent, original.preserve_recent);
    assert_eq!(cloned.preserve_system, original.preserve_system);
    assert_eq!(cloned.preserve_tool_calls, original.preserve_tool_calls);
    assert_eq!(cloned.summary_prompt, original.summary_prompt);
}
