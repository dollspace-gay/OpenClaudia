//! Model pricing and cost calculation.
//!
//! Pricing is looked up by exact model identifier against a static table
//! seeded from a const array.  Substring matching is intentionally avoided:
//! it conflates families (e.g. "claude-3-opus" vs "claude-opus-4"), masks
//! typos, and silently invents prices for models that aren't actually known.
//!
//! Unknown models return [`None`] and emit a `tracing::warn!`; callers are
//! expected to surface "unknown pricing for model X" to the user rather
//! than display `$0.00` as if it were authoritative.

use super::state::TokenUsage;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Pricing data for a model (per million tokens)
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Cost per million input tokens (USD)
    pub input_per_million: f64,
    /// Cost per million output tokens (USD)
    pub output_per_million: f64,
}

impl ModelPricing {
    const fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
        }
    }
}

/// Canonical pricing table — exact model id → rates.
///
/// Entries are listed alphabetically within a provider block to keep diffs
/// readable when rates change.  Add new models as new rows; never collapse
/// distinct model ids into a substring match.
const PRICING_TABLE: &[(&str, ModelPricing)] = &[
    // ----- Anthropic -----
    ("claude-3-opus-20240229", ModelPricing::new(15.0, 75.0)),
    ("claude-3-sonnet-20240229", ModelPricing::new(3.0, 15.0)),
    ("claude-3-haiku-20240307", ModelPricing::new(0.25, 1.25)),
    ("claude-3-5-sonnet-20240620", ModelPricing::new(3.0, 15.0)),
    ("claude-3-5-sonnet-20241022", ModelPricing::new(3.0, 15.0)),
    ("claude-3-5-haiku-20241022", ModelPricing::new(0.80, 4.0)),
    ("claude-3-7-sonnet-20250219", ModelPricing::new(3.0, 15.0)),
    ("claude-opus-4-20250514", ModelPricing::new(15.0, 75.0)),
    ("claude-opus-4-5", ModelPricing::new(15.0, 75.0)),
    ("claude-sonnet-4-20250514", ModelPricing::new(3.0, 15.0)),
    ("claude-sonnet-4-5", ModelPricing::new(3.0, 15.0)),
    ("claude-haiku-4-5", ModelPricing::new(1.0, 5.0)),
    // ----- OpenAI -----
    ("gpt-4", ModelPricing::new(30.0, 60.0)),
    ("gpt-4-turbo", ModelPricing::new(10.0, 30.0)),
    ("gpt-4o", ModelPricing::new(2.5, 10.0)),
    ("gpt-4o-mini", ModelPricing::new(0.15, 0.60)),
    ("gpt-4.1", ModelPricing::new(2.0, 8.0)),
    ("gpt-4.1-mini", ModelPricing::new(0.40, 1.60)),
    ("gpt-4.1-nano", ModelPricing::new(0.10, 0.40)),
    ("gpt-5", ModelPricing::new(2.0, 8.0)),
    ("gpt-5-mini", ModelPricing::new(0.50, 2.0)),
    ("gpt-5-nano", ModelPricing::new(0.10, 0.40)),
    ("gpt-5.2", ModelPricing::new(2.0, 8.0)),
    ("o1", ModelPricing::new(15.0, 60.0)),
    ("o1-mini", ModelPricing::new(3.0, 12.0)),
    ("o3", ModelPricing::new(10.0, 40.0)),
    ("o3-mini", ModelPricing::new(1.10, 4.40)),
    ("o4-mini", ModelPricing::new(1.10, 4.40)),
    // ----- Google Gemini -----
    ("gemini-1.5-pro", ModelPricing::new(1.25, 5.0)),
    ("gemini-1.5-flash", ModelPricing::new(0.075, 0.30)),
    ("gemini-2.0-flash", ModelPricing::new(0.075, 0.30)),
    ("gemini-2.5-pro", ModelPricing::new(1.25, 10.0)),
    ("gemini-2.5-flash", ModelPricing::new(0.075, 0.30)),
    // ----- DeepSeek -----
    ("deepseek-chat", ModelPricing::new(0.27, 1.10)),
    ("deepseek-reasoner", ModelPricing::new(0.55, 2.19)),
    // ----- Qwen -----
    ("qwen-max", ModelPricing::new(0.50, 2.0)),
    ("qwen-plus", ModelPricing::new(0.40, 1.20)),
    ("qwen-turbo", ModelPricing::new(0.30, 0.60)),
    ("qwq-32b-preview", ModelPricing::new(0.50, 2.0)),
];

