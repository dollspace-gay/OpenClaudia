//! End-to-end tests for `modes::Preset` serde rename
//! attributes — the #830 disambiguation that prefixes
//! Debug/Methodical/Director with "preset-" to avoid
//! collision with the same-named `Modifier` variants.
//!
//! Sprint 195 of the verification effort. Sprint 194
//! pinned the `modifier-*` half; this file pins the
//! matching `preset-*` half. Together they let a config
//! carry both presets and modifiers in the same document
//! with no ambiguity.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::modes::Preset;
use serde_json::{json, Value};

// ───────────────────────────────────────────────────────────────────────────
// Section A — preset-* rename for overlapping variants (#830)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn preset_debug_serializes_with_preset_prefix() {
    // PINS #830: "preset-debug" wire (NOT "debug").
    let p = Preset::Debug;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("preset-debug"));
}

#[test]
fn preset_methodical_serializes_with_preset_prefix() {
    let p = Preset::Methodical;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("preset-methodical"));
}

#[test]
fn preset_director_serializes_with_preset_prefix() {
    let p = Preset::Director;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("preset-director"));
}

#[test]
fn preset_debug_deserializes_from_preset_prefix() {
    let json = json!("preset-debug");
    let p: Preset = serde_json::from_value(json).expect("de");
    assert_eq!(p, Preset::Debug);
}

#[test]
fn preset_methodical_deserializes_from_preset_prefix() {
    let json = json!("preset-methodical");
    let p: Preset = serde_json::from_value(json).expect("de");
    assert_eq!(p, Preset::Methodical);
}

#[test]
fn preset_director_deserializes_from_preset_prefix() {
    let json = json!("preset-director");
    let p: Preset = serde_json::from_value(json).expect("de");
    assert_eq!(p, Preset::Director);
}

#[test]
fn preset_bare_debug_without_prefix_rejected_on_deserialize() {
    // PINS #830 SAFETY: bare "debug" MUST NOT deserialize to
    // Preset (creates ambiguity with Modifier::Debug).
    let json = json!("debug");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(
        outcome.is_err(),
        "bare 'debug' MUST NOT match Preset::Debug"
    );
}

#[test]
fn preset_bare_methodical_without_prefix_rejected() {
    let json = json!("methodical");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn preset_bare_director_without_prefix_rejected() {
    let json = json!("director");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Non-overlapping variants use bare lowercase
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn preset_create_serializes_as_bare_lowercase() {
    let p = Preset::Create;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("create"));
}

#[test]
fn preset_extend_serializes_as_bare_lowercase() {
    let p = Preset::Extend;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("extend"));
}

#[test]
fn preset_safe_serializes_as_bare_lowercase() {
    let p = Preset::Safe;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("safe"));
}

#[test]
fn preset_refactor_serializes_as_bare_lowercase() {
    let p = Preset::Refactor;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("refactor"));
}

#[test]
fn preset_explore_serializes_as_bare_lowercase() {
    let p = Preset::Explore;
    let json: Value = serde_json::to_value(p).expect("ser");
    assert_eq!(json, json!("explore"));
}

#[test]
fn preset_create_deserializes_from_bare_lowercase() {
    let json = json!("create");
    let p: Preset = serde_json::from_value(json).expect("de");
    assert_eq!(p, Preset::Create);
}

#[test]
fn preset_explore_deserializes_from_bare_lowercase() {
    let json = json!("explore");
    let p: Preset = serde_json::from_value(json).expect("de");
    assert_eq!(p, Preset::Explore);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Round-trip across all 8 variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn all_eight_presets_round_trip_through_json() {
    let all = [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ];
    for p in all {
        let json = serde_json::to_value(p).expect("ser");
        let back: Preset = serde_json::from_value(json).expect("de");
        assert_eq!(back, p, "round-trip failed for {p:?}");
    }
}

#[test]
fn all_eight_presets_have_distinct_wire_names() {
    let names: Vec<String> = [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ]
    .iter()
    .map(|p| {
        serde_json::to_value(p)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    })
    .collect();
    let mut sorted = names;
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 8, "MUST have 8 distinct wire names");
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Cross-shape isolation (Preset vs Modifier)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn preset_and_modifier_overlapping_variants_have_distinct_wire_forms() {
    use openclaudia::modes::Modifier;
    let pairs: &[(Preset, Modifier)] = &[
        (Preset::Debug, Modifier::Debug),
        (Preset::Methodical, Modifier::Methodical),
        (Preset::Director, Modifier::Director),
    ];
    for (preset, modifier) in pairs {
        let p_json = serde_json::to_value(preset).expect("ser preset");
        let m_json = serde_json::to_value(modifier).expect("ser modifier");
        assert_ne!(
            p_json, m_json,
            "PINS #830: {preset:?} and {modifier:?} MUST serialize distinctly"
        );
    }
}

#[test]
fn preset_debug_wire_starts_with_preset_prefix() {
    let json = serde_json::to_value(Preset::Debug).expect("ser");
    let wire = json.as_str().expect("string");
    assert!(
        wire.starts_with("preset-"),
        "preset overlap MUST use 'preset-' prefix; got {wire:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Robustness
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn preset_unknown_string_rejected() {
    let json = json!("not_a_preset_xyz");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn preset_empty_string_rejected() {
    let json = json!("");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn preset_uppercase_rejected_strict_serde() {
    // PINS DOC: serde rename_all = "lowercase" is strict.
    let json = json!("CREATE");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn preset_mixed_case_rejected() {
    let json = json!("Create");
    let outcome: Result<Preset, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Cross-method consistency (Display vs serde)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn preset_serde_form_matches_documented_5_non_overlapping_names() {
    // PINS DOC: The 5 non-overlapping presets are
    // {create, extend, safe, refactor, explore} — all bare
    // lowercase.
    let cases = [
        (Preset::Create, "create"),
        (Preset::Extend, "extend"),
        (Preset::Safe, "safe"),
        (Preset::Refactor, "refactor"),
        (Preset::Explore, "explore"),
    ];
    for (variant, expected) in cases {
        let wire = serde_json::to_value(variant)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(wire, expected);
    }
}
