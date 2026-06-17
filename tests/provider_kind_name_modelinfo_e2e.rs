//! End-to-end tests for `providers::ProviderKind::name` const
//! accessor + `ModelInfo` serde shape + `ProviderError`
//! `UnknownProvider` Display formatting.
//!
//! Sprint 106 of the verification effort. Sprint 72
//! (`provider_dispatch_apikey_e2e`) covered `get_adapter`
//! dispatch + `ProviderKind::from_model` classification; this
//! file pins the `ProviderKind::name()` accessor (the
//! authoritative "name as static str" used to key into
//! `AppConfig.providers`) and the `ModelInfo` /
//! `ProviderError` wire-shape contracts.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::providers::{ModelInfo, ProviderError, ProviderKind};

// ───────────────────────────────────────────────────────────────────────────
// Section A — ProviderKind::name accessor
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn provider_kind_name_anthropic() {
    assert_eq!(ProviderKind::Anthropic.name(), "anthropic");
}

#[test]
fn provider_kind_name_openai() {
    assert_eq!(ProviderKind::OpenAI.name(), "openai");
}

#[test]
fn provider_kind_name_google() {
    assert_eq!(ProviderKind::Google.name(), "google");
}

#[test]
fn provider_kind_name_deepseek() {
    assert_eq!(ProviderKind::DeepSeek.name(), "deepseek");
}

#[test]
fn provider_kind_name_qwen() {
    assert_eq!(ProviderKind::Qwen.name(), "qwen");
}

#[test]
fn provider_kind_name_zai() {
    assert_eq!(ProviderKind::Zai.name(), "zai");
}

#[test]
fn provider_kind_name_kimi() {
    assert_eq!(ProviderKind::Kimi.name(), "kimi");
}

#[test]
fn provider_kind_name_minimax() {
    assert_eq!(ProviderKind::MiniMax.name(), "minimax");
}

#[test]
fn provider_kind_name_unknown_returns_unknown_string() {
    assert_eq!(ProviderKind::Unknown.name(), "unknown");
}

#[test]
fn provider_kind_name_strings_are_pairwise_distinct() {
    let variants = [
        ProviderKind::Anthropic,
        ProviderKind::OpenAI,
        ProviderKind::Google,
        ProviderKind::DeepSeek,
        ProviderKind::Qwen,
        ProviderKind::Zai,
        ProviderKind::Kimi,
        ProviderKind::MiniMax,
        ProviderKind::Unknown,
    ];
    let mut names: Vec<&'static str> = variants.iter().map(ProviderKind::name).collect();
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        n,
        "name() MUST be 1:1 with variants; got {} unique of {}",
        names.len(),
        n
    );
}

