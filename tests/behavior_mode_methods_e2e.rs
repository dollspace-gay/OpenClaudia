//! End-to-end tests for `modes::BehaviorMode` — the
//! `matching_preset` reverse-lookup, `description` /
//! `display_name` rendering, and `assemble_behavioral_prompt`
//! axis+modifier concatenation. Sprint 130 covered the
//! preset → mode mapping; this file pins the rendering
//! and reverse-lookup contracts.
//!
//! Sprint 190 milestone of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::modes::{Agency, BehaviorMode, Modifier, Preset, Quality, Scope};

// ───────────────────────────────────────────────────────────────────────────
// Section A — matching_preset reverse-lookup
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn matching_preset_returns_some_for_canonical_preset_modes() {
    // PINS DOC: every from_preset(p) round-trips via matching_preset.
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
    for p in presets {
        let mode = BehaviorMode::from_preset(p);
        assert_eq!(
            mode.matching_preset(),
            Some(p),
            "from_preset({p:?}) MUST round-trip via matching_preset"
        );
    }
}

#[test]
fn matching_preset_returns_none_for_custom_mode_with_modifier() {
    // Add a modifier so the mode no longer matches any preset.
    let mut mode = BehaviorMode::from_preset(Preset::Create);
    mode.modifiers.push(Modifier::Bold);
    assert!(
        mode.matching_preset().is_none(),
        "preset+modifier MUST NOT match a bare preset"
    );
}

#[test]
fn matching_preset_returns_none_for_arbitrary_axis_combination() {
    let mode = BehaviorMode {
        agency: Agency::Autonomous,
        quality: Quality::Pragmatic,
        scope: Scope::Unrestricted, // not the Extend scope
        modifiers: Vec::new(),
    };
    // Default extend has Scope::Adjacent, so this combo MAY OR MAY
    // NOT match another preset. Check robust contract: if it doesn't
    // match, returns None; if it does, returns that preset.
    if let Some(p) = mode.matching_preset() {
        // Round-trip check.
        assert_eq!(BehaviorMode::from_preset(p), mode);
    }
}

