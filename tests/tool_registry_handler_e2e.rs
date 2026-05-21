//! End-to-end tests for `tools::registry` `ToolHandler` trait
//! introspection + `PermissionTarget` declarations + registry
//! integrity that sprint 30 left uncovered.
//!
//! Sprint 71 of the verification effort. Sprint 30 covered the
//! schema validation; this file pins the per-handler
//! `permission_target`, `name`/`definition` self-consistency,
//! and the registry's dispatch identity (same handler reference
//! returned across calls).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::registry::registry;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// All tool names that the registry exposes. Mined from
/// `get_tool_definitions` so test stays in sync with the wire
/// list.
fn registered_tool_names() -> Vec<String> {
    let defs = openclaudia::tools::get_tool_definitions();
    defs.as_array()
        .expect("tool definitions is array")
        .iter()
        .filter_map(|def| {
            def.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — name() / definition() self-consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn handler_name_matches_definition_function_name() {
    let r = registry();
    for tool_name in registered_tool_names() {
        let handler = r
            .get(&tool_name)
            .unwrap_or_else(|| panic!("handler for {tool_name:?} MUST be registered"));
        assert_eq!(
            handler.name(),
            tool_name,
            "handler.name() MUST equal registered tool name"
        );
        let def = handler.definition();
        let def_name = def["function"]["name"].as_str().unwrap_or("");
        assert_eq!(
            def_name, tool_name,
            "definition.function.name MUST equal registered tool name; got {def_name:?}"
        );
    }
}

#[test]
fn handler_definition_uses_function_type_envelope() {
    let r = registry();
    for tool_name in registered_tool_names() {
        let handler = r.get(&tool_name).unwrap();
        let def = handler.definition();
        assert_eq!(
            def["type"], "function",
            "tool {tool_name:?} definition MUST have type=function"
        );
        assert!(
            def.get("function").is_some(),
            "tool {tool_name:?} MUST have function envelope"
        );
    }
}

#[test]
fn handler_definition_function_has_parameters_schema() {
    let r = registry();
    for tool_name in registered_tool_names() {
        let handler = r.get(&tool_name).unwrap();
        let def = handler.definition();
        let params = &def["function"]["parameters"];
        assert!(
            params.is_object(),
            "tool {tool_name:?} parameters MUST be an object schema"
        );
        assert_eq!(
            params["type"], "object",
            "tool {tool_name:?} parameters.type MUST be 'object'"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — PermissionTarget declarations
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn bash_handler_declares_bash_canonical_with_command_arg_key() {
    let r = registry();
    let handler = r.get("bash").expect("bash registered");
    let target = handler
        .permission_target()
        .expect("bash MUST declare permission target");
    assert_eq!(target.canonical, "Bash");
    assert_eq!(target.arg_key, "command");
}

#[test]
fn write_file_handler_declares_write_canonical_with_path_arg_key() {
    let r = registry();
    let handler = r.get("write_file").expect("write_file registered");
    let target = handler
        .permission_target()
        .expect("write_file MUST declare permission target");
    assert_eq!(target.canonical, "Write");
    assert_eq!(target.arg_key, "path");
}

#[test]
fn edit_file_handler_declares_edit_canonical() {
    let r = registry();
    let handler = r.get("edit_file").expect("edit_file registered");
    let target = handler.permission_target().expect("MUST declare target");
    assert_eq!(target.canonical, "Edit");
    assert_eq!(target.arg_key, "path");
}

#[test]
fn read_only_tools_declare_no_permission_target() {
    // Documented contract: tools with no side effects return
    // None from permission_target() — the default impl.
    let r = registry();
    for tool_name in &["read_file", "list_files", "glob", "grep"] {
        let handler = r.get(tool_name).expect("registered");
        assert!(
            handler.permission_target().is_none(),
            "read-only tool {tool_name:?} MUST return None from permission_target"
        );
    }
}

#[test]
fn every_handler_with_permission_target_uses_non_empty_canonical_and_arg_key() {
    let r = registry();
    for tool_name in registered_tool_names() {
        let handler = r.get(&tool_name).unwrap();
        if let Some(target) = handler.permission_target() {
            assert!(
                !target.canonical.is_empty(),
                "tool {tool_name:?} permission_target.canonical MUST be non-empty"
            );
            assert!(
                !target.arg_key.is_empty(),
                "tool {tool_name:?} permission_target.arg_key MUST be non-empty"
            );
        }
    }
}

#[test]
fn permission_targets_are_referentially_stable_across_calls() {
    let r = registry();
    let handler = r.get("bash").unwrap();
    let t1 = handler.permission_target();
    let t2 = handler.permission_target();
    assert_eq!(
        t1, t2,
        "permission_target MUST be deterministic per handler"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Registry identity + dispatch shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn registry_get_returns_same_ptr_across_repeat_lookups() {
    let r = registry();
    let h1 = r.get("bash").unwrap();
    let h2 = r.get("bash").unwrap();
    // Same reference target (no heap alloc per dispatch).
    // Compare data-pointer addresses of the trait objects; both
    // arms come from the same OnceLock-backed slot.
    assert!(
        std::ptr::addr_eq(std::ptr::from_ref(h1), std::ptr::from_ref(h2)),
        "registry MUST return identical pointers across calls"
    );
}

#[test]
fn registry_returns_none_for_unregistered_name() {
    let r = registry();
    assert!(r.get("totally-not-registered-2099").is_none());
    assert!(r.get("").is_none());
}

#[test]
fn registry_singleton_is_referentially_stable_across_calls() {
    let r1 = registry();
    let r2 = registry();
    assert!(
        std::ptr::eq(r1, r2),
        "registry() MUST be a singleton (OnceLock-backed)"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — PermissionTarget shape + Eq
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn permission_target_with_same_canonical_and_arg_key_compares_equal() {
    use openclaudia::tools::registry::PermissionTarget;
    let a = PermissionTarget {
        canonical: "Bash",
        arg_key: "command",
    };
    let b = PermissionTarget {
        canonical: "Bash",
        arg_key: "command",
    };
    assert_eq!(a, b);
}

#[test]
fn permission_target_different_canonical_compares_not_equal() {
    use openclaudia::tools::registry::PermissionTarget;
    let a = PermissionTarget {
        canonical: "Bash",
        arg_key: "command",
    };
    let b = PermissionTarget {
        canonical: "Write",
        arg_key: "command",
    };
    assert_ne!(a, b);
}

#[test]
fn permission_target_is_copy_clone_for_zero_alloc_dispatch() {
    use openclaudia::tools::registry::PermissionTarget;
    let a = PermissionTarget {
        canonical: "X",
        arg_key: "y",
    };
    // Copy semantics — value passes without clone() call.
    let b = a;
    let c = a; // a still usable (Copy).
    assert_eq!(b, c);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — All registered tools end-to-end smoke
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn every_registered_tool_has_lookup_handler_and_definition() {
    let r = registry();
    for tool_name in registered_tool_names() {
        let handler = r
            .get(&tool_name)
            .unwrap_or_else(|| panic!("tool {tool_name:?} MUST resolve"));
        // The full pipeline — name + definition + maybe-target
        // — MUST not panic and MUST be self-consistent.
        let _ = handler.name();
        let _ = handler.definition();
        let _ = handler.permission_target();
    }
}
