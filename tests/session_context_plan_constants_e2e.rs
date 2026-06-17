//! End-to-end tests for `session::get_session_context`
//! per-mode rendering + `PLAN_MODE_ALLOWED_TOOLS` catalog +
//! `MCP_TOOL_PREFIX` / `PLUGIN_TOOL_PREFIX` constants +
//! `UsageExtras` arithmetic.
//!
//! Sprint 107 of the verification effort. Sprint 39 covered
//! plan-mode entry; sprint 86 covered Session per-instance
//! methods; this file pins the session-context formatter
//! (Initializer vs Coding rendering) + the plan-mode
//! allowlist catalog + the documented tool-name prefixes.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::session::{
    get_session_context, Session, SessionMode, UsageExtras, MCP_TOOL_PREFIX,
    PLAN_MODE_ALLOWED_TOOLS, PLUGIN_TOOL_PREFIX,
};

// ───────────────────────────────────────────────────────────────────────────
// Section A — get_session_context per mode
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_session_context_initializer_mode_renders_initializer_label() {
    let s = Session::new_initializer();
    let ctx = get_session_context(&s);
    assert!(
        ctx.contains("Initializer Agent"),
        "Initializer mode MUST surface label; got {ctx:?}"
    );
}

#[test]
fn get_session_context_coding_mode_renders_coding_label() {
    let s = Session::new_coding("parent-id-xyz");
    let ctx = get_session_context(&s);
    assert!(
        ctx.contains("Coding Agent"),
        "Coding mode MUST surface label; got {ctx:?}"
    );
}

#[test]
fn get_session_context_coding_mode_includes_parent_session_id() {
    let s = Session::new_coding("parent-uuid-99");
    let ctx = get_session_context(&s);
    assert!(
        ctx.contains("parent-uuid-99"),
        "Coding mode MUST include parent_session_id; got {ctx:?}"
    );
}

#[test]
fn get_session_context_initializer_does_not_mention_previous_session() {
    let s = Session::new_initializer();
    let ctx = get_session_context(&s);
    assert!(
        !ctx.to_lowercase().contains("previous session"),
        "Initializer MUST NOT mention previous session; got {ctx:?}"
    );
}

#[test]
fn get_session_context_initializer_lists_5_documented_responsibilities() {
    let s = Session::new_initializer();
    let ctx = get_session_context(&s);
    // 5 numbered responsibilities per documented prompt.
    for n in &["1.", "2.", "3.", "4.", "5."] {
        assert!(
            ctx.contains(n),
            "Initializer MUST list responsibility {n}; got {ctx:?}"
        );
    }
}

#[test]
fn get_session_context_coding_lists_4_documented_responsibilities() {
    let s = Session::new_coding("p");
    let ctx = get_session_context(&s);
    for n in &["1.", "2.", "3.", "4."] {
        assert!(
            ctx.contains(n),
            "Coding MUST list responsibility {n}; got {ctx:?}"
        );
    }
}

#[test]
fn get_session_context_returns_non_empty_for_both_modes() {
    let init = Session::new_initializer();
    let cod = Session::new_coding("p");
    assert!(!get_session_context(&init).is_empty());
    assert!(!get_session_context(&cod).is_empty());
}

#[test]
fn get_session_context_mode_outputs_are_distinct() {
    let init = Session::new_initializer();
    let cod = Session::new_coding("p");
    assert_ne!(get_session_context(&init), get_session_context(&cod));
}

