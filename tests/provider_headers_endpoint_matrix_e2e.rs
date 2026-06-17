//! End-to-end tests for every `ProviderAdapter`'s
//! `chat_endpoint` + `get_headers` + `supports_model_listing`
//! per-provider differences.
//!
//! Sprint 161 of the verification effort. Sprint 119
//! covered `pipeline::resolve_*` aggregate behavior;
//! this file pins each adapter's per-provider header set
//! plus endpoint shape so a future OpenAI-compatible
//! delegation refactor can't silently change the wire.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::providers::{get_adapter, ApiKey};

fn key() -> ApiKey {
    ApiKey::try_from_string("sk-test-key-160".to_string()).expect("valid")
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — Anthropic
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_chat_endpoint_is_v1_messages_regardless_of_model() {
    let adapter = get_adapter("anthropic").expect("anthropic");
    assert_eq!(adapter.chat_endpoint("claude-sonnet-4-5"), "/v1/messages");
    assert_eq!(adapter.chat_endpoint("claude-opus-4"), "/v1/messages");
    assert_eq!(adapter.chat_endpoint("any-model"), "/v1/messages");
}

#[test]
fn anthropic_headers_use_xapikey_not_authorization() {
    let adapter = get_adapter("anthropic").expect("anthropic");
    let api_key = key();
    let headers = adapter.get_headers(&api_key);
    // PINS WIRE: Anthropic uses x-api-key, NOT Authorization Bearer.
    assert!(header(&headers, "x-api-key").is_some());
    assert!(header(&headers, "authorization").is_none());
}

#[test]
fn anthropic_headers_include_anthropic_version_2023_06_01() {
    let adapter = get_adapter("anthropic").expect("anthropic");
    let headers = adapter.get_headers(&key());
    assert_eq!(
        header(&headers, "anthropic-version").as_deref(),
        Some("2023-06-01")
    );
}

#[test]
fn anthropic_headers_include_content_type_application_json() {
    let adapter = get_adapter("anthropic").expect("anthropic");
    let headers = adapter.get_headers(&key());
    assert_eq!(
        header(&headers, "content-type").as_deref(),
        Some("application/json")
    );
}

#[test]
fn anthropic_headers_carry_exact_api_key_value() {
    let adapter = get_adapter("anthropic").expect("anthropic");
    let headers = adapter.get_headers(&key());
    assert_eq!(
        header(&headers, "x-api-key").as_deref(),
        Some("sk-test-key-160")
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — OpenAI (canonical OpenAI-compat shape)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn openai_chat_endpoint_is_v1_chat_completions() {
    let adapter = get_adapter("openai").expect("openai");
    assert_eq!(adapter.chat_endpoint("gpt-4o"), "/v1/chat/completions");
}

#[test]
fn openai_headers_use_authorization_bearer() {
    let adapter = get_adapter("openai").expect("openai");
    let headers = adapter.get_headers(&key());
    let auth = header(&headers, "authorization").expect("Authorization");
    assert!(
        auth.starts_with("Bearer "),
        "Authorization MUST be Bearer-prefixed; got {auth:?}"
    );
    assert!(auth.ends_with("sk-test-key-160"));
}

#[test]
fn openai_headers_do_not_use_xapikey() {
    let adapter = get_adapter("openai").expect("openai");
    let headers = adapter.get_headers(&key());
    assert!(
        header(&headers, "x-api-key").is_none(),
        "openai MUST NOT use x-api-key"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Google (Gemini)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn google_chat_endpoint_embeds_model_name_in_path() {
    let adapter = get_adapter("google").expect("google");
    let ep = adapter.chat_endpoint("gemini-2.5-pro");
    // PINS WIRE: /v1beta/models/<MODEL>:generateContent
    assert_eq!(ep, "/v1beta/models/gemini-2.5-pro:generateContent");
}

#[test]
fn google_chat_endpoint_differs_per_model() {
    let adapter = get_adapter("google").expect("google");
    let a = adapter.chat_endpoint("gemini-2.5-pro");
    let b = adapter.chat_endpoint("gemini-2.5-flash");
    assert_ne!(a, b, "Google endpoint MUST vary by model");
    assert!(a.contains("gemini-2.5-pro"));
    assert!(b.contains("gemini-2.5-flash"));
}

#[test]
fn google_headers_use_x_goog_api_key_not_authorization() {
    let adapter = get_adapter("google").expect("google");
    let headers = adapter.get_headers(&key());
    // PINS WIRE: Google uses x-goog-api-key (NOT Bearer).
    assert!(header(&headers, "x-goog-api-key").is_some());
    assert!(header(&headers, "authorization").is_none());
    assert!(header(&headers, "x-api-key").is_none());
}

#[test]
fn google_x_goog_api_key_carries_exact_value() {
    let adapter = get_adapter("google").expect("google");
    let headers = adapter.get_headers(&key());
    assert_eq!(
        header(&headers, "x-goog-api-key").as_deref(),
        Some("sk-test-key-160")
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Ollama (no auth)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn ollama_chat_endpoint_is_api_chat() {
    let adapter = get_adapter("ollama").expect("ollama");
    assert_eq!(adapter.chat_endpoint("llama3"), "/api/chat");
}

#[test]
fn ollama_headers_have_no_auth_header() {
    let adapter = get_adapter("ollama").expect("ollama");
    let headers = adapter.get_headers(&key());
    // PINS DOC: Ollama doesn't require auth by default.
    assert!(header(&headers, "authorization").is_none());
    assert!(header(&headers, "x-api-key").is_none());
    assert!(header(&headers, "x-goog-api-key").is_none());
}

#[test]
fn ollama_headers_include_only_content_type() {
    let adapter = get_adapter("ollama").expect("ollama");
    let headers = adapter.get_headers(&key());
    assert_eq!(
        header(&headers, "content-type").as_deref(),
        Some("application/json")
    );
    // Only content-type header.
    assert_eq!(headers.len(), 1);
}

#[test]
fn ollama_ignores_api_key_value() {
    let adapter = get_adapter("ollama").expect("ollama");
    // Different keys produce identical headers (key ignored).
    let h1 = adapter.get_headers(&key());
    let h2 =
        adapter.get_headers(&ApiKey::try_from_string("different-key-xyz".to_string()).expect("ok"));
    assert_eq!(h1, h2, "Ollama MUST ignore api_key value");
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — OpenAI-compatible adapters delegate to OpenAI shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn deepseek_chat_endpoint_matches_openai() {
    let openai = get_adapter("openai").expect("openai");
    let deepseek = get_adapter("deepseek").expect("deepseek");
    assert_eq!(
        deepseek.chat_endpoint("deepseek-chat"),
        openai.chat_endpoint("deepseek-chat")
    );
}

#[test]
fn qwen_chat_endpoint_matches_openai() {
    let openai = get_adapter("openai").expect("openai");
    let qwen = get_adapter("qwen").expect("qwen");
    assert_eq!(
        qwen.chat_endpoint("qwen-max"),
        openai.chat_endpoint("qwen-max")
    );
}

#[test]
fn kimi_chat_endpoint_matches_openai() {
    let openai = get_adapter("openai").expect("openai");
    let kimi = get_adapter("kimi").expect("kimi");
    assert_eq!(
        kimi.chat_endpoint("kimi-k2.7-code"),
        openai.chat_endpoint("kimi-k2.7-code")
    );
}

#[test]
fn minimax_chat_endpoint_matches_openai() {
    let openai = get_adapter("openai").expect("openai");
    let minimax = get_adapter("minimax").expect("minimax");
    assert_eq!(
        minimax.chat_endpoint("MiniMax-M3"),
        openai.chat_endpoint("MiniMax-M3")
    );
}

#[test]
fn zai_chat_endpoint_uses_no_v1_prefix_distinct_from_openai() {
    // AUTHORING DISCOVERY: Z.AI (BigModel) hosts at
    // `/chat/completions` WITHOUT the `/v1/` prefix that
    // OpenAI/DeepSeek/Qwen use. So zai's endpoint does NOT
    // match openai byte-for-byte even though both delegate
    // through OpenAiCompatibleAdapter.
    let zai = get_adapter("zai").expect("zai");
    let openai = get_adapter("openai").expect("openai");
    assert_eq!(zai.chat_endpoint("glm-4"), "/chat/completions");
    assert_eq!(openai.chat_endpoint("any"), "/v1/chat/completions");
    assert_ne!(
        zai.chat_endpoint("glm-4"),
        openai.chat_endpoint("any"),
        "Z.AI MUST differ from OpenAI's /v1/ prefix"
    );
}

#[test]
fn deepseek_headers_use_bearer_like_openai() {
    let adapter = get_adapter("deepseek").expect("deepseek");
    let headers = adapter.get_headers(&key());
    let auth = header(&headers, "authorization").expect("auth");
    assert!(auth.starts_with("Bearer "));
}

#[test]
fn qwen_headers_use_bearer_like_openai() {
    let adapter = get_adapter("qwen").expect("qwen");
    let headers = adapter.get_headers(&key());
    let auth = header(&headers, "authorization").expect("auth");
    assert!(auth.starts_with("Bearer "));
}

#[test]
fn zai_headers_use_bearer_like_openai() {
    let adapter = get_adapter("zai").expect("zai");
    let headers = adapter.get_headers(&key());
    let auth = header(&headers, "authorization").expect("auth");
    assert!(auth.starts_with("Bearer "));
}

#[test]
fn kimi_headers_use_bearer_like_openai() {
    let adapter = get_adapter("kimi").expect("kimi");
    let headers = adapter.get_headers(&key());
    let auth = header(&headers, "authorization").expect("auth");
    assert!(auth.starts_with("Bearer "));
}

#[test]
fn minimax_headers_use_bearer_like_openai() {
    let adapter = get_adapter("minimax").expect("minimax");
    let headers = adapter.get_headers(&key());
    let auth = header(&headers, "authorization").expect("auth");
    assert!(auth.starts_with("Bearer "));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — supports_model_listing
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn ollama_supports_model_listing() {
    let adapter = get_adapter("ollama").expect("ollama");
    assert!(
        adapter.supports_model_listing(),
        "Ollama MUST support /v1/models"
    );
}

#[test]
fn openai_supports_model_listing() {
    let adapter = get_adapter("openai").expect("openai");
    assert!(
        adapter.supports_model_listing(),
        "OpenAI MUST support /v1/models"
    );
}

#[test]
fn kimi_supports_model_listing() {
    let adapter = get_adapter("kimi").expect("kimi");
    assert!(
        adapter.supports_model_listing(),
        "Kimi MUST support OpenAI-style /v1/models"
    );
}

#[test]
fn anthropic_does_not_support_model_listing() {
    // PINS DOC: Anthropic doesn't expose a /v1/models endpoint.
    let adapter = get_adapter("anthropic").expect("anthropic");
    assert!(
        !adapter.supports_model_listing(),
        "Anthropic MUST NOT advertise model listing"
    );
}

#[test]
fn google_does_not_support_openai_style_model_listing() {
    // PINS DOC: Google has its own list-models endpoint, NOT the
    // OpenAI-compatible /v1/models. The adapter declares no support
    // so fetch_models doesn't attempt it via that path.
    let adapter = get_adapter("google").expect("google");
    assert!(
        !adapter.supports_model_listing(),
        "Google MUST NOT advertise OpenAI-style model listing"
    );
}

#[test]
fn minimax_does_not_advertise_openai_style_model_listing() {
    let adapter = get_adapter("minimax").expect("minimax");
    assert!(
        !adapter.supports_model_listing(),
        "MiniMax MUST NOT advertise model listing until its response shape is parsed"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Cross-provider distinctness
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_and_openai_have_distinct_endpoint_shapes() {
    let anth = get_adapter("anthropic").unwrap();
    let openai = get_adapter("openai").unwrap();
    assert_ne!(
        anth.chat_endpoint("any"),
        openai.chat_endpoint("any"),
        "anthropic /v1/messages MUST differ from openai /v1/chat/completions"
    );
}

#[test]
fn anthropic_google_openai_use_distinct_auth_header_names() {
    let anth = get_adapter("anthropic").unwrap().get_headers(&key());
    let google = get_adapter("google").unwrap().get_headers(&key());
    let openai = get_adapter("openai").unwrap().get_headers(&key());
    assert!(header(&anth, "x-api-key").is_some());
    assert!(header(&google, "x-goog-api-key").is_some());
    assert!(header(&openai, "authorization").is_some());
    // Cross-checks: each set lacks the others' auth header.
    assert!(header(&anth, "x-goog-api-key").is_none());
    assert!(header(&google, "x-api-key").is_none());
    assert!(header(&openai, "x-goog-api-key").is_none());
}
