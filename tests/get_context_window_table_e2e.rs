//! End-to-end tests for `compaction::get_context_window` —
//! exact per-model constants pinned (current Claude long-context
//! models at 1M, older Claude family at 200k, GPT-5.5/5.4 at
//! 1M/1.05M, GPT-4o at 128k, GPT-4.1 at 1M, GPT-5 at 400k,
//! Gemini Pro at 1M, DeepSeek V4 at 1M, Qwen current
//! families at 1M or 256k, Kimi/Moonshot at their documented
//! family windows, MiniMax M3 at 1M / M2.x at 204.8k),
//! the substring-precedence rule (gpt-4o matches
//! BEFORE generic gpt-4), the unknown-model fallback, and
//! case-insensitivity.
//!
//! Sprint 170 of the verification effort. Sprint 92 covered
//! a few cases; this file pins the exact table values
//! and the table-walk-order contract that prevents
//! "gpt-4o".contains("gpt-4") from misclassifying.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::compaction::get_context_window;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Claude family
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn current_claude_long_context_models_return_1m() {
    for model in [
        "claude-fable-5",
        "claude-mythos-5",
        "claude-mythos-preview",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
    ] {
        assert_eq!(get_context_window(model), 1_000_000, "{model}");
    }
}

#[test]
fn older_claude_opus_returns_200k() {
    assert_eq!(get_context_window("claude-3-opus"), 200_000);
    assert_eq!(get_context_window("claude-opus-4"), 200_000);
    assert_eq!(get_context_window("claude-opus-4-5"), 200_000);
}

#[test]
fn older_claude_sonnet_returns_200k() {
    assert_eq!(get_context_window("claude-3-5-sonnet"), 200_000);
    assert_eq!(get_context_window("claude-sonnet-4-5"), 200_000);
}

#[test]
fn claude_haiku_returns_200k() {
    assert_eq!(get_context_window("claude-3-haiku"), 200_000);
    assert_eq!(get_context_window("claude-haiku-4"), 200_000);
}

