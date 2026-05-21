//! End-to-end tests for `StopConditionsConfig` predicate semantics
//! + `TokenTotals` arithmetic + `StopReason` rendering.
//!
//! Sprint 38 of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::{StopConditionsConfig, StopReason, TokenTotals};

// ───────────────────────────────────────────────────────────────────────────
// Section A — TokenTotals arithmetic
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn token_totals_combined_sums_input_and_output() {
    let t = TokenTotals {
        input: 100,
        output: 250,
    };
    assert_eq!(t.combined(), 350);
}

#[test]
fn token_totals_combined_saturates_at_u64_max() {
    // .combined() uses saturating_add so an attacker-controlled
    // upstream emitting absurd usage counts can't overflow into
    // wraparound (which would silently bypass the cap check).
    let t = TokenTotals {
        input: u64::MAX,
        output: 1,
    };
    assert_eq!(
        t.combined(),
        u64::MAX,
        "saturating: MAX + 1 stays at MAX, never wraps to 0"
    );
}

#[test]
fn token_totals_default_is_zero_zero() {
    let t = TokenTotals::default();
    assert_eq!(t.input, 0);
    assert_eq!(t.output, 0);
    assert_eq!(t.combined(), 0);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — StopReason rendering contract
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn stop_reason_as_str_matches_serde_rename_all_snake_case() {
    // Per the impl docstring: `as_str` must equal the
    // serde-snake-case rendering byte-for-byte so the event
    // log and the enum agree.
    let cases = &[
        (StopReason::InputTokenBudget, "input_token_budget"),
        (StopReason::OutputTokenBudget, "output_token_budget"),
        (StopReason::TotalTokenBudget, "total_token_budget"),
    ];
    for (reason, expected) in cases {
        assert_eq!(reason.as_str(), *expected, "{reason:?} as_str");
        // serde-JSON serializes a serde-snake_case enum value
        // as a quoted string; strip the quotes to compare.
        let json = serde_json::to_string(reason).expect("serialize");
        let unquoted = json.trim_matches('"');
        assert_eq!(
            unquoted, *expected,
            "{reason:?} serde rendering must equal as_str"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — is_inactive + default
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn default_config_is_inactive() {
    let cfg = StopConditionsConfig::default();
    assert!(cfg.is_inactive(), "all-None config must be inactive");
    // And the predicate returns None for any totals.
    assert!(cfg.is_met(TokenTotals::default()).is_none());
    assert!(
        cfg.is_met(TokenTotals {
            input: u64::MAX,
            output: u64::MAX,
        })
        .is_none(),
        "inactive config MUST never trip even at saturated totals"
    );
}

#[test]
fn any_single_cap_set_makes_config_active() {
    for cfg in &[
        StopConditionsConfig {
            max_total_input_tokens: Some(1),
            ..Default::default()
        },
        StopConditionsConfig {
            max_total_output_tokens: Some(1),
            ..Default::default()
        },
        StopConditionsConfig {
            max_total_tokens: Some(1),
            ..Default::default()
        },
    ] {
        assert!(
            !cfg.is_inactive(),
            "config with one cap set MUST report active; got {cfg:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — is_met predicate (single-cap cases)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn at_exact_cap_does_not_trigger_strict_greater_than() {
    let cfg = StopConditionsConfig {
        max_total_input_tokens: Some(1000),
        ..Default::default()
    };
    // 999, 1000 → admit; 1001 → trip.
    assert!(cfg
        .is_met(TokenTotals {
            input: 999,
            output: 0
        })
        .is_none());
    assert!(
        cfg.is_met(TokenTotals {
            input: 1000,
            output: 0
        })
        .is_none(),
        "exactly-cap MUST admit (strict >, not >=)"
    );
    assert_eq!(
        cfg.is_met(TokenTotals {
            input: 1001,
            output: 0
        }),
        Some(StopReason::InputTokenBudget),
        "cap+1 MUST trip"
    );
}

#[test]
fn output_cap_trips_only_on_output_overflow() {
    let cfg = StopConditionsConfig {
        max_total_output_tokens: Some(500),
        ..Default::default()
    };
    // Input alone, however large, MUST NOT trip the output cap.
    assert!(
        cfg.is_met(TokenTotals {
            input: 10_000_000,
            output: 0
        })
        .is_none(),
        "input-only spend MUST NOT trip output cap"
    );
    // Output overflow trips.
    assert_eq!(
        cfg.is_met(TokenTotals {
            input: 0,
            output: 501
        }),
        Some(StopReason::OutputTokenBudget)
    );
}

#[test]
fn total_cap_uses_combined_sum() {
    let cfg = StopConditionsConfig {
        max_total_tokens: Some(1000),
        ..Default::default()
    };
    // Neither input nor output alone is past 1000, but combined
    // they exceed.
    assert_eq!(
        cfg.is_met(TokenTotals {
            input: 600,
            output: 500
        }),
        Some(StopReason::TotalTokenBudget),
        "combined=1100 > 1000 MUST trip"
    );
    // Exactly-cap (1000 + 0) admits.
    assert!(cfg
        .is_met(TokenTotals {
            input: 1000,
            output: 0
        })
        .is_none());
    // Split 500+500 admits.
    assert!(cfg
        .is_met(TokenTotals {
            input: 500,
            output: 500
        })
        .is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — is_met priority order (input → output → total)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn input_budget_takes_priority_when_multiple_caps_tripped() {
    // Per the impl docstring: priority order is
    // input → output → total. When more than one cap is
    // tripped simultaneously, the test must observe the FIRST
    // in priority order.
    let cfg = StopConditionsConfig {
        max_total_input_tokens: Some(100),
        max_total_output_tokens: Some(100),
        max_total_tokens: Some(100),
    };
    // Input=101, output=101 — all three caps tripped.
    // Priority order says InputTokenBudget wins.
    assert_eq!(
        cfg.is_met(TokenTotals {
            input: 101,
            output: 101
        }),
        Some(StopReason::InputTokenBudget),
        "input cap MUST be reported first when all tripped"
    );
}

#[test]
fn output_budget_takes_priority_over_total_when_input_under_cap() {
    let cfg = StopConditionsConfig {
        max_total_input_tokens: Some(1_000_000),
        max_total_output_tokens: Some(100),
        max_total_tokens: Some(100),
    };
    // Input=0 (under), output=101 (trips output AND total
    // because combined=101>100). Output wins over total per
    // priority order.
    assert_eq!(
        cfg.is_met(TokenTotals {
            input: 0,
            output: 101
        }),
        Some(StopReason::OutputTokenBudget),
        "output cap MUST be reported before total when input is under cap"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — YAML round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn yaml_round_trip_preserves_every_set_field() {
    let yaml = r"
max_total_input_tokens: 100000
max_total_output_tokens: 50000
max_total_tokens: 200000
";
    let cfg: StopConditionsConfig = serde_yaml::from_str(yaml).expect("yaml parses");
    assert_eq!(cfg.max_total_input_tokens, Some(100_000));
    assert_eq!(cfg.max_total_output_tokens, Some(50_000));
    assert_eq!(cfg.max_total_tokens, Some(200_000));
    assert!(!cfg.is_inactive());
}

#[test]
fn empty_yaml_block_is_inactive_with_skip_serialize_if_none() {
    let cfg: StopConditionsConfig = serde_yaml::from_str("{}").expect("empty parses");
    assert!(cfg.is_inactive());
    // And re-serializing the empty config produces an empty
    // mapping because every field is `skip_serializing_if =
    // "Option::is_none"`.
    let yaml = serde_yaml::to_string(&cfg).expect("serialize");
    let trimmed = yaml.trim();
    // Empty struct yields "{}" or similar — assert no token
    // names leak into the output.
    assert!(
        !trimmed.contains("max_total_input_tokens")
            && !trimmed.contains("max_total_output_tokens")
            && !trimmed.contains("max_total_tokens"),
        "inactive config must round-trip empty; got {trimmed:?}"
    );
}

#[test]
fn partial_yaml_only_sets_named_field() {
    let yaml = "max_total_output_tokens: 1000";
    let cfg: StopConditionsConfig = serde_yaml::from_str(yaml).expect("yaml parses");
    assert_eq!(cfg.max_total_input_tokens, None);
    assert_eq!(cfg.max_total_output_tokens, Some(1000));
    assert_eq!(cfg.max_total_tokens, None);
    // Only output cap → only output overflow trips.
    assert!(cfg
        .is_met(TokenTotals {
            input: u64::MAX,
            output: 1000
        })
        .is_none());
    assert_eq!(
        cfg.is_met(TokenTotals {
            input: 0,
            output: 1001
        }),
        Some(StopReason::OutputTokenBudget)
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — saturated totals interact correctly with the predicate
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn saturated_totals_trip_total_cap_via_saturating_combined() {
    let cfg = StopConditionsConfig {
        max_total_tokens: Some(1_000_000),
        ..Default::default()
    };
    // combined() saturates at u64::MAX, which exceeds 1M.
    let saturated = TokenTotals {
        input: u64::MAX,
        output: 1,
    };
    assert_eq!(
        cfg.is_met(saturated),
        Some(StopReason::TotalTokenBudget),
        "saturated combined() MUST trip the total cap"
    );
}
