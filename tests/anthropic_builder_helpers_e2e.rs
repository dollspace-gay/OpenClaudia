//! End-to-end tests for `providers::anthropic` builder helpers —
//! `build_system_blocks`, `build_system_blocks_from_string`,
//! `convert_messages_to_anthropic`, `convert_tools_to_anthropic`.
//!
//! Sprint 122 of the verification effort. Sprint 17
//! (`provider_transform_e2e`) covered the adapter trait
//! impls + response transforms; this file pins the bare
//! helper functions used by both the adapter and the
//! Anthropic-direct mode of the proxy.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::prompt::SystemPromptBlocks;
use openclaudia::providers::{
    build_system_blocks, build_system_blocks_from_string, convert_messages_to_anthropic,
    convert_tools_to_anthropic,
};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — build_system_blocks (SystemPromptBlocks → Anthropic array)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn build_blocks_with_only_stable_prefix_returns_single_block_with_cache_control() {
    let blocks = SystemPromptBlocks {
        stable_prefix: "Stable identity + tools".to_string(),
        dynamic_suffix: String::new(),
    };
    let result = build_system_blocks(&blocks);
    let arr = result.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "Stable identity + tools");
    // PINS CACHE-CONTROL: stable prefix MUST have ephemeral.
    assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn build_blocks_with_dynamic_suffix_returns_two_blocks() {
    let blocks = SystemPromptBlocks {
        stable_prefix: "stable".to_string(),
        dynamic_suffix: "dynamic".to_string(),
    };
    let result = build_system_blocks(&blocks);
    let arr = result.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["text"], "stable");
    assert_eq!(arr[1]["text"], "dynamic");
}

#[test]
fn build_blocks_dynamic_suffix_has_no_cache_control() {
    // PINS DOC: dynamic suffix changes per-turn → NOT cached.
    let blocks = SystemPromptBlocks {
        stable_prefix: "s".to_string(),
        dynamic_suffix: "d".to_string(),
    };
    let result = build_system_blocks(&blocks);
    let arr = result.as_array().expect("array");
    assert!(
        arr[1].get("cache_control").is_none(),
        "dynamic suffix MUST NOT have cache_control; got {arr:?}"
    );
}

#[test]
fn build_blocks_with_empty_stable_prefix_still_emits_block() {
    let blocks = SystemPromptBlocks {
        stable_prefix: String::new(),
        dynamic_suffix: "only-dynamic".to_string(),
    };
    let result = build_system_blocks(&blocks);
    let arr = result.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["text"], "");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — build_system_blocks_from_string (legacy single-string)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn build_from_string_returns_single_block_with_cache_control() {
    let result = build_system_blocks_from_string("You are a helpful assistant.");
    let arr = result.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "You are a helpful assistant.");
    assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn build_from_string_with_empty_input_still_emits_block() {
    let result = build_system_blocks_from_string("");
    let arr = result.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["text"], "");
}

#[test]
fn build_from_string_handles_multi_line_input_verbatim() {
    let input = "Line 1\nLine 2\n\nLine 4";
    let result = build_system_blocks_from_string(input);
    let arr = result.as_array().expect("array");
    assert_eq!(arr[0]["text"], input);
}

#[test]
fn build_from_string_handles_unicode_input() {
    let input = "日本語システムプロンプト 🎉";
    let result = build_system_blocks_from_string(input);
    let arr = result.as_array().expect("array");
    assert_eq!(arr[0]["text"], input);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — convert_messages_to_anthropic (OpenAI → Anthropic)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn convert_messages_filters_system_messages() {
    let messages = vec![
        json!({"role": "system", "content": "You are helpful"}),
        json!({"role": "user", "content": "Hello"}),
    ];
    let converted = convert_messages_to_anthropic(&messages);
    // System filtered out → 1 message.
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["role"], "user");
}

#[test]
fn convert_messages_passes_user_messages_through() {
    let messages = vec![json!({"role": "user", "content": "Hello"})];
    let converted = convert_messages_to_anthropic(&messages);
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["role"], "user");
}

#[test]
fn convert_messages_passes_assistant_messages_through() {
    let messages = vec![json!({"role": "assistant", "content": "Response"})];
    let converted = convert_messages_to_anthropic(&messages);
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["role"], "assistant");
}

#[test]
fn convert_messages_tool_role_becomes_user_with_tool_result_block() {
    // PINS OPENAI→ANTHROPIC MAPPING: tool role → user role
    // with type: "tool_result" content block.
    let messages = vec![json!({
        "role": "tool",
        "tool_call_id": "call-123",
        "content": "result body"
    })];
    let converted = convert_messages_to_anthropic(&messages);
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["role"], "user");
    let content = converted[0]["content"].as_array().expect("array");
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["tool_use_id"], "call-123");
    assert_eq!(content[0]["content"], "result body");
}

