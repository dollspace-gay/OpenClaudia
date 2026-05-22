//! End-to-end tests for `tools::get_all_tool_definitions` —
//! the subagents=true/false delta, count invariants,
//! and the wire-shape preservation across both forks.
//!
//! Sprint 182 of the verification effort. Sprint 53 had
//! basic structure tests; this file pins the count delta
//! that the subagents flag introduces and the per-tool
//! invariants applied uniformly to both base + subagent
//! tools.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{get_all_tool_definitions, get_tool_definitions};
use serde_json::Value;

fn tool_names(defs: &Value) -> Vec<String> {
    defs.as_array()
        .expect("array")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(String::from))
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — get_tool_definitions returns the base catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_tool_definitions_returns_array_of_function_objects() {
    let defs = get_tool_definitions();
    let arr = defs.as_array().expect("array");
    assert!(!arr.is_empty(), "base catalog MUST have tools");
    for tool in arr {
        assert_eq!(tool["type"], "function");
        assert!(tool["function"].is_object());
    }
}

#[test]
fn get_tool_definitions_matches_documented_34_tool_count() {
    // PINS CATALOG SIZE: 34 base tools (matches sprint 160).
    let defs = get_tool_definitions();
    let arr = defs.as_array().expect("array");
    assert_eq!(
        arr.len(),
        34,
        "PINS: 34 base tools (adding tools requires bumping this)"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — subagents flag delta
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_all_with_subagents_false_matches_base_count() {
    // PINS: subagents=false → identical to get_tool_definitions.
    let base = get_tool_definitions();
    let no_sub = get_all_tool_definitions(false);
    let base_arr = base.as_array().unwrap();
    let no_sub_arr = no_sub.as_array().unwrap();
    assert_eq!(base_arr.len(), no_sub_arr.len());
}

#[test]
fn get_all_with_subagents_true_adds_2_tools() {
    // PINS DOC: get_subagent_tool_definitions adds exactly 2 tools.
    let base = get_tool_definitions();
    let with_sub = get_all_tool_definitions(true);
    let base_count = base.as_array().unwrap().len();
    let with_sub_count = with_sub.as_array().unwrap().len();
    assert_eq!(
        with_sub_count,
        base_count + 2,
        "PINS DELTA: subagents=true adds exactly 2 tools"
    );
}

#[test]
fn get_all_with_subagents_true_count_is_36() {
    // PINS: 34 base + 2 subagent = 36 total.
    let defs = get_all_tool_definitions(true);
    let arr = defs.as_array().expect("array");
    assert_eq!(arr.len(), 36);
}

#[test]
fn subagent_tools_appear_at_end_after_base_tools() {
    // PINS ORDER: subagent tools are EXTENDED onto base, so
    // they appear at the tail of the catalog.
    let base_names = tool_names(&get_tool_definitions());
    let all_names = tool_names(&get_all_tool_definitions(true));
    let base_len = base_names.len();
    // First N names match base catalog verbatim.
    assert_eq!(&all_names[..base_len], &base_names[..]);
}

#[test]
fn subagents_true_adds_task_tool() {
    // PINS DOC: task tool is one of the 2 subagent tools.
    let names = tool_names(&get_all_tool_definitions(true));
    let has_task = names.iter().any(|n| n.contains("task") || n == "Task");
    assert!(has_task, "PINS: task tool present in subagent set");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Per-tool wire-shape invariants across both forks
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_tool_in_no_subagent_fork_has_required_shape() {
    let defs = get_all_tool_definitions(false);
    for tool in defs.as_array().unwrap() {
        assert_eq!(tool["type"], "function");
        assert!(tool["function"]["name"].is_string());
        assert!(tool["function"]["description"].is_string());
        assert!(tool["function"]["parameters"].is_object());
    }
}

#[test]
fn every_tool_in_with_subagent_fork_has_required_shape() {
    let defs = get_all_tool_definitions(true);
    for tool in defs.as_array().unwrap() {
        assert_eq!(tool["type"], "function");
        assert!(tool["function"]["name"].is_string());
        assert!(tool["function"]["description"].is_string());
        assert!(tool["function"]["parameters"].is_object());
    }
}

#[test]
fn no_duplicate_tool_names_across_full_catalog_with_subagents() {
    let mut names = tool_names(&get_all_tool_definitions(true));
    let original_len = names.len();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        original_len,
        "PINS UNIQUENESS: no duplicate tool names across full catalog"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Determinism + idempotency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn repeated_get_all_calls_return_same_count() {
    let counts: Vec<usize> = (0..5)
        .map(|_| get_all_tool_definitions(true).as_array().unwrap().len())
        .collect();
    let first = counts[0];
    assert!(counts.iter().all(|&c| c == first));
}

#[test]
fn repeated_get_all_calls_return_same_names_in_same_order() {
    let a = tool_names(&get_all_tool_definitions(true));
    let b = tool_names(&get_all_tool_definitions(true));
    assert_eq!(a, b, "tool order MUST be deterministic");
}

#[test]
fn get_tool_definitions_and_get_all_false_have_same_byte_serialization() {
    let base_json = serde_json::to_string(&get_tool_definitions()).expect("ser");
    let nosub_json = serde_json::to_string(&get_all_tool_definitions(false)).expect("ser");
    assert_eq!(base_json, nosub_json);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Subagent tools wire shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn subagent_only_tools_have_unique_names_not_in_base_catalog() {
    let base_names: std::collections::HashSet<String> =
        tool_names(&get_tool_definitions()).into_iter().collect();
    let all_names: Vec<String> = tool_names(&get_all_tool_definitions(true));
    // Subagent tools are the names added when subagents=true.
    let subagent_only: Vec<&String> = all_names
        .iter()
        .filter(|n| !base_names.contains(*n))
        .collect();
    assert_eq!(
        subagent_only.len(),
        2,
        "PINS: exactly 2 subagent-only tools added"
    );
    // Each subagent tool name MUST be distinct.
    let mut deduped = subagent_only.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(deduped.len(), 2);
}

#[test]
fn subagent_tools_carry_function_envelope_too() {
    let base_names: std::collections::HashSet<String> =
        tool_names(&get_tool_definitions()).into_iter().collect();
    let defs = get_all_tool_definitions(true);
    let arr = defs.as_array().unwrap();
    for tool in arr {
        let name = tool["function"]["name"]
            .as_str()
            .expect("name string")
            .to_string();
        if !base_names.contains(&name) {
            // Subagent-only tool — verify the same envelope shape.
            assert_eq!(tool["type"], "function");
            assert!(tool["function"]["description"].is_string());
            assert!(tool["function"]["parameters"].is_object());
        }
    }
}