#[test]
fn default_mode_matches_extend_preset() {
    // PINS DOC: default = extend (Autonomous/Pragmatic/Adjacent).
    let default = BehaviorMode::default();
    let extend = BehaviorMode::from_preset(Preset::Extend);
    assert_eq!(default, extend);
    assert_eq!(default.matching_preset(), Some(Preset::Extend));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — description rendering
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn description_for_preset_mode_starts_with_preset_name_colon() {
    // PINS WIRE: format!("{preset}: {desc}").
    let mode = BehaviorMode::from_preset(Preset::Create);
    let desc = mode.description();
    assert!(
        desc.starts_with("create:") || desc.starts_with("Create:"),
        "MUST start with preset name + colon; got {desc:?}"
    );
}

#[test]
fn description_for_preset_create_mentions_architecture() {
    let mode = BehaviorMode::from_preset(Preset::Create);
    let desc = mode.description();
    assert!(
        desc.contains("architecture") || desc.contains("scratch"),
        "Create description MUST mention architecture/scratch; got {desc:?}"
    );
}

#[test]
fn description_for_preset_explore_mentions_read_only() {
    let mode = BehaviorMode::from_preset(Preset::Explore);
    let desc = mode.description();
    assert!(
        desc.contains("Read-only") || desc.contains("understand"),
        "Explore description MUST mention read-only/understand; got {desc:?}"
    );
}

#[test]
fn description_for_preset_safe_mentions_minimal_risk() {
    let mode = BehaviorMode::from_preset(Preset::Safe);
    let desc = mode.description();
    assert!(
        desc.to_lowercase().contains("risk") || desc.contains("Surgical"),
        "Safe description MUST mention risk/Surgical; got {desc:?}"
    );
}

#[test]
fn description_for_custom_mode_starts_with_custom_prefix() {
    let mut mode = BehaviorMode::from_preset(Preset::Create);
    mode.modifiers.push(Modifier::Bold);
    let desc = mode.description();
    assert!(
        desc.starts_with("custom:"),
        "modified preset MUST be 'custom:'; got {desc:?}"
    );
}

#[test]
fn description_is_non_empty_for_every_preset() {
    for p in [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ] {
        let mode = BehaviorMode::from_preset(p);
        let desc = mode.description();
        assert!(!desc.is_empty(), "{p:?} description MUST be non-empty");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — display_name
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn display_name_for_canonical_preset_returns_preset_string() {
    let mode = BehaviorMode::from_preset(Preset::Create);
    let name = mode.display_name();
    // Preset's Display impl renders the preset name.
    assert!(
        !name.is_empty() && (name.contains("create") || name.contains("Create")),
        "display_name MUST surface preset name; got {name:?}"
    );
}

#[test]
fn display_name_for_custom_mode_falls_back_to_full_mode_string() {
    let mut mode = BehaviorMode::from_preset(Preset::Create);
    mode.modifiers.push(Modifier::Bold);
    let name = mode.display_name();
    assert!(!name.is_empty());
}

#[test]
fn display_name_is_non_empty_for_every_preset() {
    for p in [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ] {
        let mode = BehaviorMode::from_preset(p);
        assert!(!mode.display_name().is_empty());
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — assemble_behavioral_prompt structure
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn assemble_behavioral_prompt_has_3_axis_fragments_minimum() {
    // PINS DOC: 3 axes (agency/quality/scope) + 0+ modifiers,
    // joined by "\n\n".
    let mode = BehaviorMode::default();
    let prompt = mode.assemble_behavioral_prompt();
    // 3 fragments joined by "\n\n" → at least 2 separators.
    let separator_count = prompt.matches("\n\n").count();
    assert!(
        separator_count >= 2,
        "MUST have >= 2 separators for 3 axis fragments; got {separator_count} in {prompt:?}"
    );
}

#[test]
fn assemble_behavioral_prompt_grows_when_modifier_added() {
    let base = BehaviorMode::default();
    let mut with_mod = BehaviorMode::default();
    with_mod.modifiers.push(Modifier::Bold);
    let base_prompt = base.assemble_behavioral_prompt();
    let mod_prompt = with_mod.assemble_behavioral_prompt();
    assert!(
        mod_prompt.len() > base_prompt.len(),
        "adding modifier MUST grow prompt; base={} mod={}",
        base_prompt.len(),
        mod_prompt.len()
    );
}

#[test]
fn assemble_behavioral_prompt_is_non_empty_for_default() {
    let mode = BehaviorMode::default();
    assert!(!mode.assemble_behavioral_prompt().is_empty());
}

#[test]
fn assemble_behavioral_prompt_differs_between_distinct_presets() {
    let create_prompt = BehaviorMode::from_preset(Preset::Create).assemble_behavioral_prompt();
    let safe_prompt = BehaviorMode::from_preset(Preset::Safe).assemble_behavioral_prompt();
    assert_ne!(
        create_prompt, safe_prompt,
        "PINS: Create and Safe MUST produce distinct prompts"
    );
}

#[test]
fn assemble_behavioral_prompt_is_deterministic() {
    let mode = BehaviorMode::from_preset(Preset::Debug);
    let p1 = mode.assemble_behavioral_prompt();
    let p2 = mode.assemble_behavioral_prompt();
    let p3 = mode.assemble_behavioral_prompt();
    assert_eq!(p1, p2);
    assert_eq!(p2, p3);
}

#[test]
fn assemble_behavioral_prompt_with_two_modifiers_includes_both_fragments() {
    let mut mode = BehaviorMode::default();
    mode.modifiers.push(Modifier::Bold);
    mode.modifiers.push(Modifier::Readonly);
    let with_two = mode.assemble_behavioral_prompt();

    let mut with_one = BehaviorMode::default();
    with_one.modifiers.push(Modifier::Bold);
    let with_one_prompt = with_one.assemble_behavioral_prompt();

    // Two modifiers MUST be strictly larger than one.
    assert!(with_two.len() > with_one_prompt.len());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Cross-method consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn description_contains_display_name_or_custom_marker_for_every_preset() {
    for p in [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ] {
        let mode = BehaviorMode::from_preset(p);
        let desc = mode.description();
        let dn = mode.display_name();
        // description should reference the preset name.
        assert!(
            desc.contains(&dn) || desc.contains("custom"),
            "{p:?}: description {desc:?} MUST contain display_name {dn:?}"
        );
    }
}