#[test]
fn convert_messages_tool_result_with_is_error_true_preserves_field() {
    let messages = vec![json!({
        "role": "tool",
        "tool_call_id": "call-1",
        "content": "failure",
        "is_error": true
    })];
    let converted = convert_messages_to_anthropic(&messages);
    let content = converted[0]["content"].as_array().expect("array");
    assert_eq!(content[0]["is_error"], true);
}

#[test]
fn convert_messages_tool_result_without_is_error_omits_field() {
    let messages = vec![json!({
        "role": "tool",
        "tool_call_id": "call-1",
        "content": "ok"
    })];
    let converted = convert_messages_to_anthropic(&messages);
    let content = converted[0]["content"].as_array().expect("array");
    assert!(
        content[0].get("is_error").is_none(),
        "default false is_error MUST NOT be emitted; got {content:?}"
    );
}

#[test]
fn convert_messages_assistant_with_tool_calls_becomes_tool_use_blocks() {
    let messages = vec![json!({
        "role": "assistant",
        "content": "Let me check",
        "tool_calls": [{
            "id": "call-x",
            "type": "function",
            "function": {
                "name": "bash",
                "arguments": "{\"command\": \"ls\"}"
            }
        }]
    })];
    let converted = convert_messages_to_anthropic(&messages);
    assert_eq!(converted.len(), 1);
    let content = converted[0]["content"].as_array().expect("array");
    // First block: text. Second block: tool_use.
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "Let me check");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "call-x");
    assert_eq!(content[1]["name"], "bash");
    // PINS PARSE: arguments STRING parsed into JSON OBJECT
    // on Anthropic side (input field).
    assert_eq!(content[1]["input"]["command"], "ls");
}

#[test]
fn convert_messages_assistant_with_empty_tool_calls_array_still_works() {
    let messages = vec![json!({
        "role": "assistant",
        "content": "text only",
        "tool_calls": []
    })];
    let converted = convert_messages_to_anthropic(&messages);
    assert_eq!(converted.len(), 1);
    let content = converted[0]["content"].as_array().expect("array");
    // Has the text block (no tool_use blocks).
    assert!(content
        .iter()
        .any(|b| b["type"] == "text" && b["text"] == "text only"));
}

#[test]
fn convert_messages_assistant_with_unparseable_args_falls_back_to_empty_object() {
    let messages = vec![json!({
        "role": "assistant",
        "tool_calls": [{
            "id": "x",
            "function": {"name": "tool", "arguments": "not valid json {{"}
        }]
    })];
    let converted = convert_messages_to_anthropic(&messages);
    let content = converted[0]["content"].as_array().expect("array");
    // PINS RESILIENCE: bad arguments → input = {}
    let tool_use = content
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("present");
    assert_eq!(tool_use["input"], json!({}));
}

#[test]
fn convert_messages_empty_slice_returns_empty_vec() {
    let converted = convert_messages_to_anthropic(&[]);
    assert!(converted.is_empty());
}

#[test]
fn convert_messages_all_system_messages_returns_empty_vec() {
    let messages = vec![
        json!({"role": "system", "content": "a"}),
        json!({"role": "system", "content": "b"}),
    ];
    let converted = convert_messages_to_anthropic(&messages);
    assert!(converted.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — convert_tools_to_anthropic
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn convert_tools_openai_format_to_anthropic_format() {
    let tools = vec![json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Run a shell command",
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}}
            }
        }
    })];
    let converted = convert_tools_to_anthropic(&tools);
    assert_eq!(converted.len(), 1);
    // PINS FORMAT: Anthropic tools have name/description/
    // input_schema at top level (no "function" wrapper).
    assert_eq!(converted[0]["name"], "bash");
    assert_eq!(converted[0]["description"], "Run a shell command");
    assert_eq!(converted[0]["input_schema"]["type"], "object");
}

#[test]
fn convert_tools_empty_slice_returns_empty_vec() {
    let converted = convert_tools_to_anthropic(&[]);
    assert!(converted.is_empty());
}

#[test]
fn convert_tools_multi_tool_preserves_count() {
    let tools = vec![
        json!({"type": "function", "function": {"name": "a", "parameters": {}}}),
        json!({"type": "function", "function": {"name": "b", "parameters": {}}}),
        json!({"type": "function", "function": {"name": "c", "parameters": {}}}),
    ];
    let converted = convert_tools_to_anthropic(&tools);
    assert_eq!(converted.len(), 3);
}