/// Convenience aliases — short names that resolve to a canonical model id.
///
/// Only entries where there is an unambiguous "current" choice are listed
/// here.  Anything ambiguous (e.g. plain "gpt-4") must remain an exact key
/// in [`PRICING_TABLE`].
const ALIAS_TABLE: &[(&str, &str)] = &[
    ("claude-3-5-sonnet", "claude-3-5-sonnet-20241022"),
    ("claude-3-5-haiku", "claude-3-5-haiku-20241022"),
    ("claude-3-7-sonnet", "claude-3-7-sonnet-20250219"),
    ("claude-opus-4", "claude-opus-4-20250514"),
    ("claude-sonnet-4", "claude-sonnet-4-20250514"),
];

/// Lazily-built lookup index from model id (lowercase) to pricing.
///
/// Aliases are inlined into the same map so callers only pay one lookup.
static PRICING_INDEX: LazyLock<HashMap<&'static str, ModelPricing>> = LazyLock::new(|| {
    let mut map: HashMap<&'static str, ModelPricing> =
        HashMap::with_capacity(PRICING_TABLE.len() + ALIAS_TABLE.len());
    for &(name, pricing) in PRICING_TABLE {
        // A duplicate entry in PRICING_TABLE is a programming error; surface
        // it loudly in debug builds.  In release we keep the first entry.
        debug_assert!(
            !map.contains_key(name),
            "duplicate pricing entry for model {name}"
        );
        map.entry(name).or_insert(pricing);
    }
    for &(alias, canonical) in ALIAS_TABLE {
        let pricing = map
            .get(canonical)
            .copied()
            .unwrap_or_else(|| panic!("alias {alias} points at unknown model {canonical}"));
        debug_assert!(
            !map.contains_key(alias),
            "alias {alias} collides with an existing pricing entry"
        );
        map.entry(alias).or_insert(pricing);
    }
    map
});

/// Look up pricing for a model by exact name.
///
/// Lookup is case-insensitive (the input is normalised to lowercase) but
/// otherwise strict: `"claude-3-haiku-foo"` does **not** match the entry
/// for `"claude-3-haiku-20240307"`.  Unknown models return [`None`] and log
/// a single `tracing::warn!`.
#[must_use]
pub fn get_pricing(model: &str) -> Option<ModelPricing> {
    let key = model.to_lowercase();
    let hit = PRICING_INDEX.get(key.as_str()).copied();
    if hit.is_none() {
        tracing::warn!(model = %model, "unknown pricing for model");
    }
    hit
}

/// Calculate the cost for given token usage and model.
///
/// Token counts are converted to `f64` for cost calculation. For values
/// above 2^52 (~4.5 quadrillion tokens), precision loss may occur, but
/// this is well beyond realistic usage.
#[must_use]
pub fn calculate_cost(model: &str, usage: &TokenUsage) -> Option<f64> {
    let pricing = get_pricing(model)?;
    // Realistic per-request token counts fit in `u32`; the conversion via
    // `f64::from` is then exact, which keeps clippy's pedantic
    // `cast_precision_loss` lint happy without an `#[allow]`.
    let input = f64_from_tokens(usage.input_tokens);
    let output = f64_from_tokens(usage.output_tokens);
    let cache_read = f64_from_tokens(usage.cache_read_tokens);
    let cache_write = f64_from_tokens(usage.cache_write_tokens);

    let input_cost = input * pricing.input_per_million / 1_000_000.0;
    let output_cost = output * pricing.output_per_million / 1_000_000.0;
    // Cache reads are typically 90% cheaper; cache writes ~25% more than input.
    let cache_read_cost = cache_read * pricing.input_per_million * 0.1 / 1_000_000.0;
    let cache_write_cost = cache_write * pricing.input_per_million * 1.25 / 1_000_000.0;
    Some(input_cost + output_cost + cache_read_cost + cache_write_cost)
}

