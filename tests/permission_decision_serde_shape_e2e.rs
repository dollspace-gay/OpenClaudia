//! End-to-end tests for `permissions::PermissionDecision`
//! serde wire shape — `snake_case` enum tag including the
//! `always_allow` variant (NOT `alwaysAllow` or
//! `AlwaysAllow`), plus `PartialEq`/`Eq`/`Clone` derives.
//!
//! Sprint 207 of the verification effort. Sprint 50/etc.
//! covered `PermissionRule` semantics; this file pins the
//! decision enum's wire form directly.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::permissions::PermissionDecision;
use serde_json::{json, Value};

// ───────────────────────────────────────────────────────────────────────────
// Section A — snake_case serde for each variant
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn allow_serializes_as_lowercase_allow() {
    let v: Value = serde_json::to_value(PermissionDecision::Allow).expect("ser");
    assert_eq!(v, json!("allow"));
}

#[test]
fn deny_serializes_as_lowercase_deny() {
    let v: Value = serde_json::to_value(PermissionDecision::Deny).expect("ser");
    assert_eq!(v, json!("deny"));
}

#[test]
fn always_allow_serializes_with_snake_case_underscore() {
    // PINS WIRE: "always_allow" (snake_case, NOT camelCase).
    let v: Value = serde_json::to_value(PermissionDecision::AlwaysAllow).expect("ser");
    assert_eq!(
        v,
        json!("always_allow"),
        "PINS: AlwaysAllow → 'always_allow' snake_case wire"
    );
}

#[test]
fn allow_deserializes_from_lowercase_string() {
    let d: PermissionDecision = serde_json::from_value(json!("allow")).expect("de");
    assert_eq!(d, PermissionDecision::Allow);
}

#[test]
fn deny_deserializes_from_lowercase_string() {
    let d: PermissionDecision = serde_json::from_value(json!("deny")).expect("de");
    assert_eq!(d, PermissionDecision::Deny);
}

#[test]
fn always_allow_deserializes_from_snake_case() {
    let d: PermissionDecision = serde_json::from_value(json!("always_allow")).expect("de");
    assert_eq!(d, PermissionDecision::AlwaysAllow);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Strict-case rejection
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn camel_case_always_allow_rejected_on_deserialize() {
    // PINS: snake_case strict — "alwaysAllow" MUST NOT match.
    let outcome: Result<PermissionDecision, _> = serde_json::from_value(json!("alwaysAllow"));
    assert!(outcome.is_err());
}

#[test]
fn pascal_case_always_allow_rejected_on_deserialize() {
    let outcome: Result<PermissionDecision, _> = serde_json::from_value(json!("AlwaysAllow"));
    assert!(outcome.is_err());
}

#[test]
fn uppercase_allow_rejected_on_deserialize() {
    let outcome: Result<PermissionDecision, _> = serde_json::from_value(json!("ALLOW"));
    assert!(outcome.is_err());
}

#[test]
fn unknown_decision_rejected_on_deserialize() {
    let outcome: Result<PermissionDecision, _> = serde_json::from_value(json!("maybe"));
    assert!(outcome.is_err());
}

#[test]
fn empty_string_rejected_on_deserialize() {
    let outcome: Result<PermissionDecision, _> = serde_json::from_value(json!(""));
    assert!(outcome.is_err());
}

#[test]
fn always_allow_with_hyphen_rejected() {
    // PINS: kebab "always-allow" MUST NOT match snake_case.
    let outcome: Result<PermissionDecision, _> = serde_json::from_value(json!("always-allow"));
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn all_three_variants_round_trip_through_json() {
    for variant in [
        PermissionDecision::Allow,
        PermissionDecision::Deny,
        PermissionDecision::AlwaysAllow,
    ] {
        let json = serde_json::to_value(&variant).expect("ser");
        let back: PermissionDecision = serde_json::from_value(json).expect("de");
        assert_eq!(back, variant);
    }
}

#[test]
fn three_variants_have_distinct_wire_strings() {
    let a = serde_json::to_value(PermissionDecision::Allow).unwrap();
    let d = serde_json::to_value(PermissionDecision::Deny).unwrap();
    let aa = serde_json::to_value(PermissionDecision::AlwaysAllow).unwrap();
    assert_ne!(a, d);
    assert_ne!(d, aa);
    assert_ne!(a, aa);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — PartialEq + Eq + Clone
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn clone_preserves_variant() {
    let original = PermissionDecision::AlwaysAllow;
    let cloned = original.clone();
    assert_eq!(cloned, original);
}

#[test]
fn three_variants_pairwise_distinct_under_partial_eq() {
    assert_ne!(PermissionDecision::Allow, PermissionDecision::Deny);
    assert_ne!(PermissionDecision::Allow, PermissionDecision::AlwaysAllow);
    assert_ne!(PermissionDecision::Deny, PermissionDecision::AlwaysAllow);
}

#[test]
fn debug_format_includes_variant_name() {
    let d = format!("{:?}", PermissionDecision::AlwaysAllow);
    assert!(d.contains("AlwaysAllow"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Cross-format isolation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn always_allow_wire_contains_underscore_not_hyphen() {
    let v: Value = serde_json::to_value(PermissionDecision::AlwaysAllow).expect("ser");
    let wire = v.as_str().expect("string");
    assert!(wire.contains('_'), "MUST use underscore; got {wire:?}");
    assert!(!wire.contains('-'), "MUST NOT use hyphen; got {wire:?}");
}

#[test]
fn allow_and_always_allow_share_prefix_but_distinct() {
    let allow = serde_json::to_value(PermissionDecision::Allow)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let always = serde_json::to_value(PermissionDecision::AlwaysAllow)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    // always_allow CONTAINS allow (ends in "_allow") but isn't equal.
    assert!(always.contains(&allow));
    assert_ne!(allow, always);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Send + Sync
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn permission_decision_is_send_sync_for_arc_usage() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PermissionDecision>();
}
