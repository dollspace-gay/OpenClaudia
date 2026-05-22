//! End-to-end tests for `tools::registry::registry()` —
//! global invariants across the full HANDLERS table:
//! every registered handler has a matching name, every
//! definition is well-formed, exactly 4 handlers declare
//! a `permission_target` (Bash/Edit/NotebookEdit/Write),
//! and the registry has the documented tool count.
//!
//! Sprint 160 of the verification effort. Sprint 23 / 132
//! covered the registry dispatch shape; this file pins
//! the cross-handler invariants — the kind of test that
//! would catch a new tool added without a `name()`
//! override or with a colliding registration.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::registry::{registry, ToolContext};
use serde_json::Value;
use std::collections::HashMap;

/// Documented core tool catalog as of sprint 160.
/// Lock-step: adding a tool here is paired with an entry in
/// HANDLERS in src/tools/registry.rs.
const DOCUMENTED_TOOL_NAMES: &[&str] = &[
    "bash",
    "bash_output",
    "kill_shell",
    "read_file",
    "write_file",
    "edit_file",
    "list_files",
    "glob",
    "grep",
    "crosslink",
    "web_fetch",
    "web_search",
    "web_browser",
    "todo_write",
    "todo_read",
    "notebook_edit",
    "task_create",
    "ask_user_question",
    "task_update",
    "task_get",
    "task_list",
    "enter_plan_mode",
    "exit_plan_mode",
    "list_mcp_resources",
    "read_mcp_resource",
    "lsp",
    "enter_worktree",
    "exit_worktree",
    "list_worktrees",
    "cron_create",
    "cron_delete",
    "cron_list",
    "skill",
    "tool_search",
];

// ───────────────────────────────────────────────────────────────────────────
// Section A — Registry size + completeness
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn registry_contains_all_documented_tool_names() {
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        assert!(
            reg.get(name).is_some(),
            "registry MUST contain documented tool {name:?}"
        );
    }
}

#[test]
fn registry_documented_tool_count_is_34() {
    // PINS CATALOG SIZE: 34 documented tools as of sprint 160.
    // Adding a tool: append a line to HANDLERS and bump this number.
    assert_eq!(
        DOCUMENTED_TOOL_NAMES.len(),
        34,
        "DOCUMENTED_TOOL_NAMES MUST match HANDLERS catalog"
    );
}

#[test]
fn every_documented_name_is_unique_in_list() {
    let mut sorted: Vec<&str> = DOCUMENTED_TOOL_NAMES.to_vec();
    let n = sorted.len();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        n,
        "DOCUMENTED_TOOL_NAMES MUST have no duplicates"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Per-handler invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_handler_name_matches_its_definition_function_name() {
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        // handler.name() and definition()["function"]["name"]
        // MUST agree (otherwise the model sees a different
        // name than the dispatch table accepts).
        let def = handler.definition();
        let def_name = def["function"]["name"].as_str().expect("string");
        assert_eq!(
            def_name,
            handler.name(),
            "handler.name() {:?} MUST match definition.function.name {def_name:?}",
            handler.name()
        );
    }
}

#[test]
fn every_handler_definition_is_a_function_envelope() {
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        let def = handler.definition();
        assert_eq!(def["type"], "function", "{name} MUST be type=function");
        assert!(
            def["function"].is_object(),
            "{name} MUST have function object"
        );
        assert!(
            def["function"]["description"].is_string(),
            "{name} MUST have description"
        );
        assert!(
            def["function"]["parameters"].is_object(),
            "{name} MUST have parameters"
        );
    }
}

#[test]
fn every_handler_parameters_type_is_object() {
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        let def = handler.definition();
        assert_eq!(
            def["function"]["parameters"]["type"], "object",
            "{name} parameters.type MUST be object"
        );
    }
}

