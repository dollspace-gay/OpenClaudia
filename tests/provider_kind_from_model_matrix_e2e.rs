//! End-to-end tests for `providers::ProviderKind::from_model` —
//! exhaustive classification matrix across every documented
//! prefix family, the case-insensitivity invariant, the
//! "no overlapping prefix" guard (o1 vs o100, gpt vs gpt-),
//! the qwen/qwq/qvq variants, and the Unknown fallback.
//!
//! Sprint 168 of the verification effort. Sprint 106
//! covered `ProviderKind::name` + Unknown shape; this file
//! pins the classification table itself — the dispatch
//! input for `transform_request_with_thinking` + analytics.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::providers::ProviderKind;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Anthropic prefixes
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_claude_prefix_as_anthropic() {
    assert_eq!(
        ProviderKind::from_model("claude-sonnet-4-5"),
        ProviderKind::Anthropic
    );
    assert_eq!(
        ProviderKind::from_model("claude-opus-4"),
        ProviderKind::Anthropic
    );
    assert_eq!(
        ProviderKind::from_model("claude-3-5-haiku"),
        ProviderKind::Anthropic
    );
}

#[test]
fn classify_anthropic_prefix_as_anthropic() {
    // PINS DOC: both "claude" + "anthropic" prefixes map
    // to Anthropic (covers internal naming variants).
    assert_eq!(
        ProviderKind::from_model("anthropic-claude"),
        ProviderKind::Anthropic
    );
    assert_eq!(
        ProviderKind::from_model("anthropic-sonnet"),
        ProviderKind::Anthropic
    );
}

