//! Model pricing and cost calculation.
//!
//! Pricing is looked up by **ordered prefix match** against
//! [`PRICING_TABLE`].  The table is walked top-to-bottom and the first
//! entry whose `prefix` matches the (lower-cased) model id wins.  Order is
//! therefore **load-bearing**: more specific prefixes must precede the
//! shorter prefixes that would otherwise shadow them (e.g. `"gpt-5.2"`
//! must come before `"gpt-5"`, and `"gemini-2.0-flash"` before
//! `"gemini-2"`).  This ordering invariant is enforced at test time by
//! [`tests::ordering_no_entry_is_prefix_of_earlier`].
//!
//! Unknown models return [`PricingError::UnknownModel`] and emit a
//! `tracing::warn!`; callers must handle the error explicitly rather than
//! silently displaying `$0.00`.
//!
//! ## Cache-write TTL multipliers (Anthropic)
//!
//! Anthropic prompt-cache writes are billed at a multiple of the input
//! rate depending on TTL: 1.25× for the default 5-minute ephemeral cache
//! and 2.0× for the 1-hour cache.  The two multipliers are stored
//! separately on [`ModelPricing`] (`cache_write_5m_multiplier` and
//! `cache_write_1hr_multiplier`) and selected by [`CacheWriteTtl`] passed
//! to [`calculate_cost_with_ttl`].  The shorter [`calculate_cost`] entry
//! point defaults to 5 m, matching the Anthropic API default when
//! `cache_control.ttl` is omitted.

use super::state::TokenUsage;
use thiserror::Error;

/// Errors returned by [`calculate_cost`] / [`calculate_cost_with_ttl`].
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum PricingError {
    /// The model identifier did not match any prefix in [`PRICING_TABLE`].
    ///
    /// The wrapped string is the original model id (case preserved) so
    /// the caller can log or surface it verbatim to the user.
    #[error("no pricing entry matches model `{0}`")]
    UnknownModel(String),
}

/// TTL bucket for Anthropic prompt-cache write pricing.
///
/// Maps directly onto the `ttl` field of `cache_control` in the Messages
/// API: an absent / `"5m"` value selects [`CacheWriteTtl::FiveMinutes`]
/// and a `"1h"` value selects [`CacheWriteTtl::OneHour`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheWriteTtl {
    /// Default ephemeral cache — billed at the 5 m multiplier (1.25× input
    /// for Anthropic).
    #[default]
    FiveMinutes,
    /// Long-lived cache — billed at the 1 h multiplier (2.0× input for
    /// Anthropic).
    OneHour,
}

/// Pricing data for a model (per million tokens).
///
/// All multipliers are applied against [`Self::input_per_million`]; see
/// module docs for the cache-write TTL split.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Cost per million input tokens (USD).
    pub input_per_million: f64,
    /// Cost per million output tokens (USD).
    pub output_per_million: f64,
    /// Multiplier applied to [`Self::input_per_million`] for prompt-cache
    /// reads.  Industry-standard 0.1× for Anthropic; the same ratio is
    /// re-used as a conservative default for providers without explicit
    /// cache-read pricing.
    pub cache_read_multiplier: f64,
    /// Multiplier applied to [`Self::input_per_million`] for prompt-cache
    /// writes with the default 5 m ephemeral TTL.  1.25× for Anthropic.
    pub cache_write_5m_multiplier: f64,
    /// Multiplier applied to [`Self::input_per_million`] for prompt-cache
    /// writes with the 1 h TTL.  2.0× for Anthropic.  Providers that
    /// don't expose a 1 h tier mirror the 5 m multiplier here so the
    /// selection logic stays uniform.
    pub cache_write_1hr_multiplier: f64,
}

impl ModelPricing {
    /// Anthropic-style pricing: 0.1× cache-read, 1.25× cache-write-5m, 2.0× cache-write-1h.
    const fn anthropic(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
            cache_read_multiplier: 0.1,
            cache_write_5m_multiplier: 1.25,
            cache_write_1hr_multiplier: 2.0,
        }
    }

    /// Non-Anthropic provider pricing: keep the 0.1× / 1.25× ratios that
    /// the previous fixed-ratio code applied uniformly across providers,
    /// and mirror the 5 m multiplier into the 1 h slot since these
    /// providers do not currently differentiate.  This preserves the
    /// pre-refactor cost numbers for `OpenAI` / `Google` / `DeepSeek` / `Qwen`.
    const fn other(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
            cache_read_multiplier: 0.1,
            cache_write_5m_multiplier: 1.25,
            cache_write_1hr_multiplier: 1.25,
        }
    }

    /// Select the cache-write multiplier for the requested TTL.
    #[must_use]
    pub const fn cache_write_multiplier(&self, ttl: CacheWriteTtl) -> f64 {
        match ttl {
            CacheWriteTtl::FiveMinutes => self.cache_write_5m_multiplier,
            CacheWriteTtl::OneHour => self.cache_write_1hr_multiplier,
        }
    }
}