#[test]
fn bare_claude_falls_through_to_claude_generic_200k() {
    // PINS DOC: bare "claude" without opus/sonnet/haiku
    // tag still hits the "claude" needle (last Claude row).
    assert_eq!(get_context_window("claude"), 200_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — GPT family + substring precedence (#DOC)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn current_gpt_5_5_and_5_4_long_context_models_return_documented_windows() {
    assert_eq!(get_context_window("gpt-5.5-pro"), 1_050_000);
    assert_eq!(get_context_window("gpt-5.5"), 1_000_000);
    assert_eq!(get_context_window("gpt-5.5-2026-04-23"), 1_000_000);
    assert_eq!(get_context_window("gpt-5.4-pro"), 1_050_000);
    assert_eq!(get_context_window("gpt-5.4"), 1_000_000);
    assert_eq!(get_context_window("gpt-5.4-2026-03-05"), 1_000_000);
}

#[test]
fn current_gpt_5_4_small_models_remain_400k() {
    assert_eq!(get_context_window("gpt-5.4-mini"), 400_000);
    assert_eq!(get_context_window("gpt-5.4-mini-2026-03-17"), 400_000);
    assert_eq!(get_context_window("gpt-5.4-nano"), 400_000);
    assert_eq!(get_context_window("gpt-5.4-nano-2026-03-17"), 400_000);
}

#[test]
fn gpt_5_returns_400k() {
    assert_eq!(get_context_window("gpt-5"), 400_000);
    assert_eq!(get_context_window("gpt-5-mini"), 400_000);
}

#[test]
fn gpt_4_1_returns_1m_tokens() {
    // PINS WIRE: gpt-4.1 has 1M context.
    assert_eq!(get_context_window("gpt-4.1"), 1_000_000);
}

#[test]
fn gpt_4o_returns_128k_distinct_from_gpt_4_1() {
    // PINS PRECEDENCE: "gpt-4o" MUST match BEFORE generic
    // "gpt-4" row — `"gpt-4o".contains("gpt-4")` would
    // otherwise misclassify to 128k anyway, but the contract
    // documents that 4o's row wins explicitly.
    assert_eq!(get_context_window("gpt-4o"), 128_000);
    assert_eq!(get_context_window("gpt-4o-mini"), 128_000);
}

#[test]
fn gpt_3_5_returns_16_385() {
    // PINS WIRE: GPT-3.5 has 16k context (precise: 16,385).
    assert_eq!(get_context_window("gpt-3.5-turbo"), 16_385);
    assert_eq!(get_context_window("gpt-3.5"), 16_385);
}

#[test]
fn gpt_4_1_wins_over_generic_gpt_4_due_to_table_order() {
    // PINS TABLE-ORDER: gpt-4.1 declared BEFORE gpt-4o
    // BEFORE generic gpt-4. So "gpt-4.1-turbo" matches 4.1's
    // 1M, NOT 4o's 128k or 4's 128k.
    assert_eq!(get_context_window("gpt-4.1-turbo"), 1_000_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Gemini
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn gemini_pro_returns_1m_tokens() {
    // PINS WIRE: Gemini Pro has 1M context.
    assert_eq!(get_context_window("gemini-1.5-pro"), 1_000_000);
    assert_eq!(get_context_window("gemini-2.5-pro"), 1_000_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — DeepSeek
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn current_deepseek_v4_models_and_aliases_return_1m() {
    for model in [
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "deepseek-chat",
        "deepseek-reasoner",
    ] {
        assert_eq!(get_context_window(model), 1_000_000, "{model}");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Qwen
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn current_qwen_1m_models_return_1m() {
    for model in [
        "qwen3.7-plus",
        "qwen3.7-plus-2026-05-26",
        "qwen3.7-max",
        "qwen3.7-max-2026-05-17",
        "qwen3.7-max-2026-06-08",
        "qwen3.6-plus",
        "qwen3.6-flash",
        "qwen3.5-plus",
        "qwen3.5-flash",
        "qwen3-coder-plus",
        "qwen3-coder-flash",
        "qwen-plus-2025-12-01",
        "qwen-flash-2025-07-28",
    ] {
        assert_eq!(get_context_window(model), 1_000_000, "{model}");
    }
}

#[test]
fn qwen_max_and_open_weight_families_return_documented_smaller_windows() {
    for model in [
        "qwen3.6-max-preview",
        "qwen3.6-35b-a3b",
        "qwen3.5-397b-a17b",
        "qwen3.5-122b-a10b",
        "qwen3.5-27b",
        "qwen3.5-35b-a3b",
        "qwen3-max",
        "qwen3-max-2026-01-23",
    ] {
        assert_eq!(get_context_window(model), 262_144, "{model}");
    }

    assert_eq!(get_context_window("qwen-max"), 32_768);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Kimi / Moonshot
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn kimi_k2_models_return_256k() {
    for model in [
        "kimi-k2.7-code",
        "kimi-k2.7-code-highspeed",
        "kimi-k2.6",
        "kimi-k2.5",
    ] {
        assert_eq!(get_context_window(model), 262_144, "{model}");
    }
}

#[test]
fn moonshot_v1_models_return_documented_windows() {
    for model in ["moonshot-v1-128k", "moonshot-v1-128k-vision-preview"] {
        assert_eq!(get_context_window(model), 131_072, "{model}");
    }
    for model in ["moonshot-v1-32k", "moonshot-v1-32k-vision-preview"] {
        assert_eq!(get_context_window(model), 32_768, "{model}");
    }
    for model in ["moonshot-v1-8k", "moonshot-v1-8k-vision-preview"] {
        assert_eq!(get_context_window(model), 8_192, "{model}");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — MiniMax
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn minimax_models_return_documented_windows() {
    assert_eq!(get_context_window("MiniMax-M3"), 1_000_000);
    for model in [
        "MiniMax-M2.7",
        "MiniMax-M2.7-highspeed",
        "MiniMax-M2.5",
        "MiniMax-M2.5-highspeed",
        "MiniMax-M2.1",
        "MiniMax-M2.1-highspeed",
        "MiniMax-M2",
    ] {
        assert_eq!(get_context_window(model), 204_800, "{model}");
    }
    assert_eq!(get_context_window("M2-her"), 65_536);
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — Unknown model fallback
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_model_falls_back_to_default_128k() {
    // PINS DEFAULT: DEFAULT_CONTEXT = 128_000.
    assert_eq!(get_context_window("totally-unknown-model-xyz"), 128_000);
}

#[test]
fn empty_string_returns_default() {
    assert_eq!(get_context_window(""), 128_000);
}

#[test]
fn arbitrary_provider_name_returns_default() {
    assert_eq!(get_context_window("llama-3.1"), 128_000);
    assert_eq!(get_context_window("mistral-large"), 128_000);
}

#[test]
fn random_bytes_in_name_return_default_no_panic() {
    // Junk model names MUST NOT panic, just fall through.
    let huge = "x".repeat(10_000);
    let cw = get_context_window(&huge);
    assert_eq!(cw, 128_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section I — Case insensitivity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn lookup_is_case_insensitive_for_opus() {
    assert_eq!(get_context_window("CLAUDE-OPUS"), 200_000);
    assert_eq!(get_context_window("Claude-Opus"), 200_000);
    assert_eq!(get_context_window("CLAUDE-3-OPUS"), 200_000);
}

#[test]
fn lookup_is_case_insensitive_for_gpt() {
    assert_eq!(get_context_window("GPT-4O"), 128_000);
    assert_eq!(get_context_window("GPT-5"), 400_000);
    assert_eq!(get_context_window("GPT-4.1"), 1_000_000);
}

#[test]
fn lookup_is_case_insensitive_for_gemini() {
    assert_eq!(get_context_window("GEMINI-2.5-PRO"), 1_000_000);
}

#[test]
fn lookup_is_case_insensitive_for_deepseek() {
    assert_eq!(get_context_window("DEEPSEEK-V4-PRO"), 1_000_000);
    assert_eq!(get_context_window("DeepSeek-Reasoner"), 1_000_000);
}

#[test]
fn lookup_is_case_insensitive_for_qwen() {
    assert_eq!(get_context_window("QWEN3.7-PLUS"), 1_000_000);
    assert_eq!(get_context_window("Qwen3.6-Max-Preview"), 262_144);
}

#[test]
fn lookup_is_case_insensitive_for_kimi_and_minimax() {
    assert_eq!(get_context_window("KIMI-K2.7-CODE"), 262_144);
    assert_eq!(get_context_window("MOONSHOT-V1-32K"), 32_768);
    assert_eq!(get_context_window("minimax-m3"), 1_000_000);
    assert_eq!(get_context_window("minimax-m2.7-highspeed"), 204_800);
    assert_eq!(get_context_window("m2-HER"), 65_536);
}

// ───────────────────────────────────────────────────────────────────────────
// Section J — Substring match (PINS NOT-PREFIX, contains-anywhere)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn match_is_substring_anywhere_not_just_prefix() {
    // PINS DOC: `contains(row.needle)` — substring match,
    // so a model with the needle embedded mid-name still hits.
    // E.g. "anthropic/claude-3-opus-bedrock" contains "opus".
    assert_eq!(
        get_context_window("anthropic/claude-3-opus-bedrock"),
        200_000
    );
    assert_eq!(
        get_context_window("org/my-fine-tune-of-gpt-4o-snapshot"),
        128_000
    );
}

#[test]
fn earlier_needle_in_table_wins_over_later_within_same_model_string() {
    // "claude-3-opus" contains both "opus" (row 1) and
    // "claude" (row 4). The first match wins → 200k either way,
    // but pins the find()-returns-first contract.
    assert_eq!(get_context_window("claude-3-opus"), 200_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section K — Return value always positive + sensible
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_documented_model_returns_at_least_16k_tokens() {
    // PINS LOWER BOUND: even the smallest context (GPT-3.5)
    // is ≥16k. No documented model returns 0.
    let models = [
        "claude-opus",
        "claude-sonnet",
        "claude-haiku",
        "gpt-5",
        "gpt-4.1",
        "gpt-4o",
        "gpt-3.5-turbo",
        "gemini-2.5-pro",
        "unknown-xyz",
    ];
    for m in models {
        let cw = get_context_window(m);
        assert!(cw >= 16_000, "{m}: context {cw} MUST be >= 16k");
    }
}

#[test]
fn every_documented_model_returns_at_most_1_05m_tokens() {
    // PINS UPPER BOUND: largest documented text window here is
    // 1.05M for GPT-5.5 Pro / GPT-5.4 Pro.
    let models = [
        "claude-opus",
        "claude-sonnet",
        "claude-haiku",
        "gpt-5.5-pro",
        "gpt-5.5",
        "gpt-5.4-pro",
        "gpt-5.4",
        "gpt-5",
        "gpt-4.1",
        "gpt-4o",
        "gpt-3.5-turbo",
        "gemini-2.5-pro",
        "unknown-xyz",
    ];
    for m in models {
        let cw = get_context_window(m);
        assert!(cw <= 1_050_000, "{m}: context {cw} MUST NOT exceed 1.05M");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — Idempotency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn repeated_lookups_yield_same_result() {
    // PINS PURE FUNCTION: no hidden state, no cache pollution.
    let cw1 = get_context_window("gpt-4o");
    let cw2 = get_context_window("gpt-4o");
    let cw3 = get_context_window("gpt-4o");
    assert_eq!(cw1, cw2);
    assert_eq!(cw2, cw3);
}
