//! End-to-end tests for `providers::get_adapter` —
//! exhaustive alias matrix (Google has 2 aliases, Z.AI has
//! 3, etc.), case-insensitivity, and `UnknownProvider`
//! error shape including the documented supported-list.
//!
//! Sprint 156 of the verification effort. Sprint 17
//! covered the provider transform shapes for a subset;
//! this file pins the alias-resolution table that lets
//! `proxy.target: gemini` and `proxy.target: google`
//! resolve to the same singleton adapter.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::providers::{get_adapter, ProviderError};

// ───────────────────────────────────────────────────────────────────────────
// Section A — Canonical provider names
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_canonical_resolves() {
    let adapter = get_adapter("anthropic").expect("anthropic");
    assert_eq!(adapter.name(), "anthropic");
}

#[test]
fn openai_canonical_resolves() {
    let adapter = get_adapter("openai").expect("openai");
    assert!(adapter.name().contains("openai") || adapter.name() == "openai");
}

#[test]
fn google_canonical_resolves() {
    let _ = get_adapter("google").expect("google");
}

#[test]
fn deepseek_canonical_resolves() {
    let _ = get_adapter("deepseek").expect("deepseek");
}

#[test]
fn qwen_canonical_resolves() {
    let _ = get_adapter("qwen").expect("qwen");
}

#[test]
fn zai_canonical_resolves() {
    let _ = get_adapter("zai").expect("zai");
}

#[test]
fn kimi_canonical_resolves() {
    let adapter = get_adapter("kimi").expect("kimi");
    assert_eq!(adapter.name(), "kimi");
}

#[test]
fn minimax_canonical_resolves() {
    let adapter = get_adapter("minimax").expect("minimax");
    assert_eq!(adapter.name(), "minimax");
}