/// Canonical pricing table — **ordered** prefix → rates.
///
/// Lookup is `model_lc.starts_with(prefix)` and the **first** matching
/// row wins, so longer / more-specific prefixes MUST appear before any
/// shorter prefix they would otherwise be shadowed by.  Compare:
///
/// * `"gpt-5.2"` precedes `"gpt-5"` — without this order `"gpt-5.2"`
///   would silently resolve as `"gpt-5"`.
/// * `"gemini-2.0-flash"` precedes `"gemini-2"` for the same reason.
/// * `"o1-mini"` precedes `"o1"`; `"o3-mini"` precedes `"o3"`.
///
/// The [`tests::ordering_no_entry_is_prefix_of_earlier`] test enforces
/// this invariant statically against the table; CI will fail if a new
/// row is inserted in the wrong slot.
pub static PRICING_TABLE: &[(&str, ModelPricing)] = &[
    // ---------------------------------------------------------------------
    // Anthropic — claude family
    //
    // `claude-opus-4-` covers both the dated 2025-05-14 release and the
    // forward-rolled `claude-opus-4-5` / `-6` / `-7` aliases that map to
    // the same Opus rate sheet; the dash anchors the prefix so it cannot
    // accidentally match a future `claude-opus-40-…`.
    // ---------------------------------------------------------------------
    ("claude-3-5-haiku", ModelPricing::anthropic(0.80, 4.0)),
    ("claude-3-5-sonnet", ModelPricing::anthropic(3.0, 15.0)),
    ("claude-3-7-sonnet", ModelPricing::anthropic(3.0, 15.0)),
    ("claude-3-haiku", ModelPricing::anthropic(0.25, 1.25)),
    ("claude-3-opus", ModelPricing::anthropic(15.0, 75.0)),
    ("claude-3-sonnet", ModelPricing::anthropic(3.0, 15.0)),
    ("claude-haiku-4", ModelPricing::anthropic(1.0, 5.0)),
    ("claude-opus-4-", ModelPricing::anthropic(15.0, 75.0)),
    ("claude-opus-4", ModelPricing::anthropic(15.0, 75.0)),
    ("claude-sonnet-4-", ModelPricing::anthropic(3.0, 15.0)),
    ("claude-sonnet-4", ModelPricing::anthropic(3.0, 15.0)),
    // `claude-code-20250219` is the OAuth-only "Claude Code" alias that
    // is billed at Sonnet 4 rates per Anthropic billing docs.
    ("claude-code-", ModelPricing::anthropic(3.0, 15.0)),
    // ---------------------------------------------------------------------
    // OpenAI
    //
    // Note the `gpt-5.2` → `gpt-5` and `gpt-4.1-nano` → `gpt-4.1-mini` →
    // `gpt-4.1` ordering: each shorter prefix must follow its longer
    // siblings or the longer ones become unreachable.
    // ---------------------------------------------------------------------
    ("gpt-4o-mini", ModelPricing::other(0.15, 0.60)),
    ("gpt-4o", ModelPricing::other(2.5, 10.0)),
    ("gpt-4.1-nano", ModelPricing::other(0.10, 0.40)),
    ("gpt-4.1-mini", ModelPricing::other(0.40, 1.60)),
    ("gpt-4.1", ModelPricing::other(2.0, 8.0)),
    ("gpt-4-turbo", ModelPricing::other(10.0, 30.0)),
    ("gpt-4", ModelPricing::other(30.0, 60.0)),
    ("gpt-5.2", ModelPricing::other(2.0, 8.0)),
    ("gpt-5-nano", ModelPricing::other(0.10, 0.40)),
    ("gpt-5-mini", ModelPricing::other(0.50, 2.0)),
    ("gpt-5", ModelPricing::other(2.0, 8.0)),
    ("o1-mini", ModelPricing::other(3.0, 12.0)),
    ("o1-preview", ModelPricing::other(15.0, 60.0)),
    ("o1", ModelPricing::other(15.0, 60.0)),
    ("o3-mini", ModelPricing::other(1.10, 4.40)),
    ("o3", ModelPricing::other(10.0, 40.0)),
    ("o4-mini", ModelPricing::other(1.10, 4.40)),
    ("o4-pro", ModelPricing::other(10.0, 40.0)),
    ("o4", ModelPricing::other(10.0, 40.0)),
    // ---------------------------------------------------------------------
    // Google Gemini
    //
    // `gemini-2.5-flash`/`-pro` and `gemini-2.0-flash` must precede a
    // bare `gemini-2` to avoid the shadowing the original cascade
    // suffered from; this is the second canonical ordering case called
    // out in the issue.
    // ---------------------------------------------------------------------
    ("gemini-1.5-flash", ModelPricing::other(0.075, 0.30)),
    ("gemini-1.5-pro", ModelPricing::other(1.25, 5.0)),
    ("gemini-2.0-flash", ModelPricing::other(0.075, 0.30)),
    ("gemini-2.5-flash", ModelPricing::other(0.075, 0.30)),
    ("gemini-2.5-pro", ModelPricing::other(1.25, 10.0)),
    // ---------------------------------------------------------------------
    // DeepSeek
    // ---------------------------------------------------------------------
    ("deepseek-chat", ModelPricing::other(0.27, 1.10)),
    ("deepseek-reasoner", ModelPricing::other(0.55, 2.19)),
    ("deepseek-r1", ModelPricing::other(0.55, 2.19)),
    // ---------------------------------------------------------------------
    // Qwen / QwQ
    // ---------------------------------------------------------------------
    ("qwen-max", ModelPricing::other(0.50, 2.0)),
    ("qwen-plus", ModelPricing::other(0.40, 1.20)),
    ("qwen-turbo", ModelPricing::other(0.30, 0.60)),
    ("qwen-long", ModelPricing::other(0.50, 2.0)),
    ("qwq-32b", ModelPricing::other(0.50, 2.0)),
];

