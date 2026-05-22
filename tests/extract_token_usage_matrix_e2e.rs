//! End-to-end tests for `ProviderAdapter::extract_token_usage`
//! across providers — per-provider envelope field names
//! (Anthropic `input_tokens`/`output_tokens`/cache_*, Google
//! `usageMetadata.promptTokenCount`, Ollama
//! `prompt_eval_count`/`eval_count`).
//!
//! Sprint 163 of the verification effort. Sprint 17
//! covered the `OpenAI` shape (`usage.prompt_tokens` /
//! `completion_tokens`); this file pins the per-provider
//! distinct shapes + the Ollama require-at-least-one-counter
//! invariant + cache-field mapping.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::providers::get_adapter;
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Anthropic input/output + cache_*_input_tokens
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_extracts_input_and_output_tokens_from_usage_block() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {
            "input_tokens": 100,
            "output_tokens": 50
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 0);
}

#[test]
fn anthropic_extracts_cache_read_input_tokens_field() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 200
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.cache_read_tokens, 200);
    assert_eq!(usage.cache_write_tokens, 0);
}

#[test]
fn anthropic_extracts_cache_creation_input_tokens_field() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_creation_input_tokens": 300
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.cache_write_tokens, 300);
    assert_eq!(usage.cache_read_tokens, 0);
}

#[test]
fn anthropic_missing_usage_returns_none() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({"other_field": "x"});
    assert!(adapter.extract_token_usage(&response).is_none());
}

#[test]
fn anthropic_usage_present_but_all_counters_missing_returns_zeros_not_none() {
    // PINS DOC: if `usage` key exists, return Some with zeros
    // (NOT None). Different from Ollama's at-least-one rule.
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({"usage": {}});
    let usage = adapter
        .extract_token_usage(&response)
        .expect("Some on empty usage");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
}

#[test]
fn anthropic_non_numeric_token_count_treated_as_zero() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {"input_tokens": "not a number", "output_tokens": 50}
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 50);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Google usageMetadata + camelCase counters
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn google_extracts_prompt_and_candidates_token_count_camel_case() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "usageMetadata": {
            "promptTokenCount": 75,
            "candidatesTokenCount": 25
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 75);
    assert_eq!(usage.output_tokens, 25);
}

#[test]
fn google_extracts_cached_content_token_count_into_cache_read() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "usageMetadata": {
            "promptTokenCount": 50,
            "candidatesTokenCount": 30,
            "cachedContentTokenCount": 1000
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.cache_read_tokens, 1000);
    // PINS DOC: Google has NO cache_write counter — always 0.
    assert_eq!(usage.cache_write_tokens, 0);
}

#[test]
fn google_missing_usage_metadata_returns_none() {
    let adapter = get_adapter("google").unwrap();
    let response = json!({"other": "x"});
    assert!(adapter.extract_token_usage(&response).is_none());
}

#[test]
fn google_rejects_anthropic_field_names_within_usage_metadata() {
    // PINS DISTINCTNESS: Google uses promptTokenCount, NOT
    // input_tokens. If only Anthropic-style fields are present
    // inside usageMetadata, the counters are 0 (not borrowed).
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "usageMetadata": {
            "input_tokens": 999,
            "output_tokens": 888
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
}

#[test]
fn google_usage_metadata_at_anthropic_path_returns_none() {
    // PINS PATH: Google looks at "usageMetadata", NOT "usage".
    let adapter = get_adapter("google").unwrap();
    let response = json!({
        "usage": {"promptTokenCount": 100, "candidatesTokenCount": 50}
    });
    assert!(
        adapter.extract_token_usage(&response).is_none(),
        "Google MUST require usageMetadata key, not usage"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Ollama prompt_eval_count + eval_count + require-1 invariant
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn ollama_extracts_top_level_prompt_eval_and_eval_count() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({
        "prompt_eval_count": 200,
        "eval_count": 100
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.output_tokens, 100);
}

#[test]
fn ollama_with_only_prompt_eval_count_returns_some_with_zero_output() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"prompt_eval_count": 50});
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 50);
    assert_eq!(usage.output_tokens, 0);
}

