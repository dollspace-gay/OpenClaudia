//! End-to-end tests for `list_mcp_resources` and
//! `read_mcp_resource` dispatch — both are currently
//! documented stubs that surface a "not wired" error
//! when invoked through the registry. This file pins
//! the schema is published (so the model sees them in
//! the tool list) while the dispatch returns the
//! documented unimplemented marker.
//!
//! Sprint 155 of the verification effort. Sprint 123
//! covered the underlying `McpResource` / `McpCapabilities`
//! wire shapes; this file pins the tool-dispatch-layer
//! contract distinct from the underlying MCP types.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::registry::{registry, ToolContext};
use serde_json::{json, Value};
use std::collections::HashMap;

fn dispatch(name: &str, args: &HashMap<String, Value>) -> (String, bool) {
    let mut ctx = ToolContext {
        memory_db: None,
        app_config: None,
        task_mgr: None,
    };
    registry()
        .dispatch(name, args, &mut ctx)
        .expect("tool must be registered")
}

fn args_with(entries: &[(&str, Value)]) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), v.clone());
    }
    m
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — list_mcp_resources: documented unimplemented stub
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn list_mcp_resources_no_args_returns_documented_not_wired_error() {
    let (msg, is_err) = dispatch("list_mcp_resources", &HashMap::new());
    assert!(is_err, "stub MUST surface as error so model knows to skip");
    assert!(
        msg.contains("list_mcp_resources is not wired into the tool dispatch system yet"),
        "MUST surface documented stub message; got {msg:?}"
    );
}

#[test]
fn list_mcp_resources_with_server_arg_still_returns_stub_error() {
    let args = args_with(&[("server", json!("any-server-name"))]);
    let (msg, is_err) = dispatch("list_mcp_resources", &args);
    assert!(is_err);
    // Stub ignores args; same documented message.
    assert!(msg.contains("not wired into the tool dispatch system yet"));
}

#[test]
fn list_mcp_resources_with_arbitrary_args_returns_stub_error_no_panic() {
    let args = args_with(&[
        ("server", json!("x")),
        ("extra", json!({"k": "v"})),
        ("count", json!(42)),
    ]);
    let (_msg, _is_err) = dispatch("list_mcp_resources", &args);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — read_mcp_resource: documented unimplemented stub
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn read_mcp_resource_no_args_returns_documented_not_wired_error() {
    let (msg, is_err) = dispatch("read_mcp_resource", &HashMap::new());
    assert!(is_err);
    assert!(
        msg.contains("read_mcp_resource is not wired into the tool dispatch system yet"),
        "MUST surface documented stub message; got {msg:?}"
    );
}

#[test]
fn read_mcp_resource_with_server_and_uri_still_returns_stub_error() {
    let args = args_with(&[
        ("server", json!("test-server")),
        ("uri", json!("file:///example")),
    ]);
    let (msg, is_err) = dispatch("read_mcp_resource", &args);
    assert!(is_err);
    assert!(msg.contains("not wired into the tool dispatch system yet"));
}

#[test]
fn read_mcp_resource_with_arbitrary_args_no_panic() {
    let args = args_with(&[
        ("server", json!("x")),
        ("uri", json!("y")),
        ("extra", json!([1, 2, 3])),
    ]);
    let (_msg, _is_err) = dispatch("read_mcp_resource", &args);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Schema is still published despite stub dispatch
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn list_mcp_resources_definition_includes_optional_server_arg() {
    let handler = registry().get("list_mcp_resources").expect("registered");
    let def = handler.definition();
    let server_prop = &def["function"]["parameters"]["properties"]["server"];
    assert_eq!(server_prop["type"], "string");
    // server is OPTIONAL.
    let required = def["function"]["parameters"]["required"]
        .as_array()
        .expect("required array");
    assert!(
        required.is_empty(),
        "list_mcp_resources MUST have no required fields"
    );
}

#[test]
fn read_mcp_resource_definition_requires_server_and_uri() {
    let handler = registry().get("read_mcp_resource").expect("registered");
    let def = handler.definition();
    let required = def["function"]["parameters"]["required"]
        .as_array()
        .expect("required array");
    let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
    // PINS DOC: both server + uri required.
    assert!(names.contains(&"server"));
    assert!(names.contains(&"uri"));
}

#[test]
fn read_mcp_resource_uri_field_is_string() {
    let handler = registry().get("read_mcp_resource").expect("registered");
    let def = handler.definition();
    let uri_prop = &def["function"]["parameters"]["properties"]["uri"];
    assert_eq!(uri_prop["type"], "string");
}

#[test]
fn list_mcp_resources_schema_description_mentions_mcp_servers() {
    let handler = registry().get("list_mcp_resources").expect("registered");
    let def = handler.definition();
    let desc = def["function"]["description"].as_str().expect("string");
    assert!(
        desc.contains("MCP server") || desc.contains("MCP servers"),
        "MUST surface MCP context in description; got {desc:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Stub-message uniformity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn both_mcp_resource_tools_share_documented_unimplemented_phrasing() {
    // PINS UNIFORMITY: both stub tools end their message with
    // "not wired into the tool dispatch system yet" + a note
    // about the schema being published.
    let (l_msg, _) = dispatch("list_mcp_resources", &HashMap::new());
    let (r_msg, _) = dispatch("read_mcp_resource", &HashMap::new());

    assert!(l_msg.contains("not wired into the tool dispatch system yet"));
    assert!(r_msg.contains("not wired into the tool dispatch system yet"));
    assert!(l_msg.contains("schema is published"));
    assert!(r_msg.contains("schema is published"));
}

#[test]
fn stub_messages_name_the_offending_tool_so_model_knows_which_failed() {
    // PINS DIAGNOSTIC: each stub names ITSELF in the error so
    // the model can tell list vs read failed.
    let (l_msg, _) = dispatch("list_mcp_resources", &HashMap::new());
    let (r_msg, _) = dispatch("read_mcp_resource", &HashMap::new());
    assert!(l_msg.starts_with("list_mcp_resources"));
    assert!(r_msg.starts_with("read_mcp_resource"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Registration
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn both_mcp_resource_tools_registered_in_registry() {
    assert!(registry().get("list_mcp_resources").is_some());
    assert!(registry().get("read_mcp_resource").is_some());
}

#[test]
fn both_handlers_have_no_permission_target_read_only_classification() {
    // PINS DOC: read-only tools (no mutation of user state)
    // return None from permission_target.
    let list_handler = registry().get("list_mcp_resources").expect("registered");
    let read_handler = registry().get("read_mcp_resource").expect("registered");
    assert!(
        list_handler.permission_target().is_none(),
        "list_mcp_resources MUST be read-only (no perm target)"
    );
    assert!(
        read_handler.permission_target().is_none(),
        "read_mcp_resource MUST be read-only (no perm target)"
    );
}