/// Look up pricing for a model by ordered prefix match (case-insensitive).
///
/// Returns the first [`PRICING_TABLE`] entry whose prefix the lower-cased
/// `model` starts with, or [`None`] if no entry matches.  Emits a single
/// `tracing::warn!` on miss so unknown models surface in operator logs.
#[must_use]
pub fn get_pricing(model: &str) -> Option<ModelPricing> {
    let key = model.to_lowercase();
    let hit = PRICING_TABLE
        .iter()
        .find(|(prefix, _)| key.starts_with(prefix))
        .map(|(_, pricing)| *pricing);
    if hit.is_none() {
        tracing::warn!(model = %model, "unknown pricing for model");
    }
    hit
}

/// Calculate the cost for given token usage and model.
///
/// Defaults to the 5 m ephemeral cache-write multiplier — equivalent to
/// the Anthropic API behaviour when `cache_control.ttl` is omitted.
/// Callers that have an explicit TTL should use
/// [`calculate_cost_with_ttl`].
///
/// # Errors
///
/// Returns [`PricingError::UnknownModel`] when `model` does not match any
/// entry in [`PRICING_TABLE`].  Previously this case returned
/// `Some(0.0)` in downstream `.unwrap_or(0.0)` paths; the [`Result`]
/// forces callers to make the choice explicit.
pub fn calculate_cost(model: &str, usage: &TokenUsage) -> Result<f64, PricingError> {
    calculate_cost_with_ttl(model, usage, CacheWriteTtl::FiveMinutes)
}

/// Calculate cost with explicit cache-write TTL.
///
/// # Errors
///
/// Returns [`PricingError::UnknownModel`] when `model` does not match any
/// entry in [`PRICING_TABLE`].
pub fn calculate_cost_with_ttl(
    model: &str,
    usage: &TokenUsage,
    ttl: CacheWriteTtl,
) -> Result<f64, PricingError> {
    let pricing = get_pricing(model).ok_or_else(|| PricingError::UnknownModel(model.to_string()))?;

    let input = f64_from_tokens(usage.input_tokens);
    let output = f64_from_tokens(usage.output_tokens);
    let cache_read = f64_from_tokens(usage.cache_read_tokens);
    let cache_write = f64_from_tokens(usage.cache_write_tokens);

    let input_cost = input * pricing.input_per_million / 1_000_000.0;
    let output_cost = output * pricing.output_per_million / 1_000_000.0;
    let cache_read_cost =
        cache_read * pricing.input_per_million * pricing.cache_read_multiplier / 1_000_000.0;
    let cache_write_cost =
        cache_write * pricing.input_per_million * pricing.cache_write_multiplier(ttl) / 1_000_000.0;

    Ok(input_cost + output_cost + cache_read_cost + cache_write_cost)
}

