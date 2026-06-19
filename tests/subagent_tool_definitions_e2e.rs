//! End-to-end tests for `subagent` tool-definition shapes —
//! `get_task_tool_definition`, `get_agent_output_tool_definition`,
//! `get_task_stop_tool_definition`, `get_subagent_tool_definitions` enum membership +
//! required-field contract + the documented `subagent_type`
//! + model + isolation enum values.
//!
//! Sprint 125 of the verification effort. Sprint 60
//! covered `AgentType::parse_type` + name + serde +
//! description; this file pins the tool-definition wire
//! shape that the model receives via `tools` array (task
//! tool `subagent_type` enum, `agent_output` required
//! fields, `task_stop`, model + isolation enum values).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::subagent::{
    get_agent_output_tool_definition, get_subagent_tool_definitions, get_task_stop_tool_definition,
    get_task_tool_definition, AgentType,
};
use serde_json::Value;

// ───────────────────────────────────────────────────────────────────────────
// Section A — get_task_tool_definition shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_tool_function_envelope_uses_type_function() {
    let def = get_task_tool_definition();
    assert_eq!(def["type"], "function");
}

#[test]
fn task_tool_function_name_is_task() {
    let def = get_task_tool_definition();
    assert_eq!(def["function"]["name"], "task");
}

#[test]
fn task_tool_description_mentions_subagent() {
    let def = get_task_tool_definition();
    let desc = def["function"]["description"].as_str().expect("string");
    assert!(
        desc.contains("subagent") || desc.contains("agent"),
        "MUST describe subagent purpose; got {desc:?}"
    );
}

#[test]
fn task_tool_parameters_object_has_documented_required_fields() {
    let def = get_task_tool_definition();
    let required = def["function"]["parameters"]["required"]
        .as_array()
        .expect("required array");
    let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
    // PINS REQUIRED: description, prompt, subagent_type.
    assert!(names.contains(&"description"));
    assert!(names.contains(&"prompt"));
    assert!(names.contains(&"subagent_type"));
}

#[test]
fn task_tool_subagent_type_enum_includes_4_documented_variants() {
    let def = get_task_tool_definition();
    let enum_values = def["function"]["parameters"]["properties"]["subagent_type"]["enum"]
        .as_array()
        .expect("enum array");
    let values: Vec<&str> = enum_values.iter().filter_map(Value::as_str).collect();
    // PINS DOCUMENTED VARIANTS: task-spawnable names in the enum.
    for expected in AgentType::task_tool_names() {
        assert!(
            values.contains(&expected),
            "subagent_type enum MUST include {expected:?}; got {values:?}"
        );
    }
}

#[test]
fn task_tool_subagent_type_enum_matches_task_spawnable_agents() {
    let def = get_task_tool_definition();
    let enum_values = def["function"]["parameters"]["properties"]["subagent_type"]["enum"]
        .as_array()
        .expect("enum array");
    let values: Vec<&str> = enum_values.iter().filter_map(Value::as_str).collect();
    assert_eq!(values, AgentType::task_tool_names());
    assert!(
        !values.contains(&"coordinator"),
        "coordinator is a profile/router type, not a task-spawnable agent"
    );
}

#[test]
fn task_tool_model_enum_includes_three_tiers() {
    let def = get_task_tool_definition();
    let enum_values = def["function"]["parameters"]["properties"]["model"]["enum"]
        .as_array()
        .expect("enum array");
    let values: Vec<&str> = enum_values.iter().filter_map(Value::as_str).collect();
    // PINS MODEL TIERS: sonnet/opus/haiku.
    for expected in &["sonnet", "opus", "haiku"] {
        assert!(
            values.contains(expected),
            "model enum MUST include {expected:?}; got {values:?}"
        );
    }
}

#[test]
fn task_tool_isolation_enum_includes_worktree() {
    let def = get_task_tool_definition();
    let enum_values = def["function"]["parameters"]["properties"]["isolation"]["enum"]
        .as_array()
        .expect("enum array");
    let found = enum_values
        .iter()
        .filter_map(Value::as_str)
        .any(|v| v == "worktree");
    assert!(found, "isolation enum MUST include worktree");
}

#[test]
fn task_tool_has_run_in_background_boolean_field() {
    let def = get_task_tool_definition();
    let prop = &def["function"]["parameters"]["properties"]["run_in_background"];
    assert_eq!(prop["type"], "boolean");
}

#[test]
fn task_tool_has_resume_string_field() {
    let def = get_task_tool_definition();
    let prop = &def["function"]["parameters"]["properties"]["resume"];
    assert_eq!(prop["type"], "string");
}

