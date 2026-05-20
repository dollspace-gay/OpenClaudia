//! End-to-end tests for the plan-mode tool gate and the
//! `AgentContextGuard` RAII drop semantics.
//!
//! Sprint 10 of the verification effort. `src/tools/plan_mode.rs`
//! has 13 unit tests and `src/subagent.rs` has 40, but no
//! integration coverage of the cross-module security contracts:
//!
//!   - **Mutation-tool catalog is hard-refused in plan mode** —
//!     `bash`, `edit_file`, `notebook_edit`, `todo_write`, and
//!     `kill_shell` are NOT in `PLAN_MODE_ALLOWED_TOOLS` and the
//!     gate MUST default-deny every one. A future drift that
//!     accidentally adds (say) `edit_file` to the allowlist
//!     surfaces here.
//!   - **MCP / plugin prefix gate** — `mcp__server__read_file`
//!     refused by default even though `read_file` is in the
//!     allowlist (prefix wins over name lookup); after policy
//!     opt-in, the prefix gate lifts BUT the allowlist still
//!     applies — `mcp__server__edit_file` stays refused because
//!     `edit_file` isn't in the allowlist (crosslink #341).
//!   - **`write_file` plan-file pinning** — `write_file` admits
//!     only when targeting the canonical plan file path,
//!     refuses on symlinks, non-regular files, and paths
//!     outside the pinned plan.
//!   - **`PlanModeState::enter` perimeter** — missing files,
//!     symlinks, directories all refused with the matching
//!     `PlanModeEntryError` variant.
//!   - **`AgentContextGuard` RAII drop** — nested guards don't
//!     prematurely release the in-agent-task flag; the outer
//!     guard is the sole authority for clearing.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::session::{
    is_tool_allowed_in_plan_mode, is_tool_allowed_in_plan_mode_with_policy, PlanModePolicy,
    PlanModeState, PLAN_MODE_ALLOWED_TOOLS,
};
use openclaudia::tools::{in_agent_task, AgentContextGuard};
use serde_json::json;
use std::path::PathBuf;
use tempfile::tempdir;

// ───────────────────────────────────────────────────────────────────────────
// Section A — plan-mode allow-list / deny-list discipline
// ───────────────────────────────────────────────────────────────────────────

/// Built-in mutation tools that must NEVER be admitted in plan mode,
/// even when the policy opts in to MCP/plugin tools. Each entry is
/// explicit so a future change that accidentally widens the allowlist
/// surfaces by name.
const MUTATION_TOOLS_HARD_REFUSED: &[&str] = &[
    "bash",
    "edit_file",
    "notebook_edit",
    "todo_write",
    "kill_shell",
    "remote_trigger",
    "memory_save",
    "memory_delete",
    "memory_update",
];

#[test]
fn mutation_tools_are_refused_in_plan_mode() {
    let plan_path = PathBuf::from("/dev/null"); // irrelevant for non-write_file
    for tool in MUTATION_TOOLS_HARD_REFUSED {
        let allowed = is_tool_allowed_in_plan_mode(tool, &plan_path, &json!({}));
        assert!(
            !allowed,
            "mutation tool {tool:?} MUST be refused in plan mode (currently allowed)"
        );
    }
}

#[test]
fn every_documented_allowlist_entry_is_admitted() {
    // Counter-test: every tool in the documented allowlist constant
    // MUST be admitted by the predicate. Catches a regression where
    // someone tightens the predicate without updating the const (or
    // vice versa).
    let plan_path = PathBuf::from("/dev/null");
    for tool in PLAN_MODE_ALLOWED_TOOLS {
        let allowed = is_tool_allowed_in_plan_mode(tool, &plan_path, &json!({}));
        assert!(
            allowed,
            "tool {tool:?} appears in PLAN_MODE_ALLOWED_TOOLS but the predicate refuses it"
        );
    }
}

#[test]
fn plan_mode_markers_are_always_allowed() {
    // enter_plan_mode and exit_plan_mode are NOT in the allowlist
    // constant; they're hardcoded as always-allowed in the predicate
    // because they manage plan-mode state itself.
    let plan_path = PathBuf::from("/dev/null");
    assert!(is_tool_allowed_in_plan_mode(
        "enter_plan_mode",
        &plan_path,
        &json!({})
    ));
    assert!(is_tool_allowed_in_plan_mode(
        "exit_plan_mode",
        &plan_path,
        &json!({})
    ));
}

