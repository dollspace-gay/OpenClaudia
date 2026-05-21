//! End-to-end tests for `tools::ToolCallAccumulator` (OpenAI-side
//! streaming tool-call assembly) including the
//! `MAX_PARALLEL_TOOL_CALL_SLOTS` cap, multi-delta argument
//! concatenation, parallel-index slotting, and finalize shape.
//!
//! Sprint 93 of the verification effort. Sprint 45 covered
//! the Anthropic-side accumulator; this file pins the
//! OpenAI-side `ToolCallAccumulator` which the proxy uses
//! when translating SSE chunks from OpenAI-compatible
//! providers back to the unified `ToolCall` shape.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{PartialToolCall, ToolCallAccumulator, MAX_PARALLEL_TOOL_CALL_SLOTS};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Cap constant + constructor
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn max_parallel_tool_call_slots_constant_matches_documented_value() {
    assert_eq!(MAX_PARALLEL_TOOL_CALL_SLOTS, 512);
}

#[test]
fn new_accumulator_has_empty_tool_calls_and_no_tool_calls_flag() {
    let acc = ToolCallAccumulator::new();
    assert!(acc.tool_calls.is_empty());
    assert!(!acc.has_tool_calls());
}

#[test]
fn default_accumulator_matches_new_constructor() {
    let acc_default = ToolCallAccumulator::default();
    let acc_new = ToolCallAccumulator::new();
    assert_eq!(acc_default.tool_calls.len(), acc_new.tool_calls.len());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — process_delta single-call assembly
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn process_single_delta_with_id_type_and_function_name_populates_slot() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "call-abc",
            "type": "function",
            "function": {"name": "bash"}
        }]
    }));
    assert_eq!(acc.tool_calls.len(), 1);
    let p = &acc.tool_calls[0];
    assert_eq!(p.index, 0);
    assert_eq!(p.id, "call-abc");
    assert_eq!(p.call_type, "function");
    assert_eq!(p.function_name, "bash");
    assert_eq!(p.function_arguments, "");
    assert!(acc.has_tool_calls());
}

#[test]
fn process_arguments_delta_appends_byte_string_chunks() {
    // PINS STREAMING CONTRACT: function.arguments chunks
    // are CONCATENATED across deltas (not replaced).
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "function": {"arguments": "{\"co"}
        }]
    }));
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "function": {"arguments": "mmand\":"}
        }]
    }));
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "function": {"arguments": " \"ls\"}"}
        }]
    }));
    assert_eq!(
        acc.tool_calls[0].function_arguments, "{\"command\": \"ls\"}",
        "MUST concatenate arg chunks byte-exact"
    );
}

#[test]
fn id_field_can_be_set_in_a_later_delta_after_index_was_seen() {
    let mut acc = ToolCallAccumulator::new();
    // First delta: only function name.
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "function": {"name": "bash"}
        }]
    }));
    // Second delta: id arrives now.
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "later-id"
        }]
    }));
    assert_eq!(acc.tool_calls[0].id, "later-id");
    assert_eq!(acc.tool_calls[0].function_name, "bash");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Parallel slotting (multi-index)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parallel_tool_calls_get_separate_slots() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [
            {"index": 0, "id": "a", "function": {"name": "bash"}},
            {"index": 1, "id": "b", "function": {"name": "read"}}
        ]
    }));
    assert_eq!(acc.tool_calls.len(), 2);
    assert_eq!(acc.tool_calls[0].id, "a");
    assert_eq!(acc.tool_calls[1].id, "b");
}

#[test]
fn delta_with_index_jump_pre_allocates_intermediate_slots() {
    let mut acc = ToolCallAccumulator::new();
    // First delta at index 0.
    acc.process_delta(&json!({
        "tool_calls": [{"index": 0, "id": "first"}]
    }));
    // Then delta at index 3 — intermediate slots 1, 2 are
    // pre-allocated as defaults.
    acc.process_delta(&json!({
        "tool_calls": [{"index": 3, "id": "fourth"}]
    }));
    assert_eq!(acc.tool_calls.len(), 4);
    assert_eq!(acc.tool_calls[0].id, "first");
    assert_eq!(acc.tool_calls[1].id, "");
    assert_eq!(acc.tool_calls[2].id, "");
    assert_eq!(acc.tool_calls[3].id, "fourth");
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — MAX_PARALLEL_TOOL_CALL_SLOTS cap
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn delta_with_index_at_cap_boundary_is_dropped() {
    // index == cap is OOB (cap is exclusive upper bound).
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": MAX_PARALLEL_TOOL_CALL_SLOTS,
            "id": "should-be-dropped"
        }]
    }));
    assert!(
        acc.tool_calls.is_empty(),
        "index >= cap MUST be dropped without allocation"
    );
}

