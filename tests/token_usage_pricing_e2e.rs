//! End-to-end tests for `TokenUsage` arithmetic, `UsageExtras`
//! accumulation, and the pricing math across the documented
//! cost-calculation entry points.
//!
//! Sprint 35 of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::session::{
    calculate_cost, calculate_cost_fast_mode, calculate_cost_with_extras, calculate_cost_with_ttl,
    get_pricing, CacheWriteTtl, PricingError, TokenUsage, UsageExtras,
};

// ───────────────────────────────────────────────────────────────────────────
// Section A — TokenUsage arithmetic
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn token_usage_total_sums_input_and_output_only() {
    // total() is documented as input + output ONLY — cache
    // tokens are billed at reduced multipliers and aren't
    // counted in the "headline" total used by stats summaries.
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 200,
        cache_write_tokens: 75,
    };
    assert_eq!(
        usage.total(),
        150,
        "total() must NOT include cache tokens (input + output only)"
    );
}

#[test]
fn token_usage_accumulate_adds_each_field_componentwise() {
    let mut acc = TokenUsage {
        input_tokens: 10,
        output_tokens: 20,
        cache_read_tokens: 30,
        cache_write_tokens: 40,
    };
    let next = TokenUsage {
        input_tokens: 1,
        output_tokens: 2,
        cache_read_tokens: 3,
        cache_write_tokens: 4,
    };
    acc.accumulate(&next);
    assert_eq!(acc.input_tokens, 11);
    assert_eq!(acc.output_tokens, 22);
    assert_eq!(acc.cache_read_tokens, 33);
    assert_eq!(acc.cache_write_tokens, 44);
}

#[test]
fn token_usage_default_is_all_zero() {
    let usage = TokenUsage::default();
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 0);
    assert_eq!(usage.total(), 0);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — UsageExtras
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn usage_extras_zero_constant_equals_default() {
    assert_eq!(UsageExtras::ZERO, UsageExtras::default());
}

#[test]
fn usage_extras_accumulate_is_noop_for_empty_metadata() {
    let mut acc = UsageExtras::default();
    let next = UsageExtras::ZERO;
    acc.accumulate(&next);
    assert_eq!(acc, UsageExtras::ZERO);
}