#[test]
fn task_tool_parameters_type_is_object() {
    let def = get_task_tool_definition();
    assert_eq!(def["function"]["parameters"]["type"], "object");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — get_agent_output_tool_definition shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agent_output_tool_function_envelope_uses_type_function() {
    let def = get_agent_output_tool_definition();
    assert_eq!(def["type"], "function");
}

#[test]
fn agent_output_tool_function_name_is_agent_output() {
    let def = get_agent_output_tool_definition();
    assert_eq!(def["function"]["name"], "agent_output");
}

#[test]
fn agent_output_tool_description_mentions_background_agent() {
    let def = get_agent_output_tool_definition();
    let desc = def["function"]["description"].as_str().expect("string");
    assert!(
        desc.contains("background") || desc.contains("agent"),
        "MUST mention background agent retrieval; got {desc:?}"
    );
}

#[test]
fn agent_output_tool_parameters_required_includes_agent_id() {
    let def = get_agent_output_tool_definition();
    let required = def["function"]["parameters"]["required"]
        .as_array()
        .expect("required array");
    let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
    assert!(
        names.contains(&"agent_id"),
        "MUST require agent_id; got {names:?}"
    );
}

#[test]
fn agent_output_tool_has_block_boolean_field() {
    let def = get_agent_output_tool_definition();
    let prop = &def["function"]["parameters"]["properties"]["block"];
    assert_eq!(prop["type"], "boolean");
}

#[test]
fn agent_output_tool_parameters_type_is_object() {
    let def = get_agent_output_tool_definition();
    assert_eq!(def["function"]["parameters"]["type"], "object");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B2 — get_task_stop_tool_definition shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_stop_tool_function_envelope_uses_type_function() {
    let def = get_task_stop_tool_definition();
    assert_eq!(def["type"], "function");
}

#[test]
fn task_stop_tool_function_name_is_task_stop() {
    let def = get_task_stop_tool_definition();
    assert_eq!(def["function"]["name"], "task_stop");
}

#[test]
fn task_stop_tool_description_mentions_stop_or_abort() {
    let def = get_task_stop_tool_definition();
    let desc = def["function"]["description"].as_str().expect("string");
    assert!(
        desc.contains("Stop") || desc.contains("abort") || desc.contains("Aborts"),
        "MUST describe cancellation behavior; got {desc:?}"
    );
}

#[test]
fn task_stop_tool_parameters_required_includes_agent_id() {
    let def = get_task_stop_tool_definition();
    let required = def["function"]["parameters"]["required"]
        .as_array()
        .expect("required array");
    let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
    assert!(
        names.contains(&"agent_id"),
        "MUST require agent_id; got {names:?}"
    );
}

#[test]
fn task_stop_tool_parameters_type_is_object() {
    let def = get_task_stop_tool_definition();
    assert_eq!(def["function"]["parameters"]["type"], "object");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — get_subagent_tool_definitions array
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_subagent_tool_definitions_returns_3_tools() {
    let tools = get_subagent_tool_definitions();
    let arr = tools.as_array().expect("array");
    assert_eq!(arr.len(), 3);
}

#[test]
fn get_subagent_tool_definitions_includes_documented_names() {
    let tools = get_subagent_tool_definitions();
    let arr = tools.as_array().expect("array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"task"));
    assert!(names.contains(&"agent_output"));
    assert!(names.contains(&"task_stop"));
}

#[test]
fn get_subagent_tool_definitions_names_are_pairwise_distinct() {
    let tools = get_subagent_tool_definitions();
    let arr = tools.as_array().expect("array");
    let mut names: Vec<&str> = arr
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), n);
}

#[test]
fn get_subagent_tool_definitions_every_entry_is_function_envelope() {
    let tools = get_subagent_tool_definitions();
    let arr = tools.as_array().expect("array");
    for t in arr {
        assert_eq!(t["type"], "function");
        assert!(t["function"]["name"].is_string());
        assert!(t["function"]["description"].is_string());
        assert!(t["function"]["parameters"].is_object());
    }
}

#[test]
fn get_subagent_tool_definitions_is_deterministic_across_calls() {
    let a = get_subagent_tool_definitions();
    let b = get_subagent_tool_definitions();
    assert_eq!(a, b, "tool definitions MUST be deterministic across calls");
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Cross-tool consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn task_tool_resume_field_documents_agent_id_continuity() {
    // PINS CROSSLINK #582: resume MUST be documented to
    // preserve original agent_id (transcript + cache).
    let def = get_task_tool_definition();
    let resume_desc = def["function"]["parameters"]["properties"]["resume"]["description"]
        .as_str()
        .expect("description");
    assert!(
        resume_desc.contains("agent") || resume_desc.contains("ID"),
        "resume description MUST mention agent ID continuity"
    );
}

#[test]
fn task_tool_subagent_type_enum_size_matches_documented_4() {
    let def = get_task_tool_definition();
    let enum_values = def["function"]["parameters"]["properties"]["subagent_type"]["enum"]
        .as_array()
        .expect("enum array");
    // PINS DOC: 4 documented subagent types in tool schema
    // (general-purpose / explore / plan / guide). Note:
    // "coordinator" is intentionally omitted from this tool
    // schema (it's a router type, not directly user-invokable
    // via the task tool — handled separately).
    assert_eq!(
        enum_values.len(),
        4,
        "subagent_type enum MUST have exactly 4 values; got {enum_values:?}"
    );
}
