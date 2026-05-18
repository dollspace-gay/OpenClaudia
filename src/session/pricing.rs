//! Model pricing and cost calculation.

use super::state::TokenUsage;

/// Pricing data for a model (per million tokens)
#[derive(Debug, Clone)]
pub struct ModelPricing {
    /// Cost per million input tokens (USD)
    pub input_per_million: f64,
    /// Cost per million output tokens (USD)
    pub output_per_million: f64,
}

/// Look up pricing for a model by name.
///
/// Returns hardcoded pricing for common models. Pricing is approximate
/// and may not reflect current rates or promotional pricing.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn get_pricing(model: &str) -> Option<ModelPricing> {
    let m = model.to_lowercase();
    if m.contains("opus") {
        Some(ModelPricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
        })
    } else if m.contains("sonnet") {
        // Both Sonnet 3.5 and later Sonnet models share the same pricing
        Some(ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        })
    } else if m.contains("haiku") {
        Some(ModelPricing {
            input_per_million: 0.25,
            output_per_million: 1.25,
        })
    } else if m.contains("gpt-5.2") {
        Some(ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        })
    } else if m.contains("gpt-5") && m.contains("mini") {
        Some(ModelPricing {
            input_per_million: 0.50,
            output_per_million: 2.0,
        })
    } else if m.contains("gpt-5") && m.contains("nano") {
        Some(ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        })
    } else if m.contains("gpt-5") {
        Some(ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        })
    } else if m.contains("gpt-4.1") && m.contains("nano") {
        Some(ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        })
    } else if m.contains("gpt-4.1") && m.contains("mini") {
        Some(ModelPricing {
            input_per_million: 0.40,
            output_per_million: 1.60,
        })
    } else if m.contains("gpt-4.1") {
        Some(ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        })
    } else if m.contains("gpt-4o-mini") {
        Some(ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        })
    } else if m.contains("gpt-4o") {
        Some(ModelPricing {
            input_per_million: 2.5,
            output_per_million: 10.0,
        })
    } else if m.contains("gpt-4-turbo") {
        Some(ModelPricing {
            input_per_million: 10.0,
            output_per_million: 30.0,
        })
    } else if m.contains("gpt-4") {
        Some(ModelPricing {
            input_per_million: 30.0,
            output_per_million: 60.0,
        })
    } else if m.contains("o3") || m.contains("o4") {
        Some(ModelPricing {
            input_per_million: 10.0,
            output_per_million: 40.0,
        })
    } else if m.contains("o1") {
        Some(ModelPricing {
            input_per_million: 15.0,
            output_per_million: 60.0,
        })
    } else if m.contains("gemini-2") && m.contains("flash") {
        Some(ModelPricing {
            input_per_million: 0.075,
            output_per_million: 0.30,
        })
    } else if m.contains("gemini-2") {
        Some(ModelPricing {
            input_per_million: 1.25,
            output_per_million: 10.0,
        })
    } else if m.contains("gemini") {
        Some(ModelPricing {
            input_per_million: 1.25,
            output_per_million: 5.0,
        })
    } else if m.contains("deepseek") {
        Some(ModelPricing {
            input_per_million: 0.27,
            output_per_million: 1.10,
        })
    } else if m.contains("qwen") {
        Some(ModelPricing {
            input_per_million: 0.50,
            output_per_million: 2.0,
        })
    } else {
        None
    }
}

/// Calculate the cost for given token usage and model.
///
/// Token counts are converted to `f64` for cost calculation. For values
/// above 2^52 (~4.5 quadrillion tokens), precision loss may occur, but
/// this is well beyond realistic usage.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn calculate_cost(model: &str, usage: &TokenUsage) -> Option<f64> {
    let pricing = get_pricing(model)?;
    let input_cost = usage.input_tokens as f64 * pricing.input_per_million / 1_000_000.0;
    let output_cost = usage.output_tokens as f64 * pricing.output_per_million / 1_000_000.0;
    // Cache reads are typically 90% cheaper; cache writes same as input
    let cache_read_cost =
        usage.cache_read_tokens as f64 * pricing.input_per_million * 0.1 / 1_000_000.0;
    let cache_write_cost =
        usage.cache_write_tokens as f64 * pricing.input_per_million * 1.25 / 1_000_000.0;
    Some(input_cost + output_cost + cache_read_cost + cache_write_cost)
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
        assert_eq!(cost, 0.0);
    }
}