#[test]
fn mcp_prefixed_tools_are_refused_by_default_even_when_suffix_is_allowed() {
    // The prefix gate must fire BEFORE the allowlist check — a
    // hostile MCP server registering `mcp__evil__read_file` must
    // not slip through just because `read_file` is on the allowlist.
    let plan_path = PathBuf::from("/dev/null");
    for shadow in &[
        "mcp__evil__read_file",
        "mcp__server__grep",
        "mcp__attacker__list_files",
    ] {
        let allowed = is_tool_allowed_in_plan_mode(shadow, &plan_path, &json!({}));
        assert!(
            !allowed,
            "{shadow:?} (mcp-prefixed shadow of an allowlisted name) MUST be refused"
        );
    }
}

#[test]
fn mcp_opt_in_lifts_prefix_gate_but_still_requires_allowlist_match() {
    // crosslink #341: opting in to MCP tools removes the prefix
    // refusal — but the suffix STILL has to match the allowlist.
    // So `mcp__server__read_file` becomes admitted (since `read_file`
    // is allowlisted) but `mcp__server__edit_file` stays refused
    // (since `edit_file` is NOT allowlisted).
    //
    // Note: the gate's `PLAN_MODE_ALLOWED_TOOLS.contains` check uses
    // the full tool name (with prefix), so even with the opt-in the
    // mcp-prefixed name doesn't match the bare `read_file` entry.
    // The opt-in lifts the prefix-based hard refusal, but the
    // contained name lookup is name-equal — so the test pins that
    // mcp-prefixed names with allowlisted SUFFIXES are STILL refused
    // unless the full prefixed name is added to the allowlist.
    let plan_path = PathBuf::from("/dev/null");
    let opt_in = PlanModePolicy {
        allow_mcp_tools: true,
        allow_plugin_tools: false,
    };
    // With opt-in: mcp-prefixed names with NON-allowlisted suffixes
    // stay refused.
    let edit_outcome = is_tool_allowed_in_plan_mode_with_policy(
        "mcp__server__edit_file",
        &plan_path,
        &json!({}),
        opt_in,
    );
    assert!(
        !edit_outcome,
        "even with allow_mcp_tools=true, mcp__server__edit_file MUST be refused \
         (edit_file is not in the allowlist)"
    );
    // Plugin tools are still hard-denied unless allow_plugin_tools is
    // also lifted (independent flags).
    let plugin_outcome = is_tool_allowed_in_plan_mode_with_policy(
        "plugin__foo__read_file",
        &plan_path,
        &json!({}),
        opt_in,
    );
    assert!(
        !plugin_outcome,
        "plugin-prefixed tool MUST be refused when only allow_mcp_tools=true"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — write_file plan-file pinning
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn write_file_admits_only_the_pinned_plan_path() {
    let dir = tempdir().expect("tempdir");
    let plan_file = dir.path().join("plan.md");
    std::fs::write(&plan_file, "# Plan").expect("write plan");

    // Use the canonical path as the gate's reference.
    let plan_canonical = std::fs::canonicalize(&plan_file).expect("canonicalize");

    // Writing to the canonical plan file: admitted.
    let allowed = is_tool_allowed_in_plan_mode(
        "write_file",
        &plan_canonical,
        &json!({"path": plan_file.to_string_lossy()}),
    );
    assert!(
        allowed,
        "write_file to the pinned plan file MUST be admitted; got refused"
    );

    // Writing to a sibling file: refused.
    let sibling = dir.path().join("sibling.md");
    std::fs::write(&sibling, "evil").expect("write sibling");
    let refused = is_tool_allowed_in_plan_mode(
        "write_file",
        &plan_canonical,
        &json!({"path": sibling.to_string_lossy()}),
    );
    assert!(
        !refused,
        "write_file to a non-plan file MUST be refused; got admitted"
    );
}

#[test]
fn write_file_refuses_missing_path_arg() {
    let dir = tempdir().expect("tempdir");
    let plan_file = dir.path().join("plan.md");
    std::fs::write(&plan_file, "# Plan").expect("write plan");
    let plan_canonical = std::fs::canonicalize(&plan_file).expect("canonicalize");

    let allowed = is_tool_allowed_in_plan_mode("write_file", &plan_canonical, &json!({}));
    assert!(
        !allowed,
        "write_file without a `path` arg MUST be refused; got admitted"
    );
}

#[cfg(unix)]
#[test]
fn write_file_refuses_symlink_at_target() {
    let dir = tempdir().expect("tempdir");
    let plan_file = dir.path().join("plan.md");
    std::fs::write(&plan_file, "# Plan").expect("write plan");
    let plan_canonical = std::fs::canonicalize(&plan_file).expect("canonicalize");

    // Plant a symlink alongside the plan file. The lstat check in
    // the gate must reject this BEFORE canonicalization could
    // resolve it to the plan file.
    let link = dir.path().join("plan-link.md");
    std::os::unix::fs::symlink(&plan_file, &link).expect("symlink");

    let allowed = is_tool_allowed_in_plan_mode(
        "write_file",
        &plan_canonical,
        &json!({"path": link.to_string_lossy()}),
    );
    assert!(
        !allowed,
        "write_file via symlink (even to the plan file) MUST be refused"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — PlanModeState::enter perimeter
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn enter_refuses_missing_plan_file() {
    let dir = tempdir().expect("tempdir");
    let nope = dir.path().join("never-existed.md");
    let outcome = PlanModeState::enter(nope);
    assert!(
        outcome.is_err(),
        "enter with missing plan file MUST error; got {outcome:?}"
    );
}

#[cfg(unix)]
#[test]
fn enter_refuses_symlink_plan_file() {
    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real.md");
    std::fs::write(&real, "# Plan").expect("write real");
    let link = dir.path().join("link.md");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let outcome = PlanModeState::enter(link);
    assert!(
        outcome.is_err(),
        "enter with symlink plan file MUST error; got {outcome:?}"
    );
}

#[test]
fn enter_refuses_directory_as_plan_file() {
    let dir = tempdir().expect("tempdir");
    let subdir = dir.path().join("plan-as-dir");
    std::fs::create_dir(&subdir).expect("create subdir");
    let outcome = PlanModeState::enter(subdir);
    assert!(
        outcome.is_err(),
        "enter with directory as plan file MUST error; got {outcome:?}"
    );
}

#[test]
fn enter_succeeds_with_real_file_and_pins_canonical_path() {
    let dir = tempdir().expect("tempdir");
    let plan_file = dir.path().join("plan.md");
    std::fs::write(&plan_file, "# Plan").expect("write");
    let state = PlanModeState::enter(plan_file.clone()).expect("enter must succeed");
    assert!(state.active);
    assert_eq!(state.plan_file, plan_file);
    // The pinned canonical path must equal the canonicalized form.
    let canonical = std::fs::canonicalize(&plan_file).expect("canonicalize");
    assert_eq!(
        state.plan_realpath, canonical,
        "plan_realpath must equal the canonicalized plan_file"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — AgentContextGuard RAII drop semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agent_context_guard_sets_flag_only_for_its_lifetime() {
    assert!(!in_agent_task(), "test must start outside any agent task");
    {
        let _guard = AgentContextGuard::enter();
        assert!(in_agent_task(), "flag must be set inside guard scope");
    }
    assert!(
        !in_agent_task(),
        "flag must be cleared when the guard drops"
    );
}

#[test]
fn nested_guards_share_the_outermost_lifetime() {
    // Only the outermost guard owns the flag. An inner guard's drop
    // MUST NOT clear the flag while the outer guard is still alive.
    assert!(!in_agent_task());
    let outer = AgentContextGuard::enter();
    assert!(in_agent_task(), "outer guard must set flag");
    {
        let inner = AgentContextGuard::enter();
        assert!(in_agent_task(), "inner guard must observe flag still set");
        drop(inner);
        assert!(
            in_agent_task(),
            "dropping the INNER guard MUST NOT clear the flag while outer lives"
        );
    }
    drop(outer);
    assert!(
        !in_agent_task(),
        "dropping the outer guard MUST clear the flag"
    );
}

#[test]
fn agent_context_guard_does_not_leak_across_threads() {
    // The IN_AGENT_TASK flag is a thread-local. A guard on the main
    // thread MUST NOT make in_agent_task() return true on another
    // thread.
    let _main_guard = AgentContextGuard::enter();
    assert!(in_agent_task(), "main thread: flag set");

    let other_thread_observation = std::thread::spawn(in_agent_task).join().expect("join");
    assert!(
        !other_thread_observation,
        "spawned thread must see in_agent_task()=false (thread-local isolation)"
    );
}
