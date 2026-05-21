//! End-to-end tests for `modes::fragments` single-source
//! catalog (`BASE_FRAGMENTS`, `AGENCY_FRAGMENTS`,
//! `QUALITY_FRAGMENTS`, `SCOPE_FRAGMENTS`, `MODIFIERS`)
//! plus per-variant accessor functions.
//!
//! Sprint 90 of the verification effort. The fragments
//! module is the single source of truth wiring each axis
//! variant to its embedded markdown content — drift between
//! the enum + the table is a panic at runtime via the
//! `expect()` in the accessors. This file pins the table
//! completeness + accessor lookup correctness.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::modes::fragments::{
    agency_fragment, modifier_fragment, quality_fragment, scope_fragment, ModifierEntry,
    AGENCY_FRAGMENTS, BASE_COMMS, BASE_FRAGMENTS, BASE_IDENTITY, BASE_PRINCIPLES, BASE_TOOLS,
    MODIFIERS, QUALITY_FRAGMENTS, SCOPE_FRAGMENTS,
};
use openclaudia::modes::{Agency, Modifier, Quality, Scope};

// ───────────────────────────────────────────────────────────────────────────
// Section A — BASE_FRAGMENTS catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn base_fragments_has_4_entries() {
    assert_eq!(BASE_FRAGMENTS.len(), 4);
}

#[test]
fn base_fragments_names_match_documented_constants() {
    let names: Vec<&str> = BASE_FRAGMENTS.iter().map(|(n, _)| *n).collect();
    assert!(names.contains(&"BASE_IDENTITY"));
    assert!(names.contains(&"BASE_TOOLS"));
    assert!(names.contains(&"BASE_PRINCIPLES"));
    assert!(names.contains(&"BASE_COMMS"));
}

#[test]
fn base_fragments_values_are_non_empty() {
    for (name, content) in BASE_FRAGMENTS {
        assert!(!content.is_empty(), "{name} fragment MUST be non-empty");
    }
}

