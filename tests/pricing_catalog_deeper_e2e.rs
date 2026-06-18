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
    FAST_MODE_INPUT_PER_MILLION, FAST_MODE_OUTPUT_PER_MILLION,
    OPUS_4_8_FAST_MODE_INPUT_PER_MILLION, OPUS_4_8_FAST_MODE_OUTPUT_PER_MILLION,
    WEB_SEARCH_REQUEST_USD,
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

#[test]
fn opus_4_8_fast_mode_input_per_million_is_10_dollars() {
    assert_eq!(OPUS_4_8_FAST_MODE_INPUT_PER_MILLION, 10.0);
}

#[test]
fn opus_4_8_fast_mode_output_per_million_is_50_dollars() {
    assert_eq!(OPUS_4_8_FAST_MODE_OUTPUT_PER_MILLION, 50.0);
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
fn get_pricing_for_current_anthropic_models_returns_documented_rates() {
    let fable = get_pricing("claude-fable-5").expect("fable");
    assert_eq!(fable.input_per_million, 10.0);
    assert_eq!(fable.output_per_million, 50.0);

    let opus48 = get_pricing("claude-opus-4-8").expect("opus 4.8");
    assert_eq!(opus48.input_per_million, 5.0);
    assert_eq!(opus48.output_per_million, 25.0);
    assert_eq!(
        opus48.fast_mode_input_per_million,
        Some(OPUS_4_8_FAST_MODE_INPUT_PER_MILLION)
    );
    assert_eq!(
        opus48.fast_mode_output_per_million,
        Some(OPUS_4_8_FAST_MODE_OUTPUT_PER_MILLION)
    );

    let opus47 = get_pricing("claude-opus-4-7").expect("opus 4.7");
    assert_eq!(opus47.input_per_million, 5.0);
    assert_eq!(opus47.output_per_million, 25.0);
    assert_eq!(
        opus47.fast_mode_input_per_million,
        Some(FAST_MODE_INPUT_PER_MILLION)
    );
}

#[test]
fn get_pricing_for_current_openai_gpt5_models_returns_documented_rates() {
    let gpt55 = get_pricing("gpt-5.5").expect("gpt-5.5");
    assert_eq!(gpt55.input_per_million, 5.0);
    assert_eq!(gpt55.output_per_million, 30.0);

    let gpt55_pro = get_pricing("gpt-5.5-pro").expect("gpt-5.5-pro");
    assert_eq!(gpt55_pro.input_per_million, 30.0);
    assert_eq!(gpt55_pro.output_per_million, 180.0);

    let gpt54_mini = get_pricing("gpt-5.4-mini").expect("gpt-5.4-mini");
    assert_eq!(gpt54_mini.input_per_million, 0.75);
    assert_eq!(gpt54_mini.output_per_million, 4.5);

    let gpt52_pro = get_pricing("gpt-5.2-pro").expect("gpt-5.2-pro");
    assert_eq!(gpt52_pro.input_per_million, 21.0);
    assert_eq!(gpt52_pro.output_per_million, 168.0);

    let gpt53_chat = get_pricing("gpt-5.3-chat-latest").expect("gpt-5.3-chat-latest");
    assert_eq!(gpt53_chat.input_per_million, 1.75);
    assert_eq!(gpt53_chat.output_per_million, 14.0);

    let chat_latest = get_pricing("chat-latest").expect("chat-latest");
    assert_eq!(chat_latest.input_per_million, 5.0);
    assert_eq!(chat_latest.output_per_million, 30.0);
}

#[test]
fn get_pricing_for_current_openai_compatibility_models_returns_documented_rates() {
    let codex_mini = get_pricing("codex-mini-latest").expect("codex-mini-latest");
    assert_eq!(codex_mini.input_per_million, 1.5);
    assert_eq!(codex_mini.output_per_million, 6.0);

    let gpt45 = get_pricing("gpt-4.5-preview").expect("gpt-4.5-preview");
    assert_eq!(gpt45.input_per_million, 75.0);
    assert_eq!(gpt45.output_per_million, 150.0);

    let gpt35 = get_pricing("gpt-3.5-turbo").expect("gpt-3.5-turbo");
    assert_eq!(gpt35.input_per_million, 0.5);
    assert_eq!(gpt35.output_per_million, 1.5);
}

#[test]
fn get_pricing_for_current_openai_o_series_returns_documented_rates() {
    let o3_pro = get_pricing("o3-pro").expect("o3-pro");
    assert_eq!(o3_pro.input_per_million, 20.0);
    assert_eq!(o3_pro.output_per_million, 80.0);

    let o3 = get_pricing("o3").expect("o3");
    assert_eq!(o3.input_per_million, 2.0);
    assert_eq!(o3.output_per_million, 8.0);

    let o1_pro = get_pricing("o1-pro").expect("o1-pro");
    assert_eq!(o1_pro.input_per_million, 150.0);
    assert_eq!(o1_pro.output_per_million, 600.0);

    let o1_mini = get_pricing("o1-mini").expect("o1-mini");
    assert_eq!(o1_mini.input_per_million, 1.10);
    assert_eq!(o1_mini.output_per_million, 4.40);
}

#[test]
fn get_pricing_for_current_deepseek_v4_models_returns_documented_rates() {
    let flash = get_pricing("deepseek-v4-flash").expect("deepseek-v4-flash");
    assert_eq!(flash.input_per_million, 0.14);
    assert_eq!(flash.output_per_million, 0.28);
    assert!((flash.cache_read_multiplier - 0.02).abs() < 1e-12);
    assert_eq!(
        flash.cache_write_multiplier(CacheWriteTtl::FiveMinutes),
        1.0
    );
    assert_eq!(flash.cache_write_multiplier(CacheWriteTtl::OneHour), 1.0);

    let pro = get_pricing("deepseek-v4-pro").expect("deepseek-v4-pro");
    assert_eq!(pro.input_per_million, 0.435);
    assert_eq!(pro.output_per_million, 0.87);
    assert!((pro.cache_read_multiplier - (1.0 / 120.0)).abs() < 1e-12);
    assert_eq!(pro.cache_write_multiplier(CacheWriteTtl::FiveMinutes), 1.0);
    assert_eq!(pro.cache_write_multiplier(CacheWriteTtl::OneHour), 1.0);

    for alias in ["deepseek-chat", "deepseek-reasoner"] {
        let pricing = get_pricing(alias).expect(alias);
        assert_eq!(pricing.input_per_million, flash.input_per_million);
        assert_eq!(pricing.output_per_million, flash.output_per_million);
        assert_eq!(pricing.cache_read_multiplier, flash.cache_read_multiplier);
        assert_eq!(
            pricing.cache_write_multiplier(CacheWriteTtl::FiveMinutes),
            flash.cache_write_multiplier(CacheWriteTtl::FiveMinutes)
        );
    }
}

#[test]
fn get_pricing_for_current_qwen_models_returns_documented_base_rates() {
    let max = get_pricing("qwen3.7-max-2026-05-17").expect("qwen3.7-max");
    assert_eq!(max.input_per_million, 2.50);
    assert_eq!(max.output_per_million, 7.50);

    let plus = get_pricing("qwen3.7-plus-2026-05-26").expect("qwen3.7-plus");
    assert_eq!(plus.input_per_million, 0.40);
    assert_eq!(plus.output_per_million, 1.60);

    let legacy_plus = get_pricing("qwen-plus-2025-12-01").expect("qwen-plus");
    assert_eq!(legacy_plus.input_per_million, 0.40);
    assert_eq!(legacy_plus.output_per_million, 1.20);

    let coder_flash = get_pricing("qwen3-coder-flash").expect("qwen3-coder-flash");
    assert_eq!(coder_flash.input_per_million, 0.30);
    assert_eq!(coder_flash.output_per_million, 1.50);
}

#[test]
fn get_pricing_for_current_kimi_models_returns_documented_rates() {
    let k27 = get_pricing("kimi-k2.7-code").expect("kimi-k2.7-code");
    assert_eq!(k27.input_per_million, 0.95);
    assert_eq!(k27.output_per_million, 4.0);
    assert!((k27.cache_read_multiplier - 0.20).abs() < 1e-12);

    let k27_fast = get_pricing("kimi-k2.7-code-highspeed").expect("kimi-k2.7-code-highspeed");
    assert_eq!(k27_fast.input_per_million, 1.90);
    assert_eq!(k27_fast.output_per_million, 8.0);
    assert!((k27_fast.cache_read_multiplier - 0.20).abs() < 1e-12);

    let k26 = get_pricing("kimi-k2.6").expect("kimi-k2.6");
    assert_eq!(k26.input_per_million, 0.95);
    assert_eq!(k26.output_per_million, 4.0);
    assert!((k26.cache_read_multiplier - (0.16 / 0.95)).abs() < 1e-12);

    let k25 = get_pricing("kimi-k2.5").expect("kimi-k2.5");
    assert_eq!(k25.input_per_million, 0.60);
    assert_eq!(k25.output_per_million, 3.0);
    assert!((k25.cache_read_multiplier - (1.0 / 6.0)).abs() < 1e-12);
}

#[test]
fn get_pricing_for_moonshot_v1_models_returns_documented_rates() {
    let v128 = get_pricing("moonshot-v1-128k-vision-preview").expect("moonshot-v1-128k");
    assert_eq!(v128.input_per_million, 2.0);
    assert_eq!(v128.output_per_million, 5.0);

    let v32 = get_pricing("moonshot-v1-32k").expect("moonshot-v1-32k");
    assert_eq!(v32.input_per_million, 1.0);
    assert_eq!(v32.output_per_million, 3.0);

    let v8 = get_pricing("moonshot-v1-8k").expect("moonshot-v1-8k");
    assert_eq!(v8.input_per_million, 0.20);
    assert_eq!(v8.output_per_million, 2.0);
}

#[test]
fn get_pricing_for_current_minimax_models_returns_documented_rates() {
    let m3 = get_pricing("MiniMax-M3").expect("MiniMax-M3");
    assert_eq!(m3.input_per_million, 0.30);
    assert_eq!(m3.output_per_million, 1.20);
    assert!((m3.cache_read_multiplier - 0.20).abs() < 1e-12);

    let m27 = get_pricing("MiniMax-M2.7").expect("MiniMax-M2.7");
    assert_eq!(m27.input_per_million, 0.30);
    assert_eq!(m27.output_per_million, 1.20);
    assert!((m27.cache_read_multiplier - 0.20).abs() < 1e-12);
    assert_eq!(m27.cache_write_multiplier(CacheWriteTtl::FiveMinutes), 1.25);

    let m27_fast = get_pricing("MiniMax-M2.7-highspeed").expect("MiniMax-M2.7-highspeed");
    assert_eq!(m27_fast.input_per_million, 0.60);
    assert_eq!(m27_fast.output_per_million, 2.40);
    assert!((m27_fast.cache_read_multiplier - 0.10).abs() < 1e-12);
    assert_eq!(
        m27_fast.cache_write_multiplier(CacheWriteTtl::FiveMinutes),
        0.625
    );

    let m25 = get_pricing("MiniMax-M2.5").expect("MiniMax-M2.5");
    assert_eq!(m25.input_per_million, 0.30);
    assert_eq!(m25.output_per_million, 1.20);
    assert!((m25.cache_read_multiplier - 0.10).abs() < 1e-12);
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
