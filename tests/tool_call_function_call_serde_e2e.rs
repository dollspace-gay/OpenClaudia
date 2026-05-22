//! End-to-end tests for `tools::ToolCall` and
//! `tools::FunctionCall` serde shape — the OpenAI-compat
//! wire envelope for assistant-emitted tool calls,
//! including the `call_type` → `type` rename.
//!
//! Sprint 181 of the verification effort. Several tests use
//! `ToolCall` via `execute_tool`; this file pins the wire-
//! level serde contract distinct from execution.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{FunctionCall, ToolCall};
use serde_json::{json, Value};

fn make_call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: args.to_string(),
        },
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — ToolCall.call_type renames to "type" on wire
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tool_call_serializes_call_type_field_as_type_key() {
    // PINS WIRE: Rust call_type → JSON "type".
    let tc = make_call("call_1", "bash", "{}");
    let json: Value = serde_json::to_value(&tc).expect("ser");
    assert_eq!(json["type"], "function");
    assert!(
        json.get("call_type").is_none(),
        "Rust field name MUST NOT leak to wire; got {json}"
    );
}

#[test]
fn tool_call_deserializes_type_wire_key_into_call_type_field() {
    let json = json!({
        "id": "call_1",
        "type": "function",
        "function": {"name": "bash", "arguments": "{}"}
    });
    let tc: ToolCall = serde_json::from_value(json).expect("de");
    assert_eq!(tc.call_type, "function");
}

