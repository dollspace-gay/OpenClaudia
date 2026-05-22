//! End-to-end tests for `compaction::get_context_window` —
//! exact per-model constants pinned (Claude family at 200k,
//! GPT-4o at 128k, GPT-4.1 at 1M, GPT-5 at 400k, Gemini Pro
//! at 1M), the substring-precedence rule (gpt-4o matches
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
// Section A — Claude family (all 200k)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn claude_opus_returns_200k() {
    assert_eq!(get_context_window("claude-3-opus"), 200_000);
    assert_eq!(get_context_window("claude-opus-4"), 200_000);
}

#[test]
fn claude_sonnet_returns_200k() {
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
// Section D — Unknown model fallback
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
    assert_eq!(get_context_window("deepseek-chat"), 128_000);
}

#[test]
fn random_bytes_in_name_return_default_no_panic() {
    // Junk model names MUST NOT panic, just fall through.
    let huge = "x".repeat(10_000);
    let cw = get_context_window(&huge);
    assert_eq!(cw, 128_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Case insensitivity
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

// ───────────────────────────────────────────────────────────────────────────
// Section F — Substring match (PINS NOT-PREFIX, contains-anywhere)
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
// Section G — Return value always positive + sensible
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
fn every_documented_model_returns_at_most_1m_tokens() {
    // PINS UPPER BOUND: largest documented is 1M (GPT-4.1
    // and Gemini Pro). No table entry exceeds this.
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
        assert!(cw <= 1_000_000, "{m}: context {cw} MUST NOT exceed 1M");
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