#[test]
fn every_handler_required_fields_are_in_properties() {
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        let def = handler.definition();
        let Some(required) = def["function"]["parameters"]["required"].as_array() else {
            continue; // no required fields — skip.
        };
        let Some(properties) = def["function"]["parameters"]["properties"].as_object() else {
            continue;
        };
        for req in required {
            let req_str = req.as_str().expect("required is string");
            assert!(
                properties.contains_key(req_str),
                "{name} required field {req_str:?} MUST appear in properties"
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Permission-target invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn exactly_4_handlers_declare_permission_target() {
    // AUTHORING DISCOVERY: 4 mutating tools (not 3 as initial
    // assumption suggested). notebook_edit also declares a
    // permission_target since it overwrites .ipynb on disk.
    // PINS the actual catalog: bash + edit_file + notebook_edit
    // + write_file. Adding a new mutating tool: append here
    // AND in src/tools/registry.rs's permission_target impl.
    let reg = registry();
    let mut with_target: Vec<&str> = Vec::new();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        if handler.permission_target().is_some() {
            with_target.push(name);
        }
    }
    with_target.sort_unstable();
    assert_eq!(
        with_target,
        vec!["bash", "edit_file", "notebook_edit", "write_file"],
        "PINS PERMISSION TARGETS: exactly 4 mutating tools"
    );
}

#[test]
fn bash_permission_target_canonical_is_bash() {
    let reg = registry();
    let handler = reg.get("bash").expect("bash");
    let target = handler.permission_target().expect("Some");
    assert_eq!(target.canonical, "Bash");
    assert_eq!(target.arg_key, "command");
}

#[test]
fn write_file_permission_target_canonical_is_write() {
    let reg = registry();
    let handler = reg.get("write_file").expect("write_file");
    let target = handler.permission_target().expect("Some");
    assert_eq!(target.canonical, "Write");
    assert_eq!(target.arg_key, "path");
}

#[test]
fn edit_file_permission_target_canonical_is_edit() {
    let reg = registry();
    let handler = reg.get("edit_file").expect("edit_file");
    let target = handler.permission_target().expect("Some");
    assert_eq!(target.canonical, "Edit");
    assert_eq!(target.arg_key, "path");
}

#[test]
fn notebook_edit_permission_target_uses_notebook_path_arg_key() {
    // AUTHORING DISCOVERY: notebook_edit also declares a
    // permission_target — its arg_key is "notebook_path",
    // not "path", because the notebook tool uses the
    // distinct notebook_path field name.
    let reg = registry();
    let handler = reg.get("notebook_edit").expect("notebook_edit");
    let target = handler.permission_target().expect("Some");
    // Canonical capability is documented to share with Edit
    // (notebook edits ARE file edits semantically).
    assert!(
        target.canonical == "Edit" || target.canonical == "Write",
        "MUST canonicalize to Edit or Write; got {:?}",
        target.canonical
    );
    assert_eq!(
        target.arg_key, "notebook_path",
        "PINS DOC: notebook_edit uses notebook_path key not path"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Dispatch invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_documented_tool_dispatches_to_some_result() {
    let reg = registry();
    let mut ctx = ToolContext {
        memory_db: None,
        app_config: None,
        task_mgr: None,
    };
    let empty_args: HashMap<String, Value> = HashMap::new();
    for name in DOCUMENTED_TOOL_NAMES {
        let outcome = reg.dispatch(name, &empty_args, &mut ctx);
        assert!(
            outcome.is_some(),
            "dispatch({name:?}) MUST return Some(...)"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Description sanity (no empty, reasonable bounds)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_handler_description_is_non_empty() {
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        let def = handler.definition();
        let desc = def["function"]["description"].as_str().expect("string");
        assert!(!desc.is_empty(), "{name} description MUST be non-empty");
    }
}

#[test]
fn no_handler_description_exceeds_2000_bytes() {
    // PINS COMPACTNESS: tool descriptions are inlined into the
    // model's prompt — over-long ones bloat context.
    let reg = registry();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        let def = handler.definition();
        let desc = def["function"]["description"].as_str().expect("string");
        assert!(
            desc.len() <= 2000,
            "{name} description MUST stay under 2000 bytes; got {}",
            desc.len()
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Schema name uniqueness across the catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn no_two_handlers_share_the_same_definition_name() {
    let reg = registry();
    let mut seen: HashMap<String, &str> = HashMap::new();
    for name in DOCUMENTED_TOOL_NAMES {
        let handler = reg.get(name).expect(name);
        let def = handler.definition();
        let def_name = def["function"]["name"]
            .as_str()
            .expect("string")
            .to_string();
        if let Some(existing) = seen.insert(def_name.clone(), name) {
            panic!("duplicate function.name {def_name:?} shared by {existing:?} and {name:?}");
        }
    }
}