#[test]
fn ollama_with_only_eval_count_returns_some_with_zero_input() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"eval_count": 80});
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 80);
}

#[test]
fn ollama_with_no_counters_at_all_returns_none() {
    // PINS DOC: Ollama's require-at-least-one-counter rule.
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"other_field": "x"});
    assert!(
        adapter.extract_token_usage(&response).is_none(),
        "Ollama MUST return None when neither counter present"
    );
}

#[test]
fn ollama_has_no_cache_counters() {
    let adapter = get_adapter("ollama").unwrap();
    let response = json!({"prompt_eval_count": 1, "eval_count": 1});
    let usage = adapter.extract_token_usage(&response).expect("Some");
    // PINS DOC: Ollama has no cache layer.
    assert_eq!(usage.cache_read_tokens, 0);
    assert_eq!(usage.cache_write_tokens, 0);
}

#[test]
fn ollama_non_numeric_counter_treated_as_absent() {
    let adapter = get_adapter("ollama").unwrap();
    // String values fail as_u64 → counted as absent. With both
    // absent, return None.
    let response = json!({
        "prompt_eval_count": "not_a_number",
        "eval_count": "neither"
    });
    assert!(adapter.extract_token_usage(&response).is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Cross-provider isolation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn each_provider_returns_none_for_other_providers_usage_shape() {
    let anth_resp = json!({"usage": {"input_tokens": 1, "output_tokens": 1}});
    let google_resp = json!({"usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}});
    let ollama_resp = json!({"prompt_eval_count": 1, "eval_count": 1});

    let anth = get_adapter("anthropic").unwrap();
    let google = get_adapter("google").unwrap();
    let ollama = get_adapter("ollama").unwrap();

    // Cross-mismatches → None.
    assert!(google.extract_token_usage(&anth_resp).is_none());
    assert!(google.extract_token_usage(&ollama_resp).is_none());
    assert!(ollama.extract_token_usage(&anth_resp).is_none());
    assert!(ollama.extract_token_usage(&google_resp).is_none());
    assert!(anth.extract_token_usage(&google_resp).is_none());
    assert!(anth.extract_token_usage(&ollama_resp).is_none());
}

#[test]
fn each_provider_returns_none_on_empty_object() {
    let empty = json!({});
    for name in &["anthropic", "google", "ollama"] {
        let adapter = get_adapter(name).unwrap();
        assert!(
            adapter.extract_token_usage(&empty).is_none(),
            "{name} MUST return None on empty object"
        );
    }
}

#[test]
fn each_provider_returns_none_on_json_null() {
    let nul = serde_json::Value::Null;
    for name in &["anthropic", "google", "ollama"] {
        let adapter = get_adapter(name).unwrap();
        assert!(
            adapter.extract_token_usage(&nul).is_none(),
            "{name} MUST return None on null"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Number coercion edge cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extreme_token_counts_preserved_via_u64() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {
            "input_tokens": u64::MAX,
            "output_tokens": u64::MAX
        }
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    assert_eq!(usage.input_tokens, u64::MAX);
    assert_eq!(usage.output_tokens, u64::MAX);
}

#[test]
fn negative_token_count_via_serde_signed_falls_back_to_zero() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {"input_tokens": -10, "output_tokens": -5}
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    // Value::as_u64 returns None for negative → defaults to 0.
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
}

#[test]
fn float_token_count_treated_as_zero_via_as_u64() {
    let adapter = get_adapter("anthropic").unwrap();
    let response = json!({
        "usage": {"input_tokens": 100.5, "output_tokens": 50.5}
    });
    let usage = adapter.extract_token_usage(&response).expect("Some");
    // as_u64 returns None for non-integers → zero.
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
}
