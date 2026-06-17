//! End-to-end tests for `proxy::ChatCompletionRequest` and
//! `proxy::ChatMessage` field-level serde — required vs
//! optional fields, skip-None on serialize, and the
//! `#[serde(flatten)]` extra map that captures forward-
//! compat fields.
//!
//! Sprint 158 of the verification effort. Sprint 157
//! pinned the `MessageContent` untagged dispatch; this file
//! pins the surrounding request envelope so the wire-level
//! `ChatCompletion` API contract is verified end-to-end.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
use serde_json::{json, Value};
use std::collections::HashMap;

fn minimal_message() -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Text("hi".to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    }
}

fn minimal_request(model: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![minimal_message()],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — Required fields always present
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn minimal_request_serializes_model_and_messages() {
    let req = minimal_request("claude-sonnet-4-5");
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert_eq!(json["model"], "claude-sonnet-4-5");
    assert!(json["messages"].is_array());
}

#[test]
fn deserialize_requires_model_field() {
    let json = json!({"messages": [{"role": "user", "content": "hi"}]});
    let outcome: Result<ChatCompletionRequest, _> = serde_json::from_value(json);
    assert!(
        outcome.is_err(),
        "missing model MUST be rejected at deserialize time"
    );
}

#[test]
fn deserialize_requires_messages_field() {
    let json = json!({"model": "gpt-4o"});
    let outcome: Result<ChatCompletionRequest, _> = serde_json::from_value(json);
    assert!(
        outcome.is_err(),
        "missing messages MUST be rejected at deserialize time"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Optional fields skip-None on serialize
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn none_temperature_skipped_on_serialize() {
    let req = minimal_request("m");
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!(
        json.get("temperature").is_none(),
        "None temperature MUST be skipped; got {json}"
    );
}

#[test]
fn none_max_tokens_skipped_on_serialize() {
    let req = minimal_request("m");
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!(json.get("max_tokens").is_none());
}

#[test]
fn none_stream_skipped_on_serialize() {
    let req = minimal_request("m");
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!(json.get("stream").is_none());
}

#[test]
fn none_tools_skipped_on_serialize() {
    let req = minimal_request("m");
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!(json.get("tools").is_none());
}

#[test]
fn none_tool_choice_skipped_on_serialize() {
    let req = minimal_request("m");
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!(json.get("tool_choice").is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Set values serialize
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn temperature_some_serializes_as_number() {
    let mut req = minimal_request("m");
    req.temperature = Some(0.7);
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!((json["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
}

#[test]
fn max_tokens_some_serializes_as_integer() {
    let mut req = minimal_request("m");
    req.max_tokens = Some(1024);
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert_eq!(json["max_tokens"], 1024);
}

#[test]
fn stream_some_true_serializes_as_boolean() {
    let mut req = minimal_request("m");
    req.stream = Some(true);
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert_eq!(json["stream"], true);
}

#[test]
fn stream_some_false_distinguishable_from_none() {
    let mut req = minimal_request("m");
    req.stream = Some(false);
    let json: Value = serde_json::to_value(&req).expect("ser");
    // Some(false) MUST be emitted (not skipped — skip_serializing_if
    // is Option::is_none, not is_falsy).
    assert_eq!(
        json["stream"], false,
        "Some(false) MUST serialize; got {json}"
    );
}

#[test]
fn tools_some_serializes_as_array() {
    let mut req = minimal_request("m");
    req.tools = Some(vec![json!({"type": "function", "function": {"name": "x"}})]);
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert!(json["tools"].is_array());
    assert_eq!(json["tools"].as_array().unwrap().len(), 1);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Round-trip preserves all fields
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn full_request_round_trips_through_json() {
    let original = ChatCompletionRequest {
        model: "claude-sonnet-4-5".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text("hello".to_string()),
            name: Some("alice".to_string()),
            tool_calls: None,
            tool_call_id: None,
            extra: std::collections::HashMap::new(),
        }],
        temperature: Some(0.8),
        max_tokens: Some(2048),
        stream: Some(false),
        tools: Some(vec![json!({"x": "y"})]),
        tool_choice: Some(json!("auto")),
        extra: HashMap::new(),
    };
    let json: Value = serde_json::to_value(&original).expect("ser");
    let back: ChatCompletionRequest = serde_json::from_value(json).expect("de");
    assert_eq!(back.model, original.model);
    assert_eq!(back.messages.len(), 1);
    assert!((back.temperature.unwrap() - 0.8).abs() < 1e-6);
    assert_eq!(back.max_tokens, Some(2048));
    assert_eq!(back.stream, Some(false));
    assert!(back.tools.is_some());
    assert_eq!(back.tool_choice, Some(json!("auto")));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — #[serde(flatten)] extra map
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extra_field_captures_unknown_top_level_keys_on_deserialize() {
    // PINS FLATTEN: unknown top-level keys go into the
    // `extra` HashMap so future-OpenAI-fields don't fail to
    // deserialize.
    let json = json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "hi"}],
        "future_unknown_field": "value",
        "another_one": 42
    });
    let req: ChatCompletionRequest = serde_json::from_value(json).expect("de");
    assert!(
        req.extra.contains_key("future_unknown_field"),
        "MUST capture unknown field into extra; got {:?}",
        req.extra.keys().collect::<Vec<_>>()
    );
    assert_eq!(req.extra["future_unknown_field"], json!("value"));
    assert_eq!(req.extra["another_one"], json!(42));
}

#[test]
fn extra_map_serializes_back_to_top_level_keys() {
    let mut extra = HashMap::new();
    extra.insert("custom_field".to_string(), json!("custom_value"));
    extra.insert("count".to_string(), json!(99));

    let req = ChatCompletionRequest {
        model: "m".to_string(),
        messages: vec![minimal_message()],
        temperature: None,
        max_tokens: None,
        stream: None,
        tools: None,
        tool_choice: None,
        extra,
    };
    let json: Value = serde_json::to_value(&req).expect("ser");
    assert_eq!(
        json["custom_field"], "custom_value",
        "extra field MUST appear at top level; got {json}"
    );
    assert_eq!(json["count"], 99);
}

#[test]
fn known_field_does_not_leak_into_extra_map() {
    // model/messages/temperature/max_tokens/stream/tools/
    // tool_choice ARE known — must NOT land in extra.
    let json = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi"}],
        "temperature": 0.5,
        "max_tokens": 100
    });
    let req: ChatCompletionRequest = serde_json::from_value(json).expect("de");
    assert!(!req.extra.contains_key("model"));
    assert!(!req.extra.contains_key("messages"));
    assert!(!req.extra.contains_key("temperature"));
    assert!(!req.extra.contains_key("max_tokens"));
}

#[test]
fn empty_extra_map_does_not_emit_extra_keys() {
    let req = minimal_request("m");
    let json: Value = serde_json::to_value(&req).expect("ser");
    // Top-level keys are model + messages only.
    let obj = json.as_object().unwrap();
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["messages", "model"]);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — ChatMessage field-level serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn chat_message_minimal_serializes_only_role_and_content() {
    let msg = minimal_message();
    let json: Value = serde_json::to_value(&msg).expect("ser");
    let obj = json.as_object().unwrap();
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["content", "role"]);
}

#[test]
fn chat_message_with_name_serializes_name_field() {
    let mut msg = minimal_message();
    msg.name = Some("alice".to_string());
    let json: Value = serde_json::to_value(&msg).expect("ser");
    assert_eq!(json["name"], "alice");
}

#[test]
fn chat_message_none_name_skipped() {
    let msg = minimal_message();
    let json: Value = serde_json::to_value(&msg).expect("ser");
    assert!(json.get("name").is_none());
}

#[test]
fn chat_message_none_tool_calls_skipped() {
    let msg = minimal_message();
    let json: Value = serde_json::to_value(&msg).expect("ser");
    assert!(json.get("tool_calls").is_none());
}

#[test]
fn chat_message_none_tool_call_id_skipped() {
    let msg = minimal_message();
    let json: Value = serde_json::to_value(&msg).expect("ser");
    assert!(json.get("tool_call_id").is_none());
}

#[test]
fn chat_message_extra_preserves_reasoning_fields() {
    let json = json!({
        "role": "assistant",
        "content": "done",
        "reasoning_content": "thought",
        "reasoning_details": [{ "text": "detail" }]
    });
    let msg: ChatMessage = serde_json::from_value(json).expect("de");
    assert_eq!(msg.extra["reasoning_content"], "thought");
    assert_eq!(
        msg.extra["reasoning_details"],
        json!([{ "text": "detail" }])
    );

    let serialized: Value = serde_json::to_value(&msg).expect("ser");
    assert_eq!(serialized["reasoning_content"], "thought");
    assert_eq!(
        serialized["reasoning_details"],
        json!([{ "text": "detail" }])
    );
}

#[test]
fn chat_message_required_role_field_deserialize_errors_when_missing() {
    let json = json!({"content": "hi"});
    let outcome: Result<ChatMessage, _> = serde_json::from_value(json);
    assert!(outcome.is_err(), "missing role MUST be rejected");
}

#[test]
fn chat_message_required_content_field_deserialize_errors_when_missing() {
    let json = json!({"role": "user"});
    let outcome: Result<ChatMessage, _> = serde_json::from_value(json);
    assert!(outcome.is_err(), "missing content MUST be rejected");
}

#[test]
fn chat_message_clone_preserves_named_and_extra_fields() {
    let mut extra = HashMap::new();
    extra.insert("reasoning_content".to_string(), json!("thought"));

    let original = ChatMessage {
        role: "tool".to_string(),
        content: MessageContent::Text("result".to_string()),
        name: Some("name_marker".to_string()),
        tool_calls: Some(vec![json!({"id": "c1"})]),
        tool_call_id: Some("call-x".to_string()),
        extra,
    };
    let cloned = original.clone();
    assert_eq!(cloned.role, original.role);
    assert_eq!(cloned.name, original.name);
    assert_eq!(cloned.tool_calls, original.tool_calls);
    assert_eq!(cloned.tool_call_id, original.tool_call_id);
    assert_eq!(cloned.extra, original.extra);
}