#[test]
fn delta_with_index_far_past_cap_is_dropped_no_unbounded_alloc() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 999_999,
            "id": "evil"
        }]
    }));
    assert!(
        acc.tool_calls.is_empty(),
        "extreme index MUST be dropped (DoS guard)"
    );
}

#[test]
fn delta_just_below_cap_is_accepted() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": MAX_PARALLEL_TOOL_CALL_SLOTS - 1,
            "id": "edge"
        }]
    }));
    assert_eq!(acc.tool_calls.len(), MAX_PARALLEL_TOOL_CALL_SLOTS);
    assert_eq!(acc.tool_calls[MAX_PARALLEL_TOOL_CALL_SLOTS - 1].id, "edge");
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — finalize
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn finalize_with_no_deltas_returns_empty_vec() {
    let acc = ToolCallAccumulator::new();
    let calls = acc.finalize();
    assert!(calls.is_empty());
}

#[test]
fn finalize_produces_tool_calls_with_all_fields_preserved() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": "call-1",
            "type": "function",
            "function": {"name": "bash", "arguments": "{\"command\": \"ls\"}"}
        }]
    }));
    let calls = acc.finalize();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call-1");
    assert_eq!(calls[0].call_type, "function");
    assert_eq!(calls[0].function.name, "bash");
    assert_eq!(calls[0].function.arguments, "{\"command\": \"ls\"}");
}

#[test]
fn finalize_preserves_call_order_by_index() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [
            {"index": 0, "id": "first", "function": {"name": "a"}},
            {"index": 1, "id": "second", "function": {"name": "b"}},
            {"index": 2, "id": "third", "function": {"name": "c"}}
        ]
    }));
    let calls = acc.finalize();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].id, "first");
    assert_eq!(calls[1].id, "second");
    assert_eq!(calls[2].id, "third");
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — clear + has_tool_calls
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn has_tool_calls_returns_false_on_empty() {
    let acc = ToolCallAccumulator::new();
    assert!(!acc.has_tool_calls());
}

#[test]
fn has_tool_calls_returns_true_after_delta_processed() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{"index": 0, "id": "x"}]
    }));
    assert!(acc.has_tool_calls());
}

#[test]
fn clear_resets_state_to_empty() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{"index": 0, "id": "x"}]
    }));
    assert!(acc.has_tool_calls());
    acc.clear();
    assert!(!acc.has_tool_calls());
    assert!(acc.tool_calls.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Edge cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn delta_with_no_tool_calls_field_is_noop() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({"content": "text but no tool_calls"}));
    assert!(acc.tool_calls.is_empty());
}

#[test]
fn delta_with_empty_tool_calls_array_is_noop() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({"tool_calls": []}));
    assert!(acc.tool_calls.is_empty());
}

#[test]
fn delta_with_missing_index_defaults_to_zero() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{"id": "no-index"}]
    }));
    assert_eq!(acc.tool_calls.len(), 1);
    assert_eq!(acc.tool_calls[0].id, "no-index");
}

#[test]
fn delta_with_non_string_id_field_is_ignored() {
    let mut acc = ToolCallAccumulator::new();
    acc.process_delta(&json!({
        "tool_calls": [{
            "index": 0,
            "id": 12345,
            "function": {"name": "bash"}
        }]
    }));
    assert_eq!(acc.tool_calls.len(), 1);
    // Non-string id silently dropped; function_name still captured.
    assert_eq!(acc.tool_calls[0].id, "");
    assert_eq!(acc.tool_calls[0].function_name, "bash");
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — PartialToolCall shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn partial_tool_call_default_has_empty_strings_and_zero_index() {
    let p = PartialToolCall::default();
    assert_eq!(p.index, 0);
    assert_eq!(p.id, "");
    assert_eq!(p.call_type, "");
    assert_eq!(p.function_name, "");
    assert_eq!(p.function_arguments, "");
}

#[test]
fn partial_tool_call_clone_preserves_all_fields() {
    let original = PartialToolCall {
        index: 5,
        id: "id-x".to_string(),
        call_type: "function".to_string(),
        function_name: "tool".to_string(),
        function_arguments: "{}".to_string(),
    };
    let cloned = original.clone();
    assert_eq!(cloned.index, original.index);
    assert_eq!(cloned.id, original.id);
    assert_eq!(cloned.function_arguments, original.function_arguments);
}
