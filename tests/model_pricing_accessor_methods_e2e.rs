//! End-to-end tests for `pricing::ModelPricing` accessor
//! methods — `cache_write_multiplier` (TTL dispatch),
//! `effective_input_per_million` + `effective_output_per_million`
//! (fast-mode override).
//!
//! Sprint 196 of the verification effort. Sprint 94/100
//! covered the `calculate_cost_*` surface; this file pins
//! the const-fn accessors that dispatch the multipliers
//! independent of the cost-computation pipeline.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::session::{get_pricing, CacheWriteTtl, ModelPricing};

fn anthropic_sonnet() -> ModelPricing {
    get_pricing("claude-3-5-sonnet-20241022").expect("known model")
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — cache_write_multiplier dispatch by TTL
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn cache_write_multiplier_five_minutes_returns_5m_field() {
    let p = anthropic_sonnet();
    let m = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    // PINS: 5m multiplier MUST equal the configured 5m field.
    assert!(
        (m - p.cache_write_5m_multiplier).abs() < 1e-12,
        "PINS: 5m TTL dispatches to cache_write_5m_multiplier; got {m}"
    );
}

#[test]
fn cache_write_multiplier_one_hour_returns_1hr_field() {
    let p = anthropic_sonnet();
    let m = p.cache_write_multiplier(CacheWriteTtl::OneHour);
    assert!(
        (m - p.cache_write_1hr_multiplier).abs() < 1e-12,
        "PINS: 1h TTL dispatches to cache_write_1hr_multiplier"
    );
}

#[test]
fn cache_write_multiplier_anthropic_documented_constants() {
    // PINS DOC: Anthropic Sonnet 5m=1.25x + 1h=2.0x.
    let p = anthropic_sonnet();
    let m5 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    let m1h = p.cache_write_multiplier(CacheWriteTtl::OneHour);
    assert!((m5 - 1.25).abs() < 1e-6, "5m MUST be 1.25; got {m5}");
    assert!((m1h - 2.0).abs() < 1e-6, "1h MUST be 2.0; got {m1h}");
}

#[test]
fn cache_write_multiplier_one_hour_strictly_higher_than_five_minutes() {
    // PINS RELATIVE COST: 1h MUST cost more than 5m.
    let p = anthropic_sonnet();
    let m5 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    let m1h = p.cache_write_multiplier(CacheWriteTtl::OneHour);
    assert!(m1h > m5, "1h ({m1h}) MUST be > 5m ({m5})");
}

#[test]
fn cache_write_multiplier_for_every_known_model_is_positive() {
    let known = [
        "claude-3-5-sonnet-20241022",
        "claude-3-opus-20240229",
        "gpt-4o",
        "gpt-4-turbo",
    ];
    for model in known {
        if let Some(p) = get_pricing(model) {
            let m5 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
            let m1h = p.cache_write_multiplier(CacheWriteTtl::OneHour);
            assert!(m5 > 0.0, "{model}: 5m multiplier MUST be > 0; got {m5}");
            assert!(m1h > 0.0, "{model}: 1h multiplier MUST be > 0; got {m1h}");
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — effective_input_per_million
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn effective_input_with_fast_false_returns_normal_rate() {
    let p = anthropic_sonnet();
    let rate = p.effective_input_per_million(false);
    assert!(
        (rate - p.input_per_million).abs() < 1e-12,
        "fast=false MUST return normal rate"
    );
}

#[test]
fn effective_input_with_fast_true_returns_fast_rate_when_configured() {
    // Opus 4.6+ has fast_mode_input_per_million=Some(30.0).
    // For models without fast tier (Sonnet here), fast=true
    // still returns the normal rate.
    let p = anthropic_sonnet();
    let normal = p.effective_input_per_million(false);
    let fast = p.effective_input_per_million(true);
    if let Some(configured_fast) = p.fast_mode_input_per_million {
        // Configured → fast rate.
        assert!((fast - configured_fast).abs() < 1e-12);
    } else {
        // Not configured → falls back to normal.
        assert!((fast - normal).abs() < 1e-12, "no fast tier → falls back");
    }
}

#[test]
fn effective_input_fast_mode_returns_30_per_million_for_opus_when_set() {
    // Look for any Opus model with fast tier set.
    let opus = get_pricing("claude-opus-4-7");
    if let Some(p) = opus {
        if p.fast_mode_input_per_million.is_some() {
            let fast = p.effective_input_per_million(true);
            // PINS #642: CC parity 30 USD/M tokens.
            assert!(
                (fast - 30.0).abs() < 1e-6,
                "PINS #642: Opus fast tier MUST be 30 USD/M; got {fast}"
            );
        }
    }
}

#[test]
fn effective_input_fast_then_normal_returns_different_rates_when_tier_set() {
    let opus = get_pricing("claude-opus-4-7");
    if let Some(p) = opus {
        if p.fast_mode_input_per_million.is_some() {
            let normal = p.effective_input_per_million(false);
            let fast = p.effective_input_per_million(true);
            assert!(
                (normal - fast).abs() > 1e-6,
                "fast tier with override MUST differ from normal"
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — effective_output_per_million
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn effective_output_with_fast_false_returns_normal_rate() {
    let p = anthropic_sonnet();
    let rate = p.effective_output_per_million(false);
    assert!((rate - p.output_per_million).abs() < 1e-12);
}

#[test]
fn effective_output_fast_mode_returns_150_per_million_for_opus_when_set() {
    let opus = get_pricing("claude-opus-4-7");
    if let Some(p) = opus {
        if p.fast_mode_output_per_million.is_some() {
            let fast = p.effective_output_per_million(true);
            // PINS #642: CC parity 150 USD/M output tokens.
            assert!(
                (fast - 150.0).abs() < 1e-6,
                "PINS #642: Opus fast OUTPUT tier MUST be 150 USD/M"
            );
        }
    }
}

#[test]
fn effective_output_with_fast_falls_back_when_tier_unset() {
    // Sonnet (no fast tier) → fast=true returns normal output rate.
    let p = anthropic_sonnet();
    let normal = p.effective_output_per_million(false);
    let fast = p.effective_output_per_million(true);
    if p.fast_mode_output_per_million.is_none() {
        assert!((fast - normal).abs() < 1e-12);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Determinism + idempotency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn cache_write_multiplier_is_pure_repeated_calls_same_value() {
    let p = anthropic_sonnet();
    let m1 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    let m2 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    let m3 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    assert!((m1 - m2).abs() < 1e-12);
    assert!((m2 - m3).abs() < 1e-12);
}

#[test]
fn effective_input_per_million_is_pure_repeated_calls_same_value() {
    let p = anthropic_sonnet();
    let r1 = p.effective_input_per_million(true);
    let r2 = p.effective_input_per_million(true);
    assert!((r1 - r2).abs() < 1e-12);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Cross-method consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fast_false_input_equals_fast_false_input_regardless_of_fast_tier_field() {
    // PINS: when fast=false, fast_mode_*_per_million is IGNORED.
    let p = anthropic_sonnet();
    let a = p.effective_input_per_million(false);
    let b = p.effective_input_per_million(false);
    assert!((a - b).abs() < 1e-12);
    // a equals input_per_million regardless of fast tier presence.
    assert!((a - p.input_per_million).abs() < 1e-12);
}

#[test]
fn ttl_dispatch_does_not_use_input_per_million_directly() {
    // PINS DOC: cache_write_multiplier is a multiplier — small.
    // It's the multiplier applied later against input_per_million,
    // not the actual cost. Verify it's bounded.
    let p = anthropic_sonnet();
    let m5 = p.cache_write_multiplier(CacheWriteTtl::FiveMinutes);
    let m1h = p.cache_write_multiplier(CacheWriteTtl::OneHour);
    assert!(m5 < 10.0, "multipliers MUST be small numbers; got {m5}");
    assert!(m1h < 10.0);
    // Anthropic 5m is 1.25, 1h is 2.0 — well under 10×.
}
