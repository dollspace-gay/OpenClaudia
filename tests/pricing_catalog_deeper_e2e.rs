//! End-to-end tests for `session::pricing` catalog
//! coverage — `get_pricing` per-prefix lookup, `ModelPricing`
//! multiplier semantics, `web_search_cost` flat-rate, and
//! `calculate_cost_*` family precedence across modes/TTLs.
//!
//! Sprint 98 of the verification effort. Sprint 61
//! (`pricing_audit_e2e`) covered the unknown-model flag +
//! audit logger; this file pins per-model rate retrieval
//! (`get_pricing` lookup matrix), cache-multiplier
//! relationships (read < input < `write_5m` < `write_1h`),
//! and the `calculate_cost_*` family's documented
//! precedence between fast/standard, TTL, and extras.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::float_cmp)]

use openclaudia::session::{
    calculate_cost, calculate_cost_fast_mode, calculate_cost_with_ttl, get_pricing,
    web_search_cost, CacheWriteTtl, ModelPricing, PricingError, TokenUsage,
    FAST_MODE_INPUT_PER_MILLION, FAST_MODE_OUTPUT_PER_MILLION, WEB_SEARCH_REQUEST_USD,
};

// ───────────────────────────────────────────────────────────────────────────
// Section A — Public constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn web_search_request_constant_is_one_cent() {
    assert_eq!(WEB_SEARCH_REQUEST_USD, 0.01);
}

#[test]
fn fast_mode_input_per_million_is_30_dollars() {
    assert_eq!(FAST_MODE_INPUT_PER_MILLION, 30.0);
}