#[test]
fn provider_kind_name_returns_lowercase_ascii() {
    // Documented contract: lowercase ASCII used as
    // AppConfig.providers key + get_adapter selector.
    for variant in &[
        ProviderKind::Anthropic,
        ProviderKind::OpenAI,
        ProviderKind::Google,
        ProviderKind::DeepSeek,
        ProviderKind::Qwen,
        ProviderKind::Zai,
        ProviderKind::Kimi,
        ProviderKind::MiniMax,
        ProviderKind::Unknown,
    ] {
        let name = variant.name();
        assert!(
            name.chars().all(|c| c.is_ascii_lowercase()),
            "name() MUST be lowercase ASCII; got {name:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — ProviderKind round-trip from_model → name
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn classify_then_name_round_trips_for_anthropic_model() {
    let kind = ProviderKind::from_model("claude-3-5-sonnet");
    assert_eq!(kind.name(), "anthropic");
}

#[test]
fn classify_then_name_round_trips_for_each_provider() {
    let cases = [
        ("claude-3-5-sonnet", "anthropic"),
        ("gpt-4o", "openai"),
        ("o1-preview", "openai"),
        ("gemini-2.0-flash", "google"),
        ("deepseek-coder-v2", "deepseek"),
        ("qwen2.5-coder-32b", "qwen"),
        ("qwq-32b", "qwen"),
        ("qvq-72b", "qwen"),
        ("glm-4-flash", "zai"),
        ("kimi-k2.7-code", "kimi"),
        ("moonshot-v1-128k", "kimi"),
        ("MiniMax-M3", "minimax"),
    ];
    for (model, expected_name) in &cases {
        let kind = ProviderKind::from_model(model);
        assert_eq!(
            kind.name(),
            *expected_name,
            "model {model:?} MUST classify to {expected_name:?}"
        );
    }
}

#[test]
fn classify_then_name_returns_unknown_for_unknown_models() {
    for model in &["totally-random-model", "", "xyz-9000"] {
        let kind = ProviderKind::from_model(model);
        assert_eq!(kind.name(), "unknown");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — ProviderKind Copy + Hash + Eq
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn provider_kind_is_copy() {
    let k = ProviderKind::Anthropic;
    let copy = k;
    let again = k;
    assert_eq!(copy, again);
}

#[test]
fn provider_kind_equality_via_partial_eq() {
    assert_eq!(ProviderKind::Anthropic, ProviderKind::Anthropic);
    assert_ne!(ProviderKind::Anthropic, ProviderKind::OpenAI);
}

#[test]
fn provider_kind_is_hashable() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(ProviderKind::Anthropic);
    set.insert(ProviderKind::Anthropic);
    set.insert(ProviderKind::OpenAI);
    assert_eq!(set.len(), 2, "HashSet MUST dedupe by Hash + Eq");
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — ModelInfo serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn model_info_serializes_with_id_field() {
    let info = ModelInfo {
        id: "test-model".to_string(),
        owned_by: None,
        created: None,
    };
    let json = serde_json::to_string(&info).expect("ser");
    assert!(json.contains("\"id\":\"test-model\""));
}

#[test]
fn model_info_deserializes_with_only_id_field() {
    let json = r#"{"id": "minimal"}"#;
    let info: ModelInfo = serde_json::from_str(json).expect("de");
    assert_eq!(info.id, "minimal");
    assert!(info.owned_by.is_none());
    assert!(info.created.is_none());
}

#[test]
fn model_info_round_trips_full_shape() {
    let original = ModelInfo {
        id: "claude-3-5-sonnet-20250929".to_string(),
        owned_by: Some("anthropic".to_string()),
        created: Some(1_700_000_000),
    };
    let json = serde_json::to_string(&original).expect("ser");
    let back: ModelInfo = serde_json::from_str(&json).expect("de");
    assert_eq!(back.id, original.id);
    assert_eq!(back.owned_by, original.owned_by);
    assert_eq!(back.created, original.created);
}

#[test]
fn model_info_clone_preserves_all_fields() {
    let original = ModelInfo {
        id: "x".to_string(),
        owned_by: Some("y".to_string()),
        created: Some(42),
    };
    let cloned = original.clone();
    assert_eq!(cloned.id, original.id);
    assert_eq!(cloned.owned_by, original.owned_by);
    assert_eq!(cloned.created, original.created);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — ProviderError Display formatting
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn provider_error_request_failed_carries_reason_in_display() {
    let err = ProviderError::RequestFailed("connection reset".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Request failed"));
    assert!(msg.contains("connection reset"));
}

#[test]
fn provider_error_invalid_response_carries_reason_in_display() {
    let err = ProviderError::InvalidResponse("malformed JSON".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Invalid response"));
    assert!(msg.contains("malformed JSON"));
}

#[test]
fn provider_error_unsupported_carries_reason_in_display() {
    let err = ProviderError::Unsupported("model_listing".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Unsupported"));
    assert!(msg.contains("model_listing"));
}

#[test]
fn provider_error_unknown_provider_includes_name_and_supported_list() {
    let err = ProviderError::UnknownProvider {
        name: "anthrpic".to_string(),
        supported: vec!["anthropic", "openai", "google"],
    };
    let msg = err.to_string();
    // PINS CONTRACT: error MUST mention the bad name AND the
    // supported names so users can self-correct.
    assert!(msg.contains("anthrpic"));
    assert!(msg.contains("anthropic"));
    assert!(msg.contains("openai"));
    assert!(msg.contains("google"));
}

#[test]
fn provider_error_unknown_provider_supported_list_uses_comma_separation() {
    let err = ProviderError::UnknownProvider {
        name: "bad".to_string(),
        supported: vec!["alpha", "beta", "gamma"],
    };
    let msg = err.to_string();
    // Display uses ", " join.
    assert!(msg.contains("alpha, beta, gamma"));
}