#[test]
fn ollama_canonical_resolves() {
    let _ = get_adapter("ollama").expect("ollama");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Aliases — Google
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn google_alias_gemini_resolves_to_same_adapter() {
    let canonical = get_adapter("google").expect("google");
    let alias = get_adapter("gemini").expect("gemini");
    // PINS ALIAS: same singleton (Arc-equal via pointer eq).
    assert!(std::ptr::eq(canonical, alias));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Aliases — Z.AI / GLM / Zhipu
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn zai_alias_glm_resolves_to_same_adapter() {
    let canonical = get_adapter("zai").expect("zai");
    let alias = get_adapter("glm").expect("glm");
    assert!(std::ptr::eq(canonical, alias));
}

#[test]
fn zai_alias_zhipu_resolves_to_same_adapter() {
    let canonical = get_adapter("zai").expect("zai");
    let alias = get_adapter("zhipu").expect("zhipu");
    assert!(std::ptr::eq(canonical, alias));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Aliases — Qwen / Alibaba
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn qwen_alias_alibaba_resolves_to_same_adapter() {
    let canonical = get_adapter("qwen").expect("qwen");
    let alias = get_adapter("alibaba").expect("alibaba");
    assert!(std::ptr::eq(canonical, alias));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Aliases — Kimi / Moonshot
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn kimi_alias_moonshot_resolves_to_same_adapter() {
    let canonical = get_adapter("kimi").expect("kimi");
    let alias = get_adapter("moonshot").expect("moonshot");
    assert!(std::ptr::eq(canonical, alias));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Aliases — OpenAI-compatible local providers
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn openai_alias_local_resolves_to_same_adapter() {
    let canonical = get_adapter("openai").expect("openai");
    let alias = get_adapter("local").expect("local");
    assert!(std::ptr::eq(canonical, alias));
}

#[test]
fn openai_alias_lmstudio_resolves_to_same_adapter() {
    let canonical = get_adapter("openai").expect("openai");
    let alias = get_adapter("lmstudio").expect("lmstudio");
    assert!(std::ptr::eq(canonical, alias));
}

#[test]
fn openai_alias_localai_resolves_to_same_adapter() {
    let canonical = get_adapter("openai").expect("openai");
    let alias = get_adapter("localai").expect("localai");
    assert!(std::ptr::eq(canonical, alias));
}

#[test]
fn openai_alias_text_generation_webui_resolves_to_same_adapter() {
    let canonical = get_adapter("openai").expect("openai");
    let alias = get_adapter("text-generation-webui").expect("twg");
    assert!(std::ptr::eq(canonical, alias));
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Case-insensitivity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn dispatch_is_case_insensitive_for_canonical_anthropic() {
    let lower = get_adapter("anthropic").expect("lower");
    let upper = get_adapter("ANTHROPIC").expect("upper");
    let mixed = get_adapter("AnThRoPiC").expect("mixed");
    assert!(std::ptr::eq(lower, upper));
    assert!(std::ptr::eq(lower, mixed));
}

#[test]
fn dispatch_is_case_insensitive_for_alias_gemini() {
    let lower = get_adapter("gemini").expect("lower");
    let upper = get_adapter("GEMINI").expect("upper");
    assert!(std::ptr::eq(lower, upper));
}

#[test]
fn dispatch_is_case_insensitive_for_alias_lmstudio() {
    let lower = get_adapter("lmstudio").expect("lower");
    let upper = get_adapter("LMSTUDIO").expect("upper");
    let mixed = get_adapter("LmStudio").expect("mixed");
    assert!(std::ptr::eq(lower, upper));
    assert!(std::ptr::eq(lower, mixed));
}

#[test]
fn dispatch_is_case_insensitive_for_kimi_minimax() {
    let kimi = get_adapter("kimi").expect("kimi");
    let moonshot_upper = get_adapter("MOONSHOT").expect("moonshot");
    let minimax = get_adapter("minimax").expect("minimax");
    let minimax_mixed = get_adapter("MiniMax").expect("MiniMax");
    assert!(std::ptr::eq(kimi, moonshot_upper));
    assert!(std::ptr::eq(minimax, minimax_mixed));
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — Singleton sharing across calls
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn repeated_get_adapter_returns_same_singleton() {
    // PINS SINGLETON: every call returns the same &'static
    // pointer — no per-call allocation.
    let a = get_adapter("openai").expect("openai");
    let b = get_adapter("openai").expect("openai");
    let c = get_adapter("openai").expect("openai");
    assert!(std::ptr::eq(a, b));
    assert!(std::ptr::eq(b, c));
}

#[test]
fn distinct_providers_have_distinct_singletons() {
    // anthropic and openai resolve to different adapters.
    let anthropic = get_adapter("anthropic").expect("anthropic");
    let openai = get_adapter("openai").expect("openai");
    assert!(
        !std::ptr::eq(anthropic, openai),
        "anthropic and openai MUST be distinct adapters"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section I — UnknownProvider error shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_provider_returns_error_with_offending_name() {
    let outcome = get_adapter("not_a_real_provider_xyz_156");
    let Err(err) = outcome else {
        panic!("expected Err")
    };
    assert!(matches!(err, ProviderError::UnknownProvider { .. }));
    let ProviderError::UnknownProvider { name, .. } = &err else {
        panic!("MUST be UnknownProvider");
    };
    assert_eq!(name, "not_a_real_provider_xyz_156");
}

#[test]
fn unknown_provider_error_carries_documented_supported_list() {
    let outcome = get_adapter("xyz_bogus");
    let Err(err) = outcome else {
        panic!("expected Err")
    };
    let ProviderError::UnknownProvider { supported, .. } = &err else {
        panic!("MUST be UnknownProvider");
    };
    // PINS DOC: supported list contains all canonical names + aliases.
    for name in &[
        "anthropic",
        "openai",
        "google",
        "gemini",
        "deepseek",
        "qwen",
        "alibaba",
        "zai",
        "glm",
        "zhipu",
        "kimi",
        "moonshot",
        "minimax",
        "ollama",
        "local",
        "lmstudio",
        "localai",
        "text-generation-webui",
        "openrouter",
        "opencode",
        "opencode-go",
        "openai-compatible",
    ] {
        assert!(
            supported.iter().any(|s| s == name),
            "supported MUST include {name:?}; got {supported:?}"
        );
    }
}

#[test]
fn unknown_provider_supported_list_has_exactly_22_entries() {
    // PINS COUNT: documented names accepted by runtime provider dispatch.
    let outcome = get_adapter("xyz");
    let Err(err) = outcome else {
        panic!("expected Err")
    };
    let ProviderError::UnknownProvider { supported, .. } = &err else {
        panic!("MUST be UnknownProvider");
    };
    assert_eq!(
        supported.len(),
        22,
        "MUST list exactly 22 supported names; got {supported:?}"
    );
}

#[test]
fn empty_provider_name_returns_unknown_provider_error() {
    let outcome = get_adapter("");
    let Err(err) = outcome else {
        panic!("expected Err")
    };
    assert!(matches!(err, ProviderError::UnknownProvider { .. }));
}

#[test]
fn whitespace_only_provider_name_returns_unknown_provider_error() {
    let outcome = get_adapter("   ");
    let Err(err) = outcome else {
        panic!("expected Err")
    };
    assert!(matches!(err, ProviderError::UnknownProvider { .. }));
}

// ───────────────────────────────────────────────────────────────────────────
// Section J — Cross-canonical-vs-alias adapter identity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_alias_resolves_to_a_canonical_adapter_singleton() {
    // PINS TABLE: every documented alias resolves to the
    // same singleton as its canonical.
    let pairs = &[
        ("google", "gemini"),
        ("zai", "glm"),
        ("zai", "zhipu"),
        ("qwen", "alibaba"),
        ("kimi", "moonshot"),
        ("openai", "local"),
        ("openai", "lmstudio"),
        ("openai", "localai"),
        ("openai", "text-generation-webui"),
    ];
    for (canonical, alias) in pairs {
        let c = get_adapter(canonical).expect(canonical);
        let a = get_adapter(alias).expect(alias);
        assert!(
            std::ptr::eq(c, a),
            "alias {alias:?} MUST share singleton with {canonical:?}"
        );
    }
}