#[test]
fn classify_bare_claude_string_as_anthropic() {
    assert_eq!(ProviderKind::from_model("claude"), ProviderKind::Anthropic);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — OpenAI prefixes including o-series
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_gpt_dash_prefix_as_openai() {
    assert_eq!(ProviderKind::from_model("gpt-4o"), ProviderKind::OpenAI);
    assert_eq!(
        ProviderKind::from_model("gpt-3.5-turbo"),
        ProviderKind::OpenAI
    );
    assert_eq!(
        ProviderKind::from_model("gpt-4-turbo"),
        ProviderKind::OpenAI
    );
}

#[test]
fn classify_openai_chat_and_codex_aliases_as_openai() {
    assert_eq!(
        ProviderKind::from_model("chat-latest"),
        ProviderKind::OpenAI
    );
    assert_eq!(
        ProviderKind::from_model("codex-mini-latest"),
        ProviderKind::OpenAI
    );
}

#[test]
fn classify_bare_gpt_string_as_openai() {
    // PINS DOC: bare "gpt" alone is accepted (no dash).
    assert_eq!(ProviderKind::from_model("gpt"), ProviderKind::OpenAI);
}

#[test]
fn classify_o1_o3_o4_bare_strings_as_openai() {
    // PINS DOC: bare "o1", "o3", "o4" → OpenAI.
    assert_eq!(ProviderKind::from_model("o1"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("o3"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("o4"), ProviderKind::OpenAI);
}

#[test]
fn classify_o_series_with_dash_suffix_as_openai() {
    assert_eq!(ProviderKind::from_model("o1-mini"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("o3-mini"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("o4-mini"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("o1-preview"), ProviderKind::OpenAI);
}

#[test]
fn classify_o100_o200_do_not_match_o1_overlap_rule() {
    // PINS #DOC: "o100" MUST NOT match "o1-" prefix —
    // "no overlapping prefix heuristics". So o100 → Unknown.
    assert_eq!(ProviderKind::from_model("o100"), ProviderKind::Unknown);
    assert_eq!(ProviderKind::from_model("o2"), ProviderKind::Unknown);
    assert_eq!(ProviderKind::from_model("o5"), ProviderKind::Unknown);
}

#[test]
fn classify_o2_o5_do_not_match_unlisted_o_versions() {
    // Only o1, o3, o4 are documented. o2 and o5 are Unknown.
    assert_eq!(ProviderKind::from_model("o2"), ProviderKind::Unknown);
    assert_eq!(ProviderKind::from_model("o5"), ProviderKind::Unknown);
    assert_eq!(ProviderKind::from_model("o2-mini"), ProviderKind::Unknown);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Google Gemini
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_gemini_prefix_as_google() {
    assert_eq!(
        ProviderKind::from_model("gemini-3.5-flash"),
        ProviderKind::Google
    );
    assert_eq!(
        ProviderKind::from_model("gemini-2.5-pro"),
        ProviderKind::Google
    );
    assert_eq!(
        ProviderKind::from_model("gemini-2.5-flash"),
        ProviderKind::Google
    );
    assert_eq!(
        ProviderKind::from_model("gemini-3.1-pro-preview"),
        ProviderKind::Google
    );
}

#[test]
fn classify_bare_gemini_as_google() {
    assert_eq!(ProviderKind::from_model("gemini"), ProviderKind::Google);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — DeepSeek
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_deepseek_prefix_as_deepseek() {
    assert_eq!(
        ProviderKind::from_model("deepseek-chat"),
        ProviderKind::DeepSeek
    );
    assert_eq!(
        ProviderKind::from_model("deepseek-reasoner"),
        ProviderKind::DeepSeek
    );
    assert_eq!(
        ProviderKind::from_model("deepseek-v3"),
        ProviderKind::DeepSeek
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Qwen / QwQ / QvQ
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_qwen_prefix_as_qwen() {
    assert_eq!(ProviderKind::from_model("qwen-max"), ProviderKind::Qwen);
    assert_eq!(ProviderKind::from_model("qwen-2.5-72b"), ProviderKind::Qwen);
}

#[test]
fn classify_qwq_prefix_as_qwen() {
    // PINS DOC: QwQ reasoning models also map to Qwen.
    assert_eq!(ProviderKind::from_model("qwq-32b"), ProviderKind::Qwen);
    assert_eq!(
        ProviderKind::from_model("qwq-32b-preview"),
        ProviderKind::Qwen
    );
}

#[test]
fn classify_qvq_prefix_as_qwen() {
    // PINS DOC: QvQ visual reasoning also maps to Qwen.
    assert_eq!(
        ProviderKind::from_model("qvq-72b-preview"),
        ProviderKind::Qwen
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Z.AI / GLM
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_glm_prefix_as_zai() {
    assert_eq!(ProviderKind::from_model("glm-4"), ProviderKind::Zai);
    assert_eq!(ProviderKind::from_model("glm-4-plus"), ProviderKind::Zai);
    assert_eq!(ProviderKind::from_model("glm-4.5-air"), ProviderKind::Zai);
}

#[test]
fn classify_bare_glm_as_zai() {
    assert_eq!(ProviderKind::from_model("glm"), ProviderKind::Zai);
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Kimi / Moonshot
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_kimi_prefix_as_kimi() {
    assert_eq!(
        ProviderKind::from_model("kimi-k2.7-code"),
        ProviderKind::Kimi
    );
    assert_eq!(ProviderKind::from_model("kimi-k2.6"), ProviderKind::Kimi);
}

#[test]
fn classify_moonshot_prefix_as_kimi() {
    assert_eq!(
        ProviderKind::from_model("moonshot-v1-128k"),
        ProviderKind::Kimi
    );
    assert_eq!(
        ProviderKind::from_model("moonshot-v1-8k"),
        ProviderKind::Kimi
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — MiniMax
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_minimax_prefix_as_minimax() {
    assert_eq!(
        ProviderKind::from_model("MiniMax-M3"),
        ProviderKind::MiniMax
    );
    assert_eq!(
        ProviderKind::from_model("MiniMax-M2.7-highspeed"),
        ProviderKind::MiniMax
    );
    assert_eq!(ProviderKind::from_model("M2-her"), ProviderKind::MiniMax);
}

// ───────────────────────────────────────────────────────────────────────────
// Section I — Case-insensitivity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classification_is_case_insensitive_for_anthropic() {
    assert_eq!(
        ProviderKind::from_model("CLAUDE-SONNET-4-5"),
        ProviderKind::Anthropic
    );
    assert_eq!(
        ProviderKind::from_model("Claude-Opus-4"),
        ProviderKind::Anthropic
    );
}

#[test]
fn classification_is_case_insensitive_for_gemini() {
    assert_eq!(
        ProviderKind::from_model("GEMINI-2.5-PRO"),
        ProviderKind::Google
    );
    assert_eq!(
        ProviderKind::from_model("Gemini-2.5-Flash"),
        ProviderKind::Google
    );
}

#[test]
fn classification_is_case_insensitive_for_qwen() {
    assert_eq!(ProviderKind::from_model("QWEN-MAX"), ProviderKind::Qwen);
    assert_eq!(ProviderKind::from_model("QwQ-32B"), ProviderKind::Qwen);
}

#[test]
fn classification_is_case_insensitive_for_openai_o_series() {
    assert_eq!(ProviderKind::from_model("O1-MINI"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("O3"), ProviderKind::OpenAI);
    assert_eq!(ProviderKind::from_model("GPT-4O"), ProviderKind::OpenAI);
    assert_eq!(
        ProviderKind::from_model("CODEX-MINI-LATEST"),
        ProviderKind::OpenAI
    );
}

#[test]
fn classification_is_case_insensitive_for_kimi_minimax() {
    assert_eq!(
        ProviderKind::from_model("KIMI-K2.7-CODE"),
        ProviderKind::Kimi
    );
    assert_eq!(
        ProviderKind::from_model("MOONSHOT-V1-8K"),
        ProviderKind::Kimi
    );
    assert_eq!(
        ProviderKind::from_model("MINIMAX-M3"),
        ProviderKind::MiniMax
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section J — Unknown fallback
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_empty_string_as_unknown() {
    assert_eq!(ProviderKind::from_model(""), ProviderKind::Unknown);
}

#[test]
fn classify_arbitrary_model_name_as_unknown() {
    assert_eq!(ProviderKind::from_model("llama3.1"), ProviderKind::Unknown);
    assert_eq!(
        ProviderKind::from_model("mistral-large"),
        ProviderKind::Unknown
    );
    assert_eq!(
        ProviderKind::from_model("random-bogus-model-xyz"),
        ProviderKind::Unknown
    );
}

#[test]
fn classify_provider_name_substring_not_at_start_as_unknown() {
    // PINS PREFIX (NOT SUBSTRING): the matcher requires a
    // prefix match, not a substring. "abc-claude" is NOT
    // Anthropic.
    assert_eq!(
        ProviderKind::from_model("abc-claude"),
        ProviderKind::Unknown
    );
    assert_eq!(ProviderKind::from_model("xx-gpt-4o"), ProviderKind::Unknown);
    assert_eq!(
        ProviderKind::from_model("foo-gemini"),
        ProviderKind::Unknown
    );
}

#[test]
fn classify_only_dash_or_whitespace_as_unknown() {
    assert_eq!(ProviderKind::from_model("-"), ProviderKind::Unknown);
    assert_eq!(ProviderKind::from_model("   "), ProviderKind::Unknown);
}

// ───────────────────────────────────────────────────────────────────────────
// Section K — Round-trip with name()
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn from_model_then_name_yields_canonical_provider_name() {
    let pairs = [
        ("claude-sonnet-4-5", "anthropic"),
        ("gpt-4o", "openai"),
        ("chat-latest", "openai"),
        ("codex-mini-latest", "openai"),
        ("o1-mini", "openai"),
        ("gemini-2.5-pro", "google"),
        ("deepseek-chat", "deepseek"),
        ("qwen-max", "qwen"),
        ("qwq-32b", "qwen"),
        ("glm-4", "zai"),
        ("kimi-k2.7-code", "kimi"),
        ("moonshot-v1-8k", "kimi"),
        ("MiniMax-M3", "minimax"),
        ("M2-her", "minimax"),
        ("unknown_model", "unknown"),
    ];
    for (model, expected_name) in pairs {
        let kind = ProviderKind::from_model(model);
        assert_eq!(
            kind.name(),
            expected_name,
            "model {model:?} MUST classify to {expected_name:?}"
        );
    }
}
