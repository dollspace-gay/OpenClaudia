//! End-to-end tests for `proxy::ProxyError` HTTP-status mapping +
//! response body shape + `ContentPart` serde details.
//!
//! Sprint 85 of the verification effort. Sprint 25
//! (`proxy_translation_e2e`) covered `normalize_base_url`,
//! `determine_provider`, `extract_usage_from_sse_event`, and
//! `ChatMessage` round-trips; this file pins
//! the `ProxyError::into_response` status-code matrix and
//! body envelope shape that downstream HTTP clients depend
//! on, plus the `ContentPart` per-field serde semantics.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
use openclaudia::proxy::{
    ChatMessage, ContentPart, MessageContent, ProxyError, MAX_SSE_LINE_BYTES,
    SSE_STREAM_TIMEOUT_SECS,
};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

async fn extract_status_and_body(err: ProxyError) -> (StatusCode, serde_json::Value) {
    let resp = err.into_response();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("body MUST be valid JSON");
    (status, json)
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — ProxyError → StatusCode mapping
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_api_key_maps_to_401_unauthorized() {
    let err = ProxyError::NoApiKey("anthropic".to_string());
    let (status, _body) = extract_status_and_body(err).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn hook_blocked_maps_to_403_forbidden() {
    let err = ProxyError::HookBlocked("policy violation".to_string());
    let (status, _body) = extract_status_and_body(err).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn policy_denied_maps_to_403_forbidden() {
    let err = ProxyError::PolicyDenied("model is not allowed".to_string());
    let (status, _body) = extract_status_and_body(err).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn provider_not_configured_maps_to_400_bad_request() {
    let err = ProxyError::ProviderNotConfigured("unknown".to_string());
    let (status, _body) = extract_status_and_body(err).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_body_maps_to_400_bad_request() {
    let err = ProxyError::InvalidBody("malformed JSON".to_string());
    let (status, _body) = extract_status_and_body(err).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn json_error_maps_to_400_bad_request() {
    // Construct a serde_json::Error via failed parse.
    let parse_err = serde_json::from_str::<serde_json::Value>("bad").unwrap_err();
    let err = ProxyError::JsonError(parse_err);
    let (status, _body) = extract_status_and_body(err).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Response body envelope shape
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn response_body_contains_error_object_with_message_and_type() {
    let err = ProxyError::NoApiKey("openai".to_string());
    let (_status, body) = extract_status_and_body(err).await;
    let error_obj = body.get("error").expect("error envelope MUST be present");
    assert!(error_obj.get("message").is_some());
    assert!(error_obj.get("type").is_some());
}

#[tokio::test]
async fn response_body_type_field_is_proxy_error_tag() {
    let err = ProxyError::InvalidBody("x".to_string());
    let (_status, body) = extract_status_and_body(err).await;
    assert_eq!(body["error"]["type"], "proxy_error");
}

#[tokio::test]
async fn response_body_message_includes_error_display_text() {
    let err = ProxyError::NoApiKey("anthropic".to_string());
    let (_status, body) = extract_status_and_body(err).await;
    let msg = body["error"]["message"]
        .as_str()
        .expect("message MUST be string");
    assert!(
        msg.contains("anthropic"),
        "message MUST surface the provider name; got {msg:?}"
    );
    assert!(msg.contains("No API key"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — ContentPart serde details
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn content_part_type_field_renamed_to_type_in_json() {
    let part = ContentPart {
        content_type: "text".to_string(),
        text: Some("hello".to_string()),
        image_url: None,
    };
    let json = serde_json::to_string(&part).expect("ser");
    // Documented serde rename: content_type → "type".
    assert!(
        json.contains("\"type\":\"text\""),
        "MUST use 'type' (not 'content_type') on wire; got {json:?}"
    );
    assert!(
        !json.contains("content_type"),
        "MUST NOT carry content_type field name on wire; got {json:?}"
    );
}

#[test]
fn content_part_text_part_round_trips() {
    let part = ContentPart {
        content_type: "text".to_string(),
        text: Some("hello world".to_string()),
        image_url: None,
    };
    let json = serde_json::to_string(&part).expect("ser");
    let back: ContentPart = serde_json::from_str(&json).expect("de");
    assert_eq!(back.content_type, "text");
    assert_eq!(back.text.as_deref(), Some("hello world"));
    assert!(back.image_url.is_none());
}

#[test]
fn content_part_image_url_part_round_trips() {
    let img = json!({"url": "data:image/png;base64,iVBOR..."});
    let part = ContentPart {
        content_type: "image_url".to_string(),
        text: None,
        image_url: Some(img.clone()),
    };
    let json_str = serde_json::to_string(&part).expect("ser");
    let back: ContentPart = serde_json::from_str(&json_str).expect("de");
    assert_eq!(back.content_type, "image_url");
    assert!(back.text.is_none());
    assert_eq!(back.image_url, Some(img));
}

#[test]
fn content_part_omits_none_fields_via_skip_serializing_if() {
    // Use a content_type that doesn't appear as a substring
    // in field names so the skip-check is unambiguous.
    let part = ContentPart {
        content_type: "marker".to_string(),
        text: None,
        image_url: None,
    };
    let json = serde_json::to_string(&part).expect("ser");
    // Field-name shaped: "text":  / "image_url":
    assert!(
        !json.contains("\"text\""),
        "None text MUST be skipped (no \"text\" field name); got {json:?}"
    );
    assert!(
        !json.contains("\"image_url\""),
        "None image_url MUST be skipped (no \"image_url\" field name); got {json:?}"
    );
    // The required type field IS present.
    assert!(json.contains("\"type\":\"marker\""));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — MessageContent untagged enum
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn message_content_text_deserializes_from_bare_string() {
    let json = r#""hello""#;
    let parsed: MessageContent = serde_json::from_str(json).expect("de");
    let MessageContent::Text(t) = parsed else {
        panic!("MUST parse as Text variant");
    };
    assert_eq!(t, "hello");
}

#[test]
fn message_content_parts_deserializes_from_array() {
    let json = r#"[
        {"type": "text", "text": "hello"},
        {"type": "image_url", "image_url": {"url": "https://x"}}
    ]"#;
    let parsed: MessageContent = serde_json::from_str(json).expect("de");
    let MessageContent::Parts(parts) = parsed else {
        panic!("MUST parse as Parts variant");
    };
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].content_type, "text");
    assert_eq!(parts[1].content_type, "image_url");
}

#[test]
fn message_content_serializes_text_as_bare_string() {
    let content = MessageContent::Text("plain".to_string());
    let json = serde_json::to_string(&content).expect("ser");
    assert_eq!(
        json, "\"plain\"",
        "untagged Text MUST serialize as bare string"
    );
}

#[test]
fn message_content_serializes_parts_as_array() {
    let content = MessageContent::Parts(vec![ContentPart {
        content_type: "text".to_string(),
        text: Some("a".to_string()),
        image_url: None,
    }]);
    let json = serde_json::to_string(&content).expect("ser");
    assert!(json.starts_with('['));
    assert!(json.ends_with(']'));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — ChatMessage serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn chat_message_omits_optional_fields_when_none() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Text("hi".to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    let json = serde_json::to_string(&msg).expect("ser");
    assert!(!json.contains("\"name\""));
    assert!(!json.contains("\"tool_calls\""));
    assert!(!json.contains("\"tool_call_id\""));
    // Required fields ARE present.
    assert!(json.contains("\"role\":\"user\""));
    assert!(json.contains("\"content\":\"hi\""));
}

#[test]
fn chat_message_includes_optional_fields_when_set() {
    let msg = ChatMessage {
        role: "tool".to_string(),
        content: MessageContent::Text("result".to_string()),
        name: Some("bash".to_string()),
        tool_calls: None,
        tool_call_id: Some("call-123".to_string()),
        extra: std::collections::HashMap::new(),
    };
    let json = serde_json::to_string(&msg).expect("ser");
    assert!(json.contains("\"name\":\"bash\""));
    assert!(json.contains("\"tool_call_id\":\"call-123\""));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Documented SSE constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn sse_stream_timeout_constant_is_5_minutes() {
    assert_eq!(SSE_STREAM_TIMEOUT_SECS, 300);
}

#[test]
fn max_sse_line_bytes_constant_is_1_mib() {
    assert_eq!(MAX_SSE_LINE_BYTES, 1024 * 1024);
}