#[test]
fn base_fragments_table_aligns_with_individual_consts() {
    // Pin the table entries against the individual pub
    // consts so refactors that update one but not the other
    // surface.
    let table: Vec<(&str, &str)> = BASE_FRAGMENTS.to_vec();
    assert!(table.contains(&("BASE_IDENTITY", BASE_IDENTITY)));
    assert!(table.contains(&("BASE_TOOLS", BASE_TOOLS)));
    assert!(table.contains(&("BASE_PRINCIPLES", BASE_PRINCIPLES)));
    assert!(table.contains(&("BASE_COMMS", BASE_COMMS)));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — AGENCY_FRAGMENTS catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agency_fragments_table_has_3_entries() {
    assert_eq!(AGENCY_FRAGMENTS.len(), 3);
}

#[test]
fn agency_fragments_covers_every_variant() {
    let variants: Vec<Agency> = AGENCY_FRAGMENTS.iter().map(|(a, _)| *a).collect();
    assert!(variants.contains(&Agency::Autonomous));
    assert!(variants.contains(&Agency::Collaborative));
    assert!(variants.contains(&Agency::Surgical));
}

#[test]
fn agency_fragment_accessor_returns_table_entry() {
    for (variant, content) in AGENCY_FRAGMENTS {
        let returned = agency_fragment(*variant);
        assert_eq!(
            returned, *content,
            "accessor MUST match table for {variant:?}"
        );
    }
}

#[test]
fn agency_fragments_are_non_empty_per_variant() {
    for variant in &[Agency::Autonomous, Agency::Collaborative, Agency::Surgical] {
        let fragment = agency_fragment(*variant);
        assert!(
            !fragment.is_empty(),
            "{variant:?} fragment MUST be non-empty"
        );
    }
}

#[test]
fn agency_fragments_per_variant_are_pairwise_distinct() {
    let a = agency_fragment(Agency::Autonomous);
    let b = agency_fragment(Agency::Collaborative);
    let s = agency_fragment(Agency::Surgical);
    assert_ne!(a, b, "Autonomous MUST differ from Collaborative");
    assert_ne!(b, s, "Collaborative MUST differ from Surgical");
    assert_ne!(a, s, "Autonomous MUST differ from Surgical");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — QUALITY_FRAGMENTS catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn quality_fragments_table_has_3_entries() {
    assert_eq!(QUALITY_FRAGMENTS.len(), 3);
}

#[test]
fn quality_fragments_covers_every_variant() {
    let variants: Vec<Quality> = QUALITY_FRAGMENTS.iter().map(|(q, _)| *q).collect();
    assert!(variants.contains(&Quality::Architect));
    assert!(variants.contains(&Quality::Pragmatic));
    assert!(variants.contains(&Quality::Minimal));
}

#[test]
fn quality_fragment_accessor_returns_table_entry() {
    for (variant, content) in QUALITY_FRAGMENTS {
        let returned = quality_fragment(*variant);
        assert_eq!(returned, *content);
    }
}

#[test]
fn quality_fragments_per_variant_are_pairwise_distinct() {
    let a = quality_fragment(Quality::Architect);
    let p = quality_fragment(Quality::Pragmatic);
    let m = quality_fragment(Quality::Minimal);
    assert_ne!(a, p);
    assert_ne!(p, m);
    assert_ne!(a, m);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — SCOPE_FRAGMENTS catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn scope_fragments_table_has_3_entries() {
    assert_eq!(SCOPE_FRAGMENTS.len(), 3);
}

#[test]
fn scope_fragments_covers_every_variant() {
    let variants: Vec<Scope> = SCOPE_FRAGMENTS.iter().map(|(s, _)| *s).collect();
    assert!(variants.contains(&Scope::Unrestricted));
    assert!(variants.contains(&Scope::Adjacent));
    assert!(variants.contains(&Scope::Narrow));
}

#[test]
fn scope_fragment_accessor_returns_table_entry() {
    for (variant, content) in SCOPE_FRAGMENTS {
        let returned = scope_fragment(*variant);
        assert_eq!(returned, *content);
    }
}

#[test]
fn scope_fragments_per_variant_are_pairwise_distinct() {
    let u = scope_fragment(Scope::Unrestricted);
    let a = scope_fragment(Scope::Adjacent);
    let n = scope_fragment(Scope::Narrow);
    assert_ne!(u, a);
    assert_ne!(a, n);
    assert_ne!(u, n);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — MODIFIERS catalog (ModifierEntry)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn modifiers_table_has_6_entries() {
    assert_eq!(MODIFIERS.len(), 6);
}

#[test]
fn modifiers_covers_every_variant() {
    let variants: Vec<Modifier> = MODIFIERS.iter().map(|e| e.variant).collect();
    for expected in &[
        Modifier::Bold,
        Modifier::Debug,
        Modifier::Methodical,
        Modifier::Director,
        Modifier::Readonly,
        Modifier::ContextPacing,
    ] {
        assert!(
            variants.contains(expected),
            "{expected:?} MUST be in MODIFIERS table"
        );
    }
}

#[test]
fn modifier_entry_names_match_display_form() {
    // Documented contract: ModifierEntry.name matches the
    // variant's Display form (kebab-case).
    for entry in MODIFIERS {
        let display = entry.variant.to_string();
        assert_eq!(
            entry.name, display,
            "MODIFIERS.name {:?} MUST equal Display {display:?} for {:?}",
            entry.name, entry.variant
        );
    }
}

#[test]
fn modifier_entry_names_are_pairwise_distinct() {
    let mut names: Vec<&str> = MODIFIERS.iter().map(|e| e.name).collect();
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), n, "modifier names MUST be pairwise distinct");
}

#[test]
fn modifier_entry_fragments_non_empty() {
    for entry in MODIFIERS {
        assert!(
            !entry.fragment.is_empty(),
            "{:?} fragment MUST be non-empty",
            entry.variant
        );
    }
}

#[test]
fn modifier_entry_descriptions_non_empty_and_substantive() {
    for entry in MODIFIERS {
        assert!(
            !entry.description.is_empty(),
            "{:?} description MUST be non-empty",
            entry.variant
        );
        assert!(
            entry.description.len() >= 10,
            "{:?} description MUST be substantive (>= 10 chars); got {:?}",
            entry.variant,
            entry.description
        );
    }
}

#[test]
fn modifier_fragment_accessor_returns_table_fragment() {
    for entry in MODIFIERS {
        let returned = modifier_fragment(entry.variant);
        assert_eq!(returned, entry.fragment);
    }
}

#[test]
fn modifier_fragments_per_variant_are_pairwise_distinct() {
    // Pinning that no two modifier fragments accidentally
    // point at the same embedded file.
    let fragments: Vec<&'static str> = MODIFIERS.iter().map(|e| e.fragment).collect();
    let mut sorted = fragments.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        fragments.len(),
        "modifier fragments MUST be pairwise distinct (no aliasing); got {} unique of {}",
        sorted.len(),
        fragments.len()
    );
}

#[test]
fn modifier_entry_is_copy() {
    // Documented derive: ModifierEntry is Copy + Clone.
    let entry: ModifierEntry = MODIFIERS[0];
    let copy = entry;
    // Verify entry remains usable after the implicit copy
    // (proves the Copy bound is genuine).
    let again = entry;
    assert_eq!(copy.variant, entry.variant);
    assert_eq!(again.variant, entry.variant);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Cross-axis sanity: no fragment is shared across axes
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn no_agency_fragment_collides_with_a_quality_fragment() {
    for (_, a_frag) in AGENCY_FRAGMENTS {
        for (_, q_frag) in QUALITY_FRAGMENTS {
            assert_ne!(
                a_frag, q_frag,
                "agency fragment MUST NOT match quality fragment (axis isolation)"
            );
        }
    }
}

#[test]
fn no_modifier_fragment_collides_with_an_axis_fragment() {
    for entry in MODIFIERS {
        for (_, a_frag) in AGENCY_FRAGMENTS {
            assert_ne!(entry.fragment, *a_frag);
        }
        for (_, q_frag) in QUALITY_FRAGMENTS {
            assert_ne!(entry.fragment, *q_frag);
        }
        for (_, s_frag) in SCOPE_FRAGMENTS {
            assert_ne!(entry.fragment, *s_frag);
        }
    }
}
