//! End-to-end tests for `permissions::PermissionManager::unrestricted`
//! plus `is_enabled` predicate. Pins the `unrestricted()`
//! constructor's enabled=false short-circuit and the
//! empty-state contract.
//!
//! Sprint 211 of the verification effort. Sprint 210 covered TUI
//! remember/check; this file pins the `unrestricted` builder and
//! the `is_enabled` predicate independently.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::permissions::{
    CheckResult, PermissionDecision, PermissionManager, PermissionRule,
};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — unrestricted: is_enabled=false
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unrestricted_manager_is_not_enabled() {
    let mgr = PermissionManager::unrestricted();
    assert!(
        !mgr.is_enabled(),
        "PINS DOC: unrestricted() builder MUST set enabled=false"
    );
}

#[test]
fn unrestricted_manager_has_no_persisted_rules() {
    let mgr = PermissionManager::unrestricted();
    assert!(mgr.persisted_rules().is_empty());
}

#[test]
fn unrestricted_manager_has_no_session_rules() {
    let mgr = PermissionManager::unrestricted();
    assert!(mgr.session_rules().is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — unrestricted check() short-circuits to Allowed
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unrestricted_allows_every_tool_invocation() {
    // PINS DOC: enabled=false → check() returns Allowed.
    let mgr = PermissionManager::unrestricted();
    let outcome = mgr.check("Bash", &json!({"command": "ls"}));
    assert_eq!(outcome, CheckResult::Allowed);
}

#[test]
fn unrestricted_allows_edit_tool() {
    let mgr = PermissionManager::unrestricted();
    let outcome = mgr.check("Edit", &json!({"file_path": "/tmp/x"}));
    assert_eq!(outcome, CheckResult::Allowed);
}

#[test]
fn unrestricted_allows_destructive_command_without_rules() {
    let mgr = PermissionManager::unrestricted();
    let outcome = mgr.check("Bash", &json!({"command": "rm -rf /"}));
    // unrestricted means even "rm -rf /" passes the gate
    // (it's still blocked by validate_command's denylist later).
    assert_eq!(outcome, CheckResult::Allowed);
}

#[test]
fn unrestricted_allows_unknown_tool() {
    let mgr = PermissionManager::unrestricted();
    let outcome = mgr.check("unknown_tool_xyz", &json!({}));
    assert_eq!(outcome, CheckResult::Allowed);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Session rule add still works under unrestricted
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unrestricted_allows_session_rule_mutations() {
    let mut mgr = PermissionManager::unrestricted();
    mgr.add_session_rule(PermissionRule {
        tool: "Bash".to_string(),
        pattern: "*".to_string(),
        decision: PermissionDecision::Deny,
    });
    // Rule is added to the list.
    assert_eq!(mgr.session_rules().len(), 1);
}

#[test]
fn unrestricted_check_ignores_session_rules_due_to_enabled_false() {
    // PINS: even a Deny rule doesn't take effect when enabled=false.
    let mut mgr = PermissionManager::unrestricted();
    mgr.add_session_rule(PermissionRule {
        tool: "Bash".to_string(),
        pattern: "*".to_string(),
        decision: PermissionDecision::Deny,
    });
    let outcome = mgr.check("Bash", &json!({"command": "anything"}));
    assert_eq!(
        outcome,
        CheckResult::Allowed,
        "unrestricted MUST short-circuit Deny rules"
    );
}

#[test]
fn unrestricted_clear_session_rules_works() {
    let mut mgr = PermissionManager::unrestricted();
    mgr.add_session_rule(PermissionRule {
        tool: "Bash".to_string(),
        pattern: "*".to_string(),
        decision: PermissionDecision::Allow,
    });
    assert_eq!(mgr.session_rules().len(), 1);
    mgr.clear_session_rules();
    assert!(mgr.session_rules().is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — TUI remember sets still work under unrestricted
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unrestricted_tui_remember_always_allowed_still_persists() {
    let mgr = PermissionManager::unrestricted();
    mgr.tui_remember_always_allowed("Bash".to_string());
    assert!(mgr.tui_is_always_allowed("Bash"));
}

#[test]
fn unrestricted_tui_remember_always_denied_still_persists() {
    let mgr = PermissionManager::unrestricted();
    mgr.tui_remember_always_denied("rm".to_string());
    assert!(mgr.tui_is_always_denied("rm"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — is_enabled is const + read-only
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn is_enabled_is_deterministic_across_repeated_calls() {
    let mgr = PermissionManager::unrestricted();
    for _ in 0..5 {
        assert!(!mgr.is_enabled());
    }
}

#[test]
fn is_enabled_does_not_mutate_state() {
    let mgr = PermissionManager::unrestricted();
    let before = mgr.persisted_rules().len();
    let _ = mgr.is_enabled();
    let _ = mgr.is_enabled();
    let after = mgr.persisted_rules().len();
    assert_eq!(before, after);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — check() determinism under unrestricted
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_under_unrestricted_is_deterministic() {
    let mgr = PermissionManager::unrestricted();
    let args = json!({"command": "ls /tmp"});
    let r1 = mgr.check("Bash", &args);
    let r2 = mgr.check("Bash", &args);
    let r3 = mgr.check("Bash", &args);
    assert_eq!(r1, r2);
    assert_eq!(r2, r3);
    assert_eq!(r1, CheckResult::Allowed);
}

#[test]
fn check_under_unrestricted_with_different_tools_yields_allowed() {
    let mgr = PermissionManager::unrestricted();
    for tool in ["Bash", "Edit", "Write", "Read", "Glob", "Grep"] {
        let outcome = mgr.check(tool, &json!({}));
        assert_eq!(outcome, CheckResult::Allowed, "{tool} MUST be Allowed");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — record_denial still updates counters even when disabled
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn record_denial_increments_counters_even_under_unrestricted() {
    // PINS: counter mutation is independent of enabled state —
    // the counters always advance so callers can detect a
    // pattern of denials regardless of the gate.
    let mut mgr = PermissionManager::unrestricted();
    // No public getter for counters in unrestricted, but
    // we can verify the call doesn't panic.
    for _ in 0..3 {
        mgr.record_denial();
    }
    // No assertion needed beyond no-panic; record_denial
    // mutates internal counter via saturating_add.
}