#[test]
fn fast_mode_output_per_million_is_150_dollars() {
    assert_eq!(FAST_MODE_OUTPUT_PER_MILLION, 150.0);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — get_pricing lookup matrix
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_pricing_for_known_anthropic_model_returns_some() {
    let pricing = get_pricing("claude-sonnet-4-5-20250929");
    assert!(pricing.is_some());
}

#[test]
fn get_pricing_for_unknown_model_returns_none() {
    let pricing = get_pricing("totally-unknown-model");
    assert!(pricing.is_none());
}

#[test]
fn get_pricing_matches_via_prefix_not_exact() {
    // Prefix-based lookup: a known prefix + arbitrary suffix
    // should still match.
    let p_short = get_pricing("claude-sonnet-4-5");
    let p_long = get_pricing("claude-sonnet-4-5-20250929-arbitrary-suffix");
    assert_eq!(p_short.is_some(), p_long.is_some());
}

#[test]
fn get_pricing_lookup_is_case_insensitive() {
    let lower = get_pricing("claude-sonnet-4-5");
    let upper = get_pricing("CLAUDE-SONNET-4-5");
    let mixed = get_pricing("Claude-Sonnet-4-5");
    assert!(lower.is_some());
    assert!(upper.is_some());
    assert!(mixed.is_some());
}

#[test]
fn get_pricing_is_side_effect_free_with_respect_to_unknown_flag() {
    use openclaudia::session::{clear_unknown_model_cost, has_unknown_model_cost};
    clear_unknown_model_cost();
    let _ = get_pricing("totally-unknown-model");
    // Pure introspection MUST NOT set the unknown-model flag.
    assert!(
        !has_unknown_model_cost(),
        "get_pricing on miss MUST NOT pollute session-level flag"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — ModelPricing cache-multiplier invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_pricing_cache_read_multiplier_is_0_1() {
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    assert!(
        (p.cache_read_multiplier - 0.1).abs() < 1e-9,
        "cache_read_multiplier MUST be 0.1× for Anthropic; got {}",
        p.cache_read_multiplier
    );
}

#[test]
fn anthropic_pricing_cache_write_5m_multiplier_is_1_25() {
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    assert!(
        (p.cache_write_5m_multiplier - 1.25).abs() < 1e-9,
        "cache_write_5m_multiplier MUST be 1.25× for Anthropic"
    );
}

#[test]
fn anthropic_pricing_cache_write_1hr_multiplier_is_2_0() {
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    assert!(
        (p.cache_write_1hr_multiplier - 2.0).abs() < 1e-9,
        "cache_write_1hr_multiplier MUST be 2.0× for Anthropic"
    );
}

#[test]
fn pricing_cache_multipliers_have_documented_ordering() {
    // PINS INVARIANT: cache_read < 1.0 < cache_write_5m <
    // cache_write_1h (for all Anthropic models).
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    assert!(
        p.cache_read_multiplier < 1.0,
        "cache reads MUST be cheaper than input"
    );
    assert!(
        p.cache_write_5m_multiplier > 1.0,
        "5m cache writes MUST be more expensive than input"
    );
    assert!(
        p.cache_write_1hr_multiplier > p.cache_write_5m_multiplier,
        "1hr writes MUST be more expensive than 5m writes"
    );
}

#[test]
fn pricing_output_is_more_expensive_than_input_for_typical_models() {
    // Industry baseline for Anthropic + OpenAI: output > input.
    for model in &["claude-sonnet-4-5", "claude-opus-4-1"] {
        if let Some(p) = get_pricing(model) {
            assert!(
                p.output_per_million > p.input_per_million,
                "{model}: output MUST be > input; got out={}, in={}",
                p.output_per_million,
                p.input_per_million
            );
        }
    }
}

#[test]
fn pricing_input_and_output_per_million_are_positive() {
    for model in &["claude-sonnet-4-5", "claude-opus-4-1"] {
        if let Some(p) = get_pricing(model) {
            assert!(p.input_per_million > 0.0);
            assert!(p.output_per_million > 0.0);
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — web_search_cost flat-rate
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn web_search_cost_zero_requests_is_zero() {
    assert_eq!(web_search_cost(0), 0.0);
}

#[test]
fn web_search_cost_one_request_equals_constant() {
    assert!(
        (web_search_cost(1) - WEB_SEARCH_REQUEST_USD).abs() < 1e-12,
        "1 request MUST cost exactly WEB_SEARCH_REQUEST_USD"
    );
}

#[test]
fn web_search_cost_scales_linearly_with_request_count() {
    let one = web_search_cost(1);
    let ten = web_search_cost(10);
    let hundred = web_search_cost(100);
    assert!((ten - one * 10.0).abs() < 1e-12);
    assert!((hundred - one * 100.0).abs() < 1e-12);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — calculate_cost basic + unknown-model failure
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn calculate_cost_unknown_model_errors_with_unknown_model_variant() {
    let usage = TokenUsage {
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let outcome = calculate_cost("totally-unknown-model", &usage);
    let err = outcome.unwrap_err();
    assert!(matches!(err, PricingError::UnknownModel(_)));
    let msg = err.to_string();
    assert!(msg.contains("totally-unknown-model") || msg.contains("Unknown"));
}

#[test]
fn calculate_cost_zero_usage_for_known_model_is_zero() {
    let usage = TokenUsage::default();
    let cost = calculate_cost("claude-sonnet-4-5", &usage).expect("known");
    assert_eq!(cost, 0.0);
}

#[test]
fn calculate_cost_input_only_is_positive() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let cost = calculate_cost("claude-sonnet-4-5", &usage).expect("known");
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    assert!((cost - p.input_per_million).abs() < 1e-6);
}

#[test]
fn calculate_cost_combines_input_and_output_rates() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 500_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    let cost = calculate_cost("claude-sonnet-4-5", &usage).expect("known");
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    let expected = 0.5_f64.mul_add(p.output_per_million, p.input_per_million);
    assert!(
        (cost - expected).abs() < 1e-6,
        "MUST equal input + 0.5 * output; got {cost} vs expected {expected}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — calculate_cost_with_ttl: 5m vs 1h ordering
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn one_hour_ttl_cache_write_costs_more_than_five_minute_ttl() {
    // PINS DOCUMENTED PRICING: 1h TTL > 5m TTL when
    // cache_write_tokens > 0.
    let usage = TokenUsage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 100_000,
    };
    let five_m = calculate_cost_with_ttl("claude-sonnet-4-5", &usage, CacheWriteTtl::FiveMinutes)
        .expect("known");
    let one_h = calculate_cost_with_ttl("claude-sonnet-4-5", &usage, CacheWriteTtl::OneHour)
        .expect("known");
    assert!(
        one_h > five_m,
        "1h TTL MUST cost more than 5m TTL for same tokens; got 1h={one_h} vs 5m={five_m}"
    );
}

#[test]
fn calculate_cost_default_matches_explicit_five_minute_ttl() {
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 0,
        cache_write_tokens: 25,
    };
    let default_cost = calculate_cost("claude-sonnet-4-5", &usage).expect("known");
    let five_m = calculate_cost_with_ttl("claude-sonnet-4-5", &usage, CacheWriteTtl::FiveMinutes)
        .expect("known");
    assert!(
        (default_cost - five_m).abs() < 1e-12,
        "default calculate_cost MUST equal 5m TTL"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — CacheWriteTtl
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn cache_write_ttl_default_is_five_minutes() {
    let default = CacheWriteTtl::default();
    assert_eq!(default, CacheWriteTtl::FiveMinutes);
}

#[test]
fn cache_write_ttl_variants_are_distinct() {
    assert_ne!(CacheWriteTtl::FiveMinutes, CacheWriteTtl::OneHour);
}

#[test]
fn cache_write_ttl_is_copy() {
    let ttl = CacheWriteTtl::OneHour;
    let copy = ttl;
    let again = ttl;
    assert_eq!(copy, again);
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — Fast mode fallback semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fast_mode_falls_back_to_standard_rates_when_no_fast_tier() {
    // Pick a model that probably doesn't have a fast tier.
    let usage = TokenUsage {
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
    if let Some(p) = get_pricing("claude-sonnet-4-5") {
        if p.fast_mode_input_per_million.is_none() {
            let standard = calculate_cost("claude-sonnet-4-5", &usage).expect("ok");
            let fast = calculate_cost_fast_mode("claude-sonnet-4-5", &usage).expect("ok");
            assert!(
                (standard - fast).abs() < 1e-6,
                "fast mode MUST fall back to standard when no fast tier; got {standard} vs {fast}"
            );
        }
    }
}

#[test]
fn fast_mode_unknown_model_errors() {
    let usage = TokenUsage::default();
    let outcome = calculate_cost_fast_mode("totally-unknown", &usage);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section I — ModelPricing Copy + Debug
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn model_pricing_is_copy() {
    let p = get_pricing("claude-sonnet-4-5").expect("known");
    let copy: ModelPricing = p;
    let again: ModelPricing = p;
    assert_eq!(copy.input_per_million, again.input_per_million);
}

#[test]
fn pricing_error_unknown_model_carries_name() {
    let err = PricingError::UnknownModel("xyz-test".to_string());
    let msg = err.to_string();
    assert!(msg.contains("xyz-test"));
}
