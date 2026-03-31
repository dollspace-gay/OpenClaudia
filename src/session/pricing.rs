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
pub fn get_pricing(model: &str) -> Option<ModelPricing> {
    let m = model.to_lowercase();
    match () {
        _ if m.contains("opus") => Some(ModelPricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
        }),
        _ if m.contains("sonnet") && m.contains("3.5") => Some(ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        }),
        _ if m.contains("sonnet") => Some(ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        }),
        _ if m.contains("haiku") => Some(ModelPricing {
            input_per_million: 0.25,
            output_per_million: 1.25,
        }),
        _ if m.contains("gpt-5.2") => Some(ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        }),
        _ if m.contains("gpt-5") && m.contains("mini") => Some(ModelPricing {
            input_per_million: 0.50,
            output_per_million: 2.0,
        }),
        _ if m.contains("gpt-5") && m.contains("nano") => Some(ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        }),
        _ if m.contains("gpt-5") => Some(ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        }),
        _ if m.contains("gpt-4.1") && m.contains("nano") => Some(ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.40,
        }),
        _ if m.contains("gpt-4.1") && m.contains("mini") => Some(ModelPricing {
            input_per_million: 0.40,
            output_per_million: 1.60,
        }),
        _ if m.contains("gpt-4.1") => Some(ModelPricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
        }),
        _ if m.contains("gpt-4o-mini") => Some(ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
        }),
        _ if m.contains("gpt-4o") => Some(ModelPricing {
            input_per_million: 2.5,
            output_per_million: 10.0,
        }),
        _ if m.contains("gpt-4") => Some(ModelPricing {
            input_per_million: 30.0,
            output_per_million: 60.0,
        }),
        _ if m.contains("o3") || m.contains("o4") => Some(ModelPricing {
            input_per_million: 10.0,
            output_per_million: 40.0,
        }),
        _ if m.contains("o1") => Some(ModelPricing {
            input_per_million: 15.0,
            output_per_million: 60.0,
        }),
        _ if m.contains("gemini-2") && m.contains("flash") => Some(ModelPricing {
            input_per_million: 0.075,
            output_per_million: 0.30,
        }),
        _ if m.contains("gemini-2") => Some(ModelPricing {
            input_per_million: 1.25,
            output_per_million: 10.0,
        }),
        _ if m.contains("gemini") => Some(ModelPricing {
            input_per_million: 1.25,
            output_per_million: 5.0,
        }),
        _ if m.contains("deepseek") => Some(ModelPricing {
            input_per_million: 0.27,
            output_per_million: 1.10,
        }),
        _ if m.contains("qwen") => Some(ModelPricing {
            input_per_million: 0.50,
            output_per_million: 2.0,
        }),
        _ => None,
    }
}

/// Calculate the cost for given token usage and model
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
        assert!(c > 0.3 && c < 0.5, "Expected ~$0.375, got {}", c);
    }
}