#[test]
fn tool_call_with_unusual_type_string_round_trips() {
    let mut tc = make_call("c1", "x", "{}");
    tc.call_type = "future-experimental-type".to_string();
    let json: Value = serde_json::to_value(&tc).expect("ser");
    assert_eq!(json["type"], "future-experimental-type");
    let back: ToolCall = serde_json::from_value(json).expect("de");
    assert_eq!(back.call_type, "future-experimental-type");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — FunctionCall serde shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn function_call_serializes_name_and_arguments_fields() {
    let fc = FunctionCall {
        name: "list_files".to_string(),
        arguments: r#"{"path":"/tmp"}"#.to_string(),
    };
    let json: Value = serde_json::to_value(&fc).expect("ser");
    assert_eq!(json["name"], "list_files");
    assert_eq!(json["arguments"], r#"{"path":"/tmp"}"#);
}

#[test]
fn function_call_arguments_remain_string_not_object() {
    // PINS WIRE: arguments is a STRING containing JSON, not
    // a parsed object — OpenAI's documented shape.
    let fc = FunctionCall {
        name: "x".to_string(),
        arguments: r#"{"key":"value"}"#.to_string(),
    };
    let json: Value = serde_json::to_value(&fc).expect("ser");
    assert!(
        json["arguments"].is_string(),
        "arguments MUST be string (not object); got {json}"
    );
}

#[test]
fn function_call_with_empty_arguments_string_serializes() {
    let fc = FunctionCall {
        name: "noop".to_string(),
        arguments: String::new(),
    };
    let json: Value = serde_json::to_value(&fc).expect("ser");
    assert_eq!(json["arguments"], "");
}

#[test]
fn function_call_deserialize_from_json_with_string_arguments() {
    let json = json!({
        "name": "echo",
        "arguments": "{\"x\":1}"
    });
    let fc: FunctionCall = serde_json::from_value(json).expect("de");
    assert_eq!(fc.name, "echo");
    assert_eq!(fc.arguments, "{\"x\":1}");
}

#[test]
fn function_call_deserialize_rejects_arguments_as_object() {
    // PINS WIRE: arguments must be string, not an object.
    let json = json!({
        "name": "echo",
        "arguments": {"x": 1}
    });
    let outcome: Result<FunctionCall, _> = serde_json::from_value(json);
    assert!(
        outcome.is_err(),
        "arguments as object MUST be rejected (string required)"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — ToolCall envelope round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tool_call_full_round_trip_preserves_all_fields() {
    let original = make_call(
        "call_abc",
        "edit_file",
        r#"{"path":"/x","old_string":"a","new_string":"b"}"#,
    );
    let json: Value = serde_json::to_value(&original).expect("ser");
    let back: ToolCall = serde_json::from_value(json).expect("de");
    assert_eq!(back.id, original.id);
    assert_eq!(back.call_type, original.call_type);
    assert_eq!(back.function.name, original.function.name);
    assert_eq!(back.function.arguments, original.function.arguments);
}

#[test]
fn tool_call_serialization_matches_openai_documented_shape() {
    // PINS WIRE: exact JSON shape that OpenAI returns.
    let tc = make_call("call_1", "bash", r#"{"command":"ls"}"#);
    let json: Value = serde_json::to_value(&tc).expect("ser");
    let obj = json.as_object().unwrap();
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["function", "id", "type"]);
}

#[test]
fn tool_call_deserialize_from_canonical_openai_chunk() {
    let json = json!({
        "id": "call_xyz",
        "type": "function",
        "function": {
            "name": "bash",
            "arguments": r#"{"command":"echo hi"}"#
        }
    });
    let tc: ToolCall = serde_json::from_value(json).expect("de");
    assert_eq!(tc.id, "call_xyz");
    assert_eq!(tc.call_type, "function");
    assert_eq!(tc.function.name, "bash");
    assert_eq!(tc.function.arguments, r#"{"command":"echo hi"}"#);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Required-field validation on deserialize
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tool_call_missing_id_rejected() {
    let json = json!({
        "type": "function",
        "function": {"name": "x", "arguments": "{}"}
    });
    let outcome: Result<ToolCall, _> = serde_json::from_value(json);
    assert!(outcome.is_err(), "missing id MUST be rejected");
}

#[test]
fn tool_call_missing_type_rejected() {
    let json = json!({
        "id": "x",
        "function": {"name": "x", "arguments": "{}"}
    });
    let outcome: Result<ToolCall, _> = serde_json::from_value(json);
    assert!(outcome.is_err(), "missing type MUST be rejected");
}

#[test]
fn tool_call_missing_function_rejected() {
    let json = json!({
        "id": "x",
        "type": "function"
    });
    let outcome: Result<ToolCall, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn function_call_missing_name_rejected() {
    let json = json!({"arguments": "{}"});
    let outcome: Result<FunctionCall, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn function_call_missing_arguments_rejected() {
    let json = json!({"name": "x"});
    let outcome: Result<FunctionCall, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Clone preserves all fields
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tool_call_clone_preserves_all_fields() {
    let original = make_call("a", "b", "c");
    let cloned = original.clone();
    assert_eq!(cloned.id, original.id);
    assert_eq!(cloned.call_type, original.call_type);
    assert_eq!(cloned.function.name, original.function.name);
    assert_eq!(cloned.function.arguments, original.function.arguments);
}

#[test]
fn function_call_clone_preserves_both_fields() {
    let original = FunctionCall {
        name: "alpha".to_string(),
        arguments: "beta".to_string(),
    };
    let cloned = original.clone();
    assert_eq!(cloned.name, "alpha");
    assert_eq!(cloned.arguments, "beta");
    // Original still usable.
    assert_eq!(original.name, "alpha");
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Unicode + edge content
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tool_call_with_unicode_function_name_round_trips() {
    let tc = make_call("c1", "日本語ツール", "{}");
    let json: Value = serde_json::to_value(&tc).expect("ser");
    let back: ToolCall = serde_json::from_value(json).expect("de");
    assert_eq!(back.function.name, "日本語ツール");
}

#[test]
fn tool_call_with_escaped_quotes_in_arguments_round_trips() {
    let tc = make_call("c1", "bash", r#"{"command":"echo \"hi\""}"#);
    let json: Value = serde_json::to_value(&tc).expect("ser");
    let back: ToolCall = serde_json::from_value(json).expect("de");
    assert_eq!(back.function.arguments, r#"{"command":"echo \"hi\""}"#);
}

#[test]
fn tool_call_with_empty_id_serializes_as_empty_string() {
    let tc = make_call("", "x", "{}");
    let json: Value = serde_json::to_value(&tc).expect("ser");
    assert_eq!(json["id"], "");
}