#[test]
fn usage_extras_ignores_legacy_web_search_requests_json() {
    let extras: UsageExtras =
        serde_json::from_str(r#"{"web_search_requests":5}"#).expect("legacy extras deserialize");
    assert_eq!(extras, UsageExtras::ZERO);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — get_pricing dispatch
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_pricing_resolves_well_known_anthropic_models() {
    for model in &[
        "claude-3-5-sonnet-20241022",
        "claude-3-opus-20240229",
        "claude-3-haiku-20240307",
    ] {
        let pricing = get_pricing(model);
        assert!(pricing.is_some(), "{model} MUST have a pricing entry");
        let p = pricing.unwrap();
        assert!(
            p.input_per_million > 0.0,
            "{model} input rate must be positive; got {}",
            p.input_per_million
        );
        assert!(
            p.output_per_million > p.input_per_million,
            "{model} output rate ({}) MUST exceed input rate ({}); LLM pricing convention",
            p.output_per_million,
            p.input_per_million
        );
    }
}

#[test]
fn get_pricing_is_case_insensitive() {
    // Use bit-level comparison via `to_bits` to satisfy
    // clippy::float_cmp — the lookups return the same
    // `ModelPricing` value object, so the f64 fields must be
    // bit-identical (no arithmetic to introduce rounding).
    let lower = get_pricing("claude-3-5-sonnet-20241022").expect("lower");
    let upper = get_pricing("CLAUDE-3-5-SONNET-20241022").expect("upper");
    let mixed = get_pricing("Claude-3-5-Sonnet-20241022").expect("mixed");
    assert_eq!(
        lower.input_per_million.to_bits(),
        upper.input_per_million.to_bits(),
        "case must not affect price lookup"
    );
    assert_eq!(
        lower.input_per_million.to_bits(),
        mixed.input_per_million.to_bits(),
    );
}

#[test]
fn get_pricing_returns_none_for_unknown_model() {
    let pricing = get_pricing("totally-unknown-model-xyz-2099");
    assert!(pricing.is_none());
}

#[test]
fn anthropic_cache_multipliers_match_documented_industry_constants() {
    let p = get_pricing("claude-3-5-sonnet-20241022").expect("sonnet pricing");
    // Documented Anthropic constants:
    //  - cache_read: 0.1×
    //  - cache_write_5m: 1.25×
    //  - cache_write_1h: 2.0×
    let eps = 1e-9;
    assert!(
        (p.cache_read_multiplier - 0.1).abs() < eps,
        "cache_read must be 0.1×; got {}",
        p.cache_read_multiplier
    );
    assert!(
        (p.cache_write_5m_multiplier - 1.25).abs() < eps,
        "cache_write_5m must be 1.25×; got {}",
        p.cache_write_5m_multiplier
    );
    assert!(
        (p.cache_write_1hr_multiplier - 2.0).abs() < eps,
        "cache_write_1h must be 2.0×; got {}",
        p.cache_write_1hr_multiplier
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — calculate_cost variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn calculate_cost_known_model_succeeds() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let outcome = calculate_cost("claude-3-5-sonnet-20241022", &usage);
    let cost = outcome.expect("known model must succeed");
    assert!(cost > 0.0, "cost must be positive; got {cost}");
    // Sanity: 1M input + 1M output = input_rate + output_rate
    let p = get_pricing("claude-3-5-sonnet-20241022").unwrap();
    let expected = p.input_per_million + p.output_per_million;
    assert!(
        (cost - expected).abs() < 1e-6,
        "cost must equal input_rate + output_rate; got {cost}, expected {expected}"
    );
}

#[test]
fn calculate_cost_unknown_model_returns_pricing_error() {
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let outcome = calculate_cost("totally-unknown-model-xyz-2099", &usage);
    assert!(
        matches!(outcome, Err(PricingError::UnknownModel { .. })),
        "unknown model MUST return UnknownModel error; got {outcome:?}"
    );
}

#[test]
fn calculate_cost_with_ttl_one_hour_costs_more_than_five_minutes() {
    let usage = TokenUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 1_000_000,
    };
    let short_ttl_cost = calculate_cost_with_ttl(
        "claude-3-5-sonnet-20241022",
        &usage,
        CacheWriteTtl::FiveMinutes,
    )
    .expect("5m ttl");
    let long_ttl_cost =
        calculate_cost_with_ttl("claude-3-5-sonnet-20241022", &usage, CacheWriteTtl::OneHour)
            .expect("1h ttl");
    // 1h write costs ~1.6× the 5m write (2.0/1.25 ratio).
    assert!(
        long_ttl_cost > short_ttl_cost,
        "1h cache-write MUST cost more than 5m cache-write; \
         got short_ttl={short_ttl_cost}, long_ttl={long_ttl_cost}"
    );
}

#[test]
fn calculate_cost_with_extras_preserves_token_cost() {
    let usage = TokenUsage {
        input_tokens: 100_000,
        output_tokens: 10_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let base = calculate_cost("claude-3-5-sonnet-20241022", &usage).expect("base");
    let with_extras =
        calculate_cost_with_extras("claude-3-5-sonnet-20241022", &usage, &UsageExtras::ZERO)
            .expect("with extras");
    assert!(
        (with_extras - base).abs() < 1e-9,
        "extras must not change token pricing; got base={base}, with_extras={with_extras}"
    );
}

#[test]
fn calculate_cost_fast_mode_falls_back_to_standard_for_models_without_fast_tier() {
    // For models that don't have a fast-mode rate (most), the
    // fast-mode cost equals the standard cost.
    let usage = TokenUsage {
        input_tokens: 1_000,
        output_tokens: 1_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    // claude-3-5-sonnet-20241022 has no fast-mode tier (only
    // Opus 4.6+ does). Standard and fast costs must equal.
    let std_cost = calculate_cost("claude-3-5-sonnet-20241022", &usage).expect("std");
    let fast_cost = calculate_cost_fast_mode("claude-3-5-sonnet-20241022", &usage).expect("fast");
    assert!(
        (std_cost - fast_cost).abs() < 1e-9,
        "models without fast tier: fast == standard; got std={std_cost}, fast={fast_cost}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — cache-write 5m vs 1h selection math
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn cache_write_5m_cost_uses_documented_multiplier() {
    // 1M cache-write tokens at 5m TTL = input_rate × 1.25.
    let usage = TokenUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 1_000_000,
    };
    let cost = calculate_cost_with_ttl(
        "claude-3-5-sonnet-20241022",
        &usage,
        CacheWriteTtl::FiveMinutes,
    )
    .expect("cost");
    let p = get_pricing("claude-3-5-sonnet-20241022").unwrap();
    let expected = p.input_per_million * p.cache_write_5m_multiplier;
    assert!(
        (cost - expected).abs() < 1e-6,
        "5m cache-write cost: 1M × rate × multiplier; got {cost}, expected {expected}"
    );
}

#[test]
fn cache_read_cost_uses_documented_multiplier() {
    // 1M cache-read tokens = input_rate × 0.1.
    let usage = TokenUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 1_000_000,
        cache_write_tokens: 0,
    };
    let cost = calculate_cost("claude-3-5-sonnet-20241022", &usage).expect("cost");
    let p = get_pricing("claude-3-5-sonnet-20241022").unwrap();
    let expected = p.input_per_million * p.cache_read_multiplier;
    assert!(
        (cost - expected).abs() < 1e-6,
        "cache-read cost: 1M × rate × 0.1; got {cost}, expected {expected}"
    );
}

#[test]
fn cost_breakdown_is_additive_across_token_kinds() {
    // Cost(input + output + cache_read + cache_write) =
    //   Cost(input only) + Cost(output only) + Cost(cache_read only) + Cost(cache_write only).
    // (Linear cost model — no hidden cross-term.)
    let model = "claude-3-5-sonnet-20241022";
    let total = TokenUsage {
        input_tokens: 100_000,
        output_tokens: 50_000,
        cache_read_tokens: 200_000,
        cache_write_tokens: 30_000,
    };
    let only_input = TokenUsage {
        input_tokens: total.input_tokens,
        ..TokenUsage::default()
    };
    let only_output = TokenUsage {
        output_tokens: total.output_tokens,
        ..TokenUsage::default()
    };
    let only_cread = TokenUsage {
        cache_read_tokens: total.cache_read_tokens,
        ..TokenUsage::default()
    };
    let only_cwrite = TokenUsage {
        cache_write_tokens: total.cache_write_tokens,
        ..TokenUsage::default()
    };

    let total_cost = calculate_cost(model, &total).expect("total");
    let sum_cost = calculate_cost(model, &only_input).expect("in")
        + calculate_cost(model, &only_output).expect("out")
        + calculate_cost(model, &only_cread).expect("cread")
        + calculate_cost(model, &only_cwrite).expect("cwrite");

    assert!(
        (total_cost - sum_cost).abs() < 1e-6,
        "cost MUST be additive across token kinds; got total={total_cost}, sum={sum_cost}"
    );
}