/// Lossless `u64 -> f64` conversion for token counts.
///
/// `f64` exactly represents every integer up to `2^53`.  Realistic token
/// counts per request are well under `u32::MAX` (~4.3 billion); for the
/// pathological case of a `u64` larger than that we saturate to
/// `u32::MAX` so the conversion via [`f64::from`] is exact and clippy's
/// pedantic `cast_precision_loss` lint does not fire.  Saturation here is
/// strictly preferable to silent precision loss: it produces an obviously
/// wrong (very large) cost number rather than a subtly wrong one.
fn f64_from_tokens(n: u64) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_pricing_known_models() {
        assert!(get_pricing("claude-3-opus-20240229").is_some());
        assert!(get_pricing("claude-3-sonnet-20240229").is_some());
        assert!(get_pricing("claude-3-haiku-20240307").is_some());
        assert!(get_pricing("gpt-4o").is_some());
        assert!(get_pricing("gpt-4o-mini").is_some());
        assert!(get_pricing("gemini-2.0-flash").is_some());
        assert!(get_pricing("deepseek-chat").is_some());

        // Unknown model returns None
        assert!(get_pricing("totally-unknown-model").is_none());
    }

    #[test]
    fn test_calculate_cost() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let cost = calculate_cost("claude-3-haiku-20240307", &usage);
        assert!(cost.is_some());
        let c = cost.unwrap();
        // haiku: $0.25/M input + $1.25/M output * 0.1M = $0.25 + $0.125 = $0.375
        assert!(c > 0.3 && c < 0.5, "Expected ~$0.375, got {c}");
    }

    // -----------------------------------------------------------------------
    // #495 — exact-match lookup tests
    // -----------------------------------------------------------------------

    /// #495 / KISS: exact model id returns the rates declared in the table.
    #[test]
    fn exact_match_returns_declared_rates() {
        let p = get_pricing("claude-3-haiku-20240307").expect("haiku must be known");
        assert!((p.input_per_million - 0.25).abs() < f64::EPSILON);
        assert!((p.output_per_million - 1.25).abs() < f64::EPSILON);

        let p = get_pricing("gpt-4o").expect("gpt-4o must be known");
        assert!((p.input_per_million - 2.5).abs() < f64::EPSILON);
        assert!((p.output_per_million - 10.0).abs() < f64::EPSILON);

        let p = get_pricing("claude-opus-4-20250514").expect("opus-4 must be known");
        assert!((p.input_per_million - 15.0).abs() < f64::EPSILON);
        assert!((p.output_per_million - 75.0).abs() < f64::EPSILON);
    }

    /// #495: unknown model id returns None — never a silent $0.00.
    #[test]
    fn unknown_model_returns_none() {
        assert!(get_pricing("totally-made-up-model-xyz").is_none());
        assert!(get_pricing("").is_none());
    }

    /// #495 anti-regression: a string that *contains* a known model id
    /// must NOT match it.  This is the precise bug the issue calls out —
    /// the old `m.contains("haiku")` cascade would return Haiku pricing
    /// for any string with "haiku" in it.
    #[test]
    fn substring_does_not_match() {
        // Trailing garbage on a real model id.
        assert!(
            get_pricing("claude-3-haiku-foo").is_none(),
            "substring containment must not satisfy lookup"
        );
        // The bare family name "haiku" used to win against every Haiku.
        assert!(get_pricing("haiku").is_none());
        // Leading garbage in front of a known id.
        assert!(get_pricing("not-really-gpt-4o").is_none());
        // A foreign model that happens to share a keyword with the old
        // cascade.  Under the old code, "opus-pricing-test" would return
        // Opus rates; here it must return None.
        assert!(get_pricing("opus-pricing-test").is_none());
    }

    /// #495: alias mapping — "claude-3-5-sonnet" resolves to the dated id.
    #[test]
    fn alias_maps_to_canonical_rates() {
        let aliased = get_pricing("claude-3-5-sonnet").expect("alias must resolve");
        let canonical = get_pricing("claude-3-5-sonnet-20241022").expect("canonical must resolve");
        assert!((aliased.input_per_million - canonical.input_per_million).abs() < f64::EPSILON);
        assert!((aliased.output_per_million - canonical.output_per_million).abs() < f64::EPSILON);

        // And a second alias to prove the mechanism isn't a one-off.
        let opus_alias = get_pricing("claude-opus-4").expect("opus-4 alias must resolve");
        let opus_canon =
            get_pricing("claude-opus-4-20250514").expect("opus-4 canonical must resolve");
        assert!((opus_alias.input_per_million - opus_canon.input_per_million).abs() < f64::EPSILON);
    }

    /// #495: lookup is case-insensitive on the input, but still exact-match.
    #[test]
    fn lookup_is_case_insensitive() {
        assert!(get_pricing("GPT-4o").is_some());
        assert!(get_pricing("Claude-3-Haiku-20240307").is_some());
        // Case folding does not relax the exact-match rule.
        assert!(get_pricing("GPT-4O-FOO").is_none());
    }

    // -----------------------------------------------------------------------
    // B5 — calculate_cost: cache-read and cache-write tokens (spec §B5)
    // Pins OC's CURRENT fixed-ratio behavior without asserting CC is wrong.
    // Divergences vs CC are noted inline as gap markers.
    // -----------------------------------------------------------------------

    /// B5: cache-read tokens apply the 0.1× fixed ratio on OC.
    /// CC uses per-model `promptCacheReadTokens` from `MODEL_COSTS`; OC uses
    /// `input_per_million × 0.1`.  This test pins OC's ratio.
    #[test]
    fn b5_cache_read_tokens_apply_point_one_ratio() {
        // 1 million cache-read tokens at Sonnet pricing ($3.00/M input).
        // OC: cache_read_cost = 3.00 × 0.1 = $0.30
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0,
        };
        let cost = calculate_cost("claude-sonnet-4-5", &usage).unwrap();
        let expected = 3.0 * 0.1; // $0.30
        assert!(
            (cost - expected).abs() < 1e-9,
            "cache-read ratio must be 0.1× input price; got {cost}, expected {expected}"
        );
    }

    /// B5: cache-write tokens apply the 1.25× fixed ratio on OC.
    /// CC uses per-model `promptCacheWriteTokens`; OC uses
    /// `input_per_million × 1.25`.  This test pins OC's ratio.
    #[test]
    fn b5_cache_write_tokens_apply_one_point_two_five_ratio() {
        // 1 million cache-write tokens at Sonnet pricing ($3.00/M input).
        // OC: cache_write_cost = 3.00 × 1.25 = $3.75
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 1_000_000,
        };
        let cost = calculate_cost("claude-sonnet-4-5", &usage).unwrap();
        let expected = 3.0 * 1.25; // $3.75
        assert!(
            (cost - expected).abs() < 1e-9,
            "cache-write ratio must be 1.25× input price; got {cost}, expected {expected}"
        );
    }

    /// B5: combined input + output + cache-read + cache-write — four terms sum
    /// correctly under OC's formula.
    #[test]
    fn b5_all_four_token_buckets_sum_correctly() {
        // Use Haiku pricing: $0.25/M input, $1.25/M output.
        // cache_read  = $0.25 × 0.1  = $0.025/M
        // cache_write = $0.25 × 1.25 = $0.3125/M
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
        };
        let cost = calculate_cost("claude-3-haiku-20240307", &usage).unwrap();
        let expected = 0.25f64.mul_add(1.25, 0.25f64.mul_add(0.1, 0.25 + 1.25));
        assert!(
            (cost - expected).abs() < 1e-9,
            "four-bucket sum wrong; got {cost}, expected {expected}"
        );
    }

    /// B5 divergence pin: unknown model returns None in OC.
    /// CC returns a default cost instead of None — this pins OC's behavior.
    #[test]
    fn b5_unknown_model_returns_none() {
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        // Divergence vs CC: CC falls back to default model cost; OC returns None.
        let cost = calculate_cost("completely-unknown-model-xyz", &usage);
        assert!(
            cost.is_none(),
            "OC returns None for unknown model (CC gap: CC returns default cost)"
        );
    }

    /// B5: zero-token usage returns Some(0.0), not None.
    #[test]
    fn b5_zero_tokens_returns_zero_cost() {
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let cost = calculate_cost("claude-3-haiku-20240307", &usage).unwrap();
        assert!(cost.abs() < f64::EPSILON);
    }
}
