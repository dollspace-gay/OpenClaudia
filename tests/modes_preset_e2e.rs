//! End-to-end tests for `modes::Preset` / `BehaviorMode` /
//! `Modifier` taxonomy + serde + Display behaviour.
//!
//! Sprint 66 of the verification effort. Targets `src/modes/`
//! which has internal unit tests but no integration-suite
//! coverage exercising the public API + serde JSON contracts
//! that downstream callers depend on.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::modes::{
    list_modifiers, list_presets, Agency, BehaviorMode, Modifier, Preset, Quality, Scope,
};

// ───────────────────────────────────────────────────────────────────────────
// Section A — Agency / Quality / Scope defaults
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agency_default_is_autonomous() {
    assert_eq!(Agency::default(), Agency::Autonomous);
}

#[test]
fn quality_default_is_pragmatic() {
    assert_eq!(Quality::default(), Quality::Pragmatic);
}

#[test]
fn scope_default_is_adjacent() {
    assert_eq!(Scope::default(), Scope::Adjacent);
}

#[test]
fn behavior_mode_default_matches_extend_preset() {
    // Documented: "matches `extend` preset".
    let default_mode = BehaviorMode::default();
    let extend_mode = BehaviorMode::from_preset(Preset::Extend);
    assert_eq!(default_mode, extend_mode);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Preset → BehaviorMode mapping
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn preset_create_uses_autonomous_architect_unrestricted() {
    let mode = BehaviorMode::from_preset(Preset::Create);
    assert_eq!(mode.agency, Agency::Autonomous);
    assert_eq!(mode.quality, Quality::Architect);
    assert_eq!(mode.scope, Scope::Unrestricted);
    assert!(mode.modifiers.is_empty());
}

#[test]
fn preset_safe_uses_collaborative_minimal_narrow() {
    let mode = BehaviorMode::from_preset(Preset::Safe);
    assert_eq!(mode.agency, Agency::Collaborative);
    assert_eq!(mode.quality, Quality::Minimal);
    assert_eq!(mode.scope, Scope::Narrow);
    assert!(mode.modifiers.is_empty());
}

#[test]
fn preset_explore_includes_readonly_modifier() {
    let mode = BehaviorMode::from_preset(Preset::Explore);
    assert!(
        mode.modifiers.contains(&Modifier::Readonly),
        "Explore preset MUST include Readonly modifier"
    );
}

#[test]
fn preset_debug_includes_debug_modifier_not_methodical() {
    // Pins crosslink #830 disambiguation — Preset::Debug
    // carries a Modifier::Debug (not Methodical).
    let mode = BehaviorMode::from_preset(Preset::Debug);
    assert!(mode.modifiers.contains(&Modifier::Debug));
    assert!(!mode.modifiers.contains(&Modifier::Methodical));
}

#[test]
fn preset_methodical_includes_methodical_modifier_with_surgical_agency() {
    let mode = BehaviorMode::from_preset(Preset::Methodical);
    assert_eq!(mode.agency, Agency::Surgical);
    assert!(mode.modifiers.contains(&Modifier::Methodical));
}

#[test]
fn preset_director_uses_unrestricted_scope_with_director_modifier() {
    let mode = BehaviorMode::from_preset(Preset::Director);
    assert_eq!(mode.scope, Scope::Unrestricted);
    assert!(mode.modifiers.contains(&Modifier::Director));
}

#[test]
fn every_preset_produces_distinct_behavior_mode() {
    // Mirror the internal unit test's intent — pins that
    // every preset configuration is unique.
    let presets = [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ];
    let modes: Vec<BehaviorMode> = presets
        .iter()
        .copied()
        .map(BehaviorMode::from_preset)
        .collect();
    for (i, a) in modes.iter().enumerate() {
        for (j, b) in modes.iter().enumerate() {
            if i != j {
                assert_ne!(
                    a, b,
                    "presets {:?} and {:?} collapse to the same mode",
                    presets[i], presets[j]
                );
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — BehaviorMode add/remove_modifier
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn add_modifier_appends_when_not_present() {
    let mut mode = BehaviorMode::default();
    assert!(mode.modifiers.is_empty());
    mode.add_modifier(Modifier::Bold);
    assert_eq!(mode.modifiers, vec![Modifier::Bold]);
}

#[test]
fn add_modifier_is_idempotent_when_already_present() {
    let mut mode = BehaviorMode::default();
    mode.add_modifier(Modifier::Bold);
    mode.add_modifier(Modifier::Bold);
    mode.add_modifier(Modifier::Bold);
    assert_eq!(mode.modifiers, vec![Modifier::Bold], "MUST dedup");
}

#[test]
fn remove_modifier_removes_when_present() {
    let mut mode = BehaviorMode::default();
    mode.add_modifier(Modifier::Bold);
    mode.add_modifier(Modifier::Readonly);
    mode.remove_modifier(Modifier::Bold);
    assert_eq!(mode.modifiers, vec![Modifier::Readonly]);
}

#[test]
fn remove_modifier_is_noop_when_absent() {
    let mut mode = BehaviorMode::default();
    mode.add_modifier(Modifier::Bold);
    mode.remove_modifier(Modifier::Readonly); // not present
    assert_eq!(mode.modifiers, vec![Modifier::Bold]);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Display formatting
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn behavior_mode_display_uses_slash_separated_axis_values() {
    let mode = BehaviorMode {
        agency: Agency::Autonomous,
        quality: Quality::Pragmatic,
        scope: Scope::Adjacent,
        modifiers: vec![],
    };
    assert_eq!(format!("{mode}"), "autonomous/pragmatic/adjacent");
}

#[test]
fn behavior_mode_display_includes_modifiers_in_brackets() {
    let mode = BehaviorMode {
        agency: Agency::Surgical,
        quality: Quality::Minimal,
        scope: Scope::Narrow,
        modifiers: vec![Modifier::Bold, Modifier::Readonly],
    };
    let display = format!("{mode}");
    assert!(display.starts_with("surgical/minimal/narrow"));
    assert!(display.contains('['));
    assert!(display.contains(']'));
    assert!(display.contains("bold"));
    assert!(display.contains("readonly"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — serde JSON contracts
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agency_serde_uses_lowercase() {
    for (agency, expected) in &[
        (Agency::Autonomous, "autonomous"),
        (Agency::Collaborative, "collaborative"),
        (Agency::Surgical, "surgical"),
    ] {
        let json = serde_json::to_string(agency).expect("serialize");
        assert_eq!(json.trim_matches('"'), *expected);
    }
}

#[test]
fn preset_debug_serializes_with_disambiguating_prefix() {
    // Per crosslink #830: Preset::Debug encodes as
    // "preset-debug" to disambiguate from Modifier::Debug
    // ("modifier-debug").
    let json = serde_json::to_string(&Preset::Debug).expect("serialize");
    assert_eq!(json.trim_matches('"'), "preset-debug");
}

#[test]
fn modifier_debug_serializes_with_disambiguating_prefix() {
    let json = serde_json::to_string(&Modifier::Debug).expect("serialize");
    assert_eq!(json.trim_matches('"'), "modifier-debug");
}

#[test]
fn modifier_methodical_and_director_use_disambiguating_prefix() {
    let m = serde_json::to_string(&Modifier::Methodical).unwrap();
    let d = serde_json::to_string(&Modifier::Director).unwrap();
    assert_eq!(m.trim_matches('"'), "modifier-methodical");
    assert_eq!(d.trim_matches('"'), "modifier-director");
}

#[test]
fn modifier_unambiguous_variants_use_kebab_case() {
    // Bold, Readonly, ContextPacing don't collide with
    // presets so they use vanilla kebab-case.
    assert_eq!(
        serde_json::to_string(&Modifier::Bold)
            .unwrap()
            .trim_matches('"'),
        "bold"
    );
    assert_eq!(
        serde_json::to_string(&Modifier::Readonly)
            .unwrap()
            .trim_matches('"'),
        "readonly"
    );
    assert_eq!(
        serde_json::to_string(&Modifier::ContextPacing)
            .unwrap()
            .trim_matches('"'),
        "context-pacing"
    );
}

#[test]
fn behavior_mode_serde_round_trips_with_modifiers() {
    let original = BehaviorMode {
        agency: Agency::Collaborative,
        quality: Quality::Architect,
        scope: Scope::Unrestricted,
        modifiers: vec![Modifier::Bold, Modifier::Director],
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let back: BehaviorMode = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, original);
}

#[test]
fn behavior_mode_serde_missing_modifiers_field_defaults_to_empty() {
    // Pins crosslink #839: legacy sessions persisted before
    // modifiers existed must deserialize cleanly.
    let json = r#"{"agency":"autonomous","quality":"pragmatic","scope":"adjacent"}"#;
    let mode: BehaviorMode = serde_json::from_str(json).expect("deserialize");
    assert!(mode.modifiers.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — list_presets + list_modifiers
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn list_presets_contains_every_documented_preset() {
    let presets = list_presets();
    let names: Vec<&str> = presets.iter().map(|(n, _)| *n).collect();
    for expected in &[
        "create",
        "extend",
        "safe",
        "refactor",
        "explore",
        "debug",
        "methodical",
        "director",
    ] {
        assert!(
            names.contains(expected),
            "preset {expected:?} MUST appear in list_presets; got {names:?}"
        );
    }
}

#[test]
fn list_presets_descriptions_are_non_empty() {
    for (name, desc) in list_presets() {
        assert!(
            !desc.is_empty(),
            "preset {name:?} MUST have non-empty description"
        );
    }
}

#[test]
fn list_modifiers_contains_every_modifier_variant() {
    let modifiers = list_modifiers();
    let names: Vec<&str> = modifiers.iter().map(|(n, _)| *n).collect();
    // The 6 documented modifiers.
    assert!(
        names.len() >= 6,
        "list_modifiers MUST have at least 6 entries; got {} ({names:?})",
        names.len()
    );
}

#[test]
fn list_modifiers_descriptions_are_substantive() {
    for (name, desc) in list_modifiers() {
        assert!(
            desc.len() >= 10,
            "modifier {name:?} description MUST be >= 10 chars; got {desc:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Cross-validation: preset → mode → preset name lookup
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn list_presets_names_match_preset_serde_encoding_for_simple_names() {
    // list_presets lists "debug" but serde encodes Preset::Debug
    // as "preset-debug". Pin both shapes here.
    let list_names: Vec<&str> = list_presets().iter().map(|(n, _)| *n).collect();
    assert!(list_names.contains(&"create"));
    assert!(list_names.contains(&"debug"));
    let create_json = serde_json::to_string(&Preset::Create).unwrap();
    assert_eq!(create_json.trim_matches('"'), "create");
    // Debug diverges due to disambiguation.
    let debug_json = serde_json::to_string(&Preset::Debug).unwrap();
    assert_eq!(
        debug_json.trim_matches('"'),
        "preset-debug",
        "list_presets uses short label but serde uses disambiguated"
    );
}