/// Lossless `u64 -> f64` conversion for token counts.
///
/// `f64` exactly represents every integer up to `2^53`.  Realistic token
/// counts per request are well under `u32::MAX` (~4.3 billion); for the
/// pathological case of a `u64` larger than that we saturate to
/// `u32::MAX` so the conversion via [`f64::from`] is exact and clippy's
/// pedantic `cast_precision_loss` lint does not fire.  Saturation here
/// is strictly preferable to silent precision loss: it produces an
/// obviously wrong (very large) cost number rather than a subtly wrong
/// one.
fn f64_from_tokens(n: u64) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Mandate test 1 — gpt-5.2 resolves BEFORE gpt-5 in the ordered walk.
    // Regression guard against the original 17-branch if/else hazard
    // where reordering silently broke pricing.
    // -----------------------------------------------------------------------
    #[test]
    fn ordering_gpt_5_2_resolves_before_gpt_5() {
        // gpt-5.2 has its own row at $2/M input / $8/M output (identical
        // to gpt-5 today, but the *resolution path* must hit the 5.2
        // row, not the 5 row).
        let idx_5_2 = PRICING_TABLE
            .iter()
            .position(|(p, _)| *p == "gpt-5.2")
            .expect("table must contain gpt-5.2 prefix");
        let idx_5 = PRICING_TABLE
            .iter()
            .position(|(p, _)| *p == "gpt-5")
            .expect("table must contain gpt-5 prefix");
        assert!(
            idx_5_2 < idx_5,
            "gpt-5.2 must precede gpt-5 in PRICING_TABLE (got {idx_5_2} vs {idx_5}); \
             otherwise starts_with(\"gpt-5\") shadows the 5.2 entry"
        );
        // And the lookup actually resolves.
        assert!(get_pricing("gpt-5.2").is_some());
        assert!(get_pricing("gpt-5.2-turbo").is_some());
    }

    // -----------------------------------------------------------------------
    // Mandate test 2 — gemini-2.0-flash resolves before any shorter
    // gemini-2 prefix (none exists today, but if one is ever added it
    // MUST go after the specific flash/pro entries).
    // -----------------------------------------------------------------------
    #[test]
    fn ordering_gemini_2_0_flash_resolves_specifically() {
        let idx_flash = PRICING_TABLE
            .iter()
            .position(|(p, _)| *p == "gemini-2.0-flash")
            .expect("table must contain gemini-2.0-flash");
        // Any future bare `gemini-2` prefix must appear strictly after.
        for (i, (prefix, _)) in PRICING_TABLE.iter().enumerate() {
            if *prefix == "gemini-2" {
                assert!(
                    i > idx_flash,
                    "a bare `gemini-2` prefix at {i} would shadow `gemini-2.0-flash` at {idx_flash}"
                );
            }
        }
        // And the specific flash entry resolves.
        let p = get_pricing("gemini-2.0-flash").expect("must resolve");
        assert!((p.input_per_million - 0.075).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Mandate test 3 — ordering sanity: no entry is a strict prefix of an
    // earlier entry.  This is the structural invariant of an ordered
    // prefix table; violating it means the earlier entry would shadow
    // the later one for any model id long enough to match both.
    // -----------------------------------------------------------------------
    #[test]
    fn ordering_no_entry_is_prefix_of_earlier() {
        for (later_idx, (later_prefix, _)) in PRICING_TABLE.iter().enumerate() {
            for (earlier_prefix, _) in &PRICING_TABLE[..later_idx] {
                assert!(
                    !later_prefix.starts_with(earlier_prefix),
                    "PRICING_TABLE ordering violation: `{later_prefix}` (at {later_idx}) is \
                     shadowed by earlier entry `{earlier_prefix}` — move `{later_prefix}` \
                     before `{earlier_prefix}` or rename it"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mandate test 4 — unknown model returns PricingError::UnknownModel,
    // carrying the original input verbatim for operator diagnostics.
    // -----------------------------------------------------------------------
    #[test]
    fn unknown_model_returns_err() {
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let err = calculate_cost("totally-unknown-model-xyz", &usage).unwrap_err();
        assert_eq!(
            err,
            PricingError::UnknownModel("totally-unknown-model-xyz".to_string())
        );
        // get_pricing surface is still Option-returning so the lookup
        // primitive stays cheap; the Result lives on the cost API.
        assert!(get_pricing("totally-unknown-model-xyz").is_none());
        assert!(get_pricing("").is_none());
    }

    // -----------------------------------------------------------------------
    // Mandate test 5 — 1 h cache write uses the 2.0× multiplier for
    // Anthropic models (the under-billing bug the issue called out).
    // -----------------------------------------------------------------------
    #[test]
    fn anthropic_1hr_cache_write_uses_two_x_multiplier() {
        // 1 M cache-write tokens at Sonnet input pricing ($3/M):
        //   5 m → 3 × 1.25 = $3.75
        //   1 h → 3 × 2.00 = $6.00
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 1_000_000,
        };
        let cost_five_min =
            calculate_cost_with_ttl("claude-sonnet-4-5", &usage, CacheWriteTtl::FiveMinutes)
                .expect("sonnet must resolve");
        let cost_one_hour =
            calculate_cost_with_ttl("claude-sonnet-4-5", &usage, CacheWriteTtl::OneHour)
                .expect("sonnet must resolve");
        assert!(
            (cost_five_min - 3.75).abs() < 1e-9,
            "5m cache write must be 1.25× input (got {cost_five_min}, expected $3.75)"
        );
        assert!(
            (cost_one_hour - 6.0).abs() < 1e-9,
            "1h cache write must be 2.0× input (got {cost_one_hour}, expected $6.00) — \
             this is the under-billing bug from issue #388"
        );
        // And the 1 h cost is strictly greater than the 5 m cost, which
        // is the operational invariant downstream budget code relies on.
        assert!(
            cost_one_hour > cost_five_min,
            "1h cache write must cost more than 5m"
        );
    }

    // -----------------------------------------------------------------------
    // Mandate test 6 — property test: every model name that appears as a
    // string literal in src/providers/ resolves through the pricing
    // table.  The list is curated (not auto-scraped at test time) so the
    // assertion stays deterministic and the failure mode is obvious:
    // adding a new model literal to a provider that has no PRICING_TABLE
    // entry breaks this test, forcing the pricing entry to be added in
    // the same change.
    // -----------------------------------------------------------------------
    #[test]
    fn every_provider_model_name_resolves() {
        // Curated from grep across src/providers/*.rs at #388.  Add new
        // models here when they're introduced in a provider; the test
        // will fail loudly if PRICING_TABLE is out of sync.
        let provider_models: &[&str] = &[
            // Anthropic provider
            "claude-3-5-haiku-20241022",
            "claude-3-sonnet",
            "claude-code-20250219",
            "claude-opus-4",
            "claude-opus-4-6",
            "claude-opus-4-7",
            // OpenAI provider
            "gpt-4",
            "gpt-4o",
            "gpt-4o-mini",
            "o1",
            "o1-preview",
            "o3",
            "o3-mini",
            "o4",
            "o4-pro",
            // Google provider
            "gemini-2.5-pro",
            // DeepSeek
            "deepseek-r1",
            "deepseek-reasoner",
            // Qwen / QwQ
            "qwen-long",
            "qwq-32b",
        ];

        let mut missing = Vec::new();
        for name in provider_models {
            if get_pricing(name).is_none() {
                missing.push(*name);
            }
        }
        assert!(
            missing.is_empty(),
            "PRICING_TABLE is missing entries for provider model(s): {missing:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Behavioural regressions retained from #495 work — these keep the
    // pre-refactor cost numbers intact for default-TTL workloads.
    // -----------------------------------------------------------------------

    #[test]
    fn known_model_basic_lookup() {
        assert!(get_pricing("claude-3-opus-20240229").is_some());
        assert!(get_pricing("claude-3-sonnet-20240229").is_some());
        assert!(get_pricing("claude-3-haiku-20240307").is_some());
        assert!(get_pricing("gpt-4o").is_some());
        assert!(get_pricing("gpt-4o-mini").is_some());
        assert!(get_pricing("gemini-2.0-flash").is_some());
        assert!(get_pricing("deepseek-chat").is_some());
    }

    /// Haiku pricing — 1 M input + 0.1 M output ≈ $0.375.
    #[test]
    fn basic_cost_haiku() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let cost = calculate_cost("claude-3-haiku-20240307", &usage).expect("haiku must resolve");
        // 0.25 input + 0.125 output = 0.375
        assert!((cost - 0.375).abs() < 1e-9, "expected ~$0.375, got {cost}");
    }

    /// Exact rate declarations from `PRICING_TABLE` are returned verbatim.
    #[test]
    fn exact_rates_returned() {
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

    /// Case-insensitive lookup on the input.
    #[test]
    fn lookup_is_case_insensitive() {
        assert!(get_pricing("GPT-4o").is_some());
        assert!(get_pricing("Claude-3-Haiku-20240307").is_some());
    }

    /// Cache-read tokens apply the 0.1× ratio (Anthropic).
    #[test]
    fn cache_read_tokens_apply_point_one_ratio() {
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0,
        };
        let cost = calculate_cost("claude-sonnet-4-5", &usage).expect("sonnet must resolve");
        let expected = 3.0 * 0.1; // $0.30
        assert!(
            (cost - expected).abs() < 1e-9,
            "cache-read must be 0.1× input; got {cost}, expected {expected}"
        );
    }

    /// Default-TTL cache write (5 m) → 1.25× input.
    #[test]
    fn default_cache_write_tokens_apply_one_point_two_five_ratio() {
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 1_000_000,
        };
        let cost = calculate_cost("claude-sonnet-4-5", &usage).expect("sonnet must resolve");
        let expected = 3.0 * 1.25; // $3.75
        assert!(
            (cost - expected).abs() < 1e-9,
            "default cache-write (5m) must be 1.25× input; got {cost}, expected {expected}"
        );
    }

    /// Combined four-bucket sum on default TTL.
    #[test]
    fn all_four_token_buckets_sum_correctly() {
        // Haiku: $0.25/M input, $1.25/M output.
        // cache_read  = 0.25 × 0.1  = $0.025/M
        // cache_write = 0.25 × 1.25 = $0.3125/M
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
        };
        let cost = calculate_cost("claude-3-haiku-20240307", &usage).expect("haiku must resolve");
        let expected = 0.25f64.mul_add(1.25, 0.25f64.mul_add(0.1, 0.25 + 1.25));
        assert!(
            (cost - expected).abs() < 1e-9,
            "four-bucket sum wrong; got {cost}, expected {expected}"
        );
    }

    /// Zero usage returns Ok(0.0), not Err.
    #[test]
    fn zero_tokens_returns_zero_cost() {
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let cost = calculate_cost("claude-3-haiku-20240307", &usage).expect("haiku must resolve");
        assert!(cost.abs() < f64::EPSILON);
    }

    /// Sanity: claude-opus-4 prefix covers the dated id, the -5/-6/-7
    /// roll-forwards, AND a bare `claude-opus-4` alias.
    #[test]
    fn claude_opus_4_prefix_covers_dated_and_rollforward() {
        for id in [
            "claude-opus-4",
            "claude-opus-4-20250514",
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
        ] {
            let p = get_pricing(id).unwrap_or_else(|| panic!("{id} must resolve"));
            assert!((p.input_per_million - 15.0).abs() < f64::EPSILON, "{id}");
            assert!((p.output_per_million - 75.0).abs() < f64::EPSILON, "{id}");
        }
    }

    /// Sanity: claude-3-opus and claude-opus-4 stay distinct families
    /// (the original failure mode that motivated exact-match in #495).
    /// With the ordered prefix table they share no common prefix.
    #[test]
    fn opus_3_and_opus_4_are_distinct_families() {
        let opus3 = get_pricing("claude-3-opus-20240229").expect("opus3 must resolve");
        let opus4 = get_pricing("claude-opus-4-20250514").expect("opus4 must resolve");
        // Both happen to share the $15 / $75 rate sheet today, so we
        // assert they resolved through different table entries instead
        // of through the same shadowing prefix.  We do this by reading
        // back which prefix matched each id.
        let key3 = "claude-3-opus-20240229";
        let key4 = "claude-opus-4-20250514";
        let prefix3 = PRICING_TABLE
            .iter()
            .find(|(p, _)| key3.starts_with(p))
            .map(|(p, _)| *p);
        let prefix4 = PRICING_TABLE
            .iter()
            .find(|(p, _)| key4.starts_with(p))
            .map(|(p, _)| *p);
        assert_eq!(prefix3, Some("claude-3-opus"));
        assert_eq!(prefix4, Some("claude-opus-4-"));
        assert_ne!(
            prefix3, prefix4,
            "opus-3 and opus-4 must resolve through distinct PRICING_TABLE rows"
        );
        // And the rates themselves still match the family's price sheet.
        assert!((opus3.input_per_million - opus4.input_per_million).abs() < f64::EPSILON);
    }
}