#[test]
fn get_session_context_starts_with_header_marker() {
    // Both modes use markdown header `## Session Context:`.
    let init = Session::new_initializer();
    let cod = Session::new_coding("p");
    assert!(get_session_context(&init).starts_with("## Session Context:"));
    assert!(get_session_context(&cod).starts_with("## Session Context:"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — PLAN_MODE_ALLOWED_TOOLS catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn plan_mode_allowed_tools_includes_documented_read_only_tools() {
    // Read-only inspection tools that plan mode permits.
    // (Authoring discovery: catalog includes read_file/list_files/grep/web_*
    // plus task/crosslink/bash_output/todo_read — but NOT glob.)
    for tool in &["read_file", "list_files", "grep", "web_fetch", "web_search"] {
        assert!(
            PLAN_MODE_ALLOWED_TOOLS.contains(tool),
            "{tool:?} MUST be in PLAN_MODE_ALLOWED_TOOLS"
        );
    }
}

#[test]
fn plan_mode_allowed_tools_excludes_mutation_tools() {
    // Mutation tools MUST NOT be in the plan-mode allowlist.
    for tool in &[
        "write_file",
        "edit_file",
        "bash",
        "kill_shell",
        "kill_shells_for_agent",
    ] {
        assert!(
            !PLAN_MODE_ALLOWED_TOOLS.contains(tool),
            "{tool:?} MUST NOT be in PLAN_MODE_ALLOWED_TOOLS (mutation)"
        );
    }
}

#[test]
fn plan_mode_allowed_tools_is_non_empty() {
    assert!(!PLAN_MODE_ALLOWED_TOOLS.is_empty());
}

#[test]
fn plan_mode_allowed_tools_entries_are_pairwise_distinct() {
    let mut seen = PLAN_MODE_ALLOWED_TOOLS.to_vec();
    let n = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        n,
        "PLAN_MODE_ALLOWED_TOOLS MUST have no duplicates; got {n} entries, {} unique",
        seen.len()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Tool name prefix constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn mcp_tool_prefix_is_documented_double_underscore_form() {
    assert_eq!(MCP_TOOL_PREFIX, "mcp__");
}

#[test]
fn plugin_tool_prefix_is_documented_double_underscore_form() {
    assert_eq!(PLUGIN_TOOL_PREFIX, "plugin__");
}

#[test]
fn mcp_and_plugin_prefixes_are_distinct() {
    assert_ne!(MCP_TOOL_PREFIX, PLUGIN_TOOL_PREFIX);
}

#[test]
fn prefix_constants_end_with_double_underscore() {
    assert!(MCP_TOOL_PREFIX.ends_with("__"));
    assert!(PLUGIN_TOOL_PREFIX.ends_with("__"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — UsageExtras arithmetic
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn usage_extras_default_is_zero() {
    let extras = UsageExtras::default();
    assert_eq!(extras.web_search_requests, 0);
}

#[test]
fn usage_extras_zero_constant_equals_default() {
    assert_eq!(UsageExtras::ZERO, UsageExtras::default());
}

#[test]
fn usage_extras_accumulate_sums_web_search_requests() {
    let mut a = UsageExtras {
        web_search_requests: 5,
    };
    let b = UsageExtras {
        web_search_requests: 3,
    };
    a.accumulate(&b);
    assert_eq!(a.web_search_requests, 8);
}

#[test]
fn usage_extras_accumulate_with_zero_is_identity() {
    let mut a = UsageExtras {
        web_search_requests: 7,
    };
    a.accumulate(&UsageExtras::ZERO);
    assert_eq!(a.web_search_requests, 7);
}

#[test]
fn usage_extras_partial_eq_holds_for_equal_values() {
    let a = UsageExtras {
        web_search_requests: 5,
    };
    let b = UsageExtras {
        web_search_requests: 5,
    };
    assert_eq!(a, b);
}

#[test]
fn usage_extras_partial_eq_distinguishes_different_values() {
    let a = UsageExtras {
        web_search_requests: 5,
    };
    let b = UsageExtras {
        web_search_requests: 6,
    };
    assert_ne!(a, b);
}

#[test]
fn usage_extras_is_copy() {
    let a = UsageExtras {
        web_search_requests: 99,
    };
    let copy = a;
    let again = a;
    assert_eq!(copy, again);
}

#[test]
fn usage_extras_serde_round_trips() {
    let original = UsageExtras {
        web_search_requests: 42,
    };
    let json = serde_json::to_string(&original).expect("ser");
    let back: UsageExtras = serde_json::from_str(&json).expect("de");
    assert_eq!(back, original);
}

#[test]
fn usage_extras_deserializes_from_empty_object_using_default() {
    // #[serde(default)] on web_search_requests.
    let back: UsageExtras = serde_json::from_str("{}").expect("de");
    assert_eq!(back, UsageExtras::default());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — SessionMode invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn session_new_initializer_mode_drives_initializer_context() {
    let s = Session::new_initializer();
    assert_eq!(s.mode, SessionMode::Initializer);
}

#[test]
fn session_new_coding_mode_drives_coding_context() {
    let s = Session::new_coding("p");
    assert_eq!(s.mode, SessionMode::Coding);
}
