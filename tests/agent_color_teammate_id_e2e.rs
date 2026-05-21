//! End-to-end tests for `coordinator::AgentColor` palette +
//! `for_index` round-robin + serde lowercase rendering, plus
//! `TeammateId` shape + uniqueness + `as_str` round-trip.
//!
//! Sprint 115 of the verification effort. Sprint 21 + 36
//! covered `Coordinator` / `TaskQueue` + `TeammateState`
//! transitions; this file pins the `AgentColor` palette
//! (CC parity rainbow), the `for_index` modulo round-robin,
//! and the `TeammateId` newtype shape.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::coordinator::{AgentColor, TeammateId};

// ───────────────────────────────────────────────────────────────────────────
// Section A — AgentColor::PALETTE shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agent_color_palette_has_exactly_7_entries() {
    assert_eq!(AgentColor::PALETTE.len(), 7);
}

#[test]
fn agent_color_palette_matches_documented_rainbow_order() {
    // PINS CC PARITY: rainbow order so transcripts in
    // both harnesses color-code identically.
    let expected = [
        AgentColor::Red,
        AgentColor::Orange,
        AgentColor::Yellow,
        AgentColor::Green,
        AgentColor::Blue,
        AgentColor::Indigo,
        AgentColor::Violet,
    ];
    assert_eq!(AgentColor::PALETTE, &expected);
}

#[test]
fn agent_color_palette_entries_are_pairwise_distinct() {
    let mut palette: Vec<AgentColor> = AgentColor::PALETTE.to_vec();
    let n = palette.len();
    palette.sort_by_key(|c| format!("{c:?}"));
    palette.dedup();
    assert_eq!(palette.len(), n);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — AgentColor::for_index round-robin
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn for_index_0_returns_red() {
    assert_eq!(AgentColor::for_index(0), AgentColor::Red);
}

#[test]
fn for_index_1_returns_orange() {
    assert_eq!(AgentColor::for_index(1), AgentColor::Orange);
}

#[test]
fn for_index_6_returns_violet() {
    assert_eq!(AgentColor::for_index(6), AgentColor::Violet);
}

#[test]
fn for_index_7_wraps_back_to_red() {
    // PINS ROUND-ROBIN: 7 % 7 = 0 → Red.
    assert_eq!(AgentColor::for_index(7), AgentColor::Red);
}

#[test]
fn for_index_14_wraps_back_to_red_via_double_modulo() {
    assert_eq!(AgentColor::for_index(14), AgentColor::Red);
}

#[test]
fn for_index_arbitrary_high_value_yields_documented_color() {
    // 100 % 7 = 2 → Yellow.
    assert_eq!(AgentColor::for_index(100), AgentColor::Yellow);
    // 99 % 7 = 1 → Orange.
    assert_eq!(AgentColor::for_index(99), AgentColor::Orange);
    // usize::MAX % 7: u64 max % 7 = ?
    let _ = AgentColor::for_index(usize::MAX);
}

#[test]
fn for_index_walks_through_full_palette_in_documented_order() {
    let walked: Vec<AgentColor> = (0..7).map(AgentColor::for_index).collect();
    assert_eq!(walked, AgentColor::PALETTE.to_vec());
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — AgentColor serde (lowercase)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agent_color_serializes_as_lowercase_strings() {
    let cases = [
        (AgentColor::Red, "red"),
        (AgentColor::Orange, "orange"),
        (AgentColor::Yellow, "yellow"),
        (AgentColor::Green, "green"),
        (AgentColor::Blue, "blue"),
        (AgentColor::Indigo, "indigo"),
        (AgentColor::Violet, "violet"),
    ];
    for (color, expected) in cases {
        let json = serde_json::to_string(&color).expect("ser");
        assert_eq!(json.trim_matches('"'), expected);
    }
}

#[test]
fn agent_color_deserializes_from_lowercase_strings() {
    let cases = [
        ("\"red\"", AgentColor::Red),
        ("\"orange\"", AgentColor::Orange),
        ("\"violet\"", AgentColor::Violet),
    ];
    for (input, expected) in cases {
        let parsed: AgentColor = serde_json::from_str(input).expect("de");
        assert_eq!(parsed, expected);
    }
}

#[test]
fn agent_color_rejects_uppercase_or_capitalized_strings() {
    assert!(serde_json::from_str::<AgentColor>("\"Red\"").is_err());
    assert!(serde_json::from_str::<AgentColor>("\"RED\"").is_err());
}

#[test]
fn agent_color_round_trips_all_7_variants() {
    for variant in AgentColor::PALETTE {
        let json = serde_json::to_string(variant).expect("ser");
        let back: AgentColor = serde_json::from_str(&json).expect("de");
        assert_eq!(back, *variant);
    }
}

#[test]
fn agent_color_is_copy_and_eq() {
    let c = AgentColor::Blue;
    let copy = c;
    let again = c;
    assert_eq!(copy, again);
    assert_eq!(c, AgentColor::Blue);
    assert_ne!(c, AgentColor::Red);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — TeammateId newtype
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn teammate_id_new_generates_non_empty_string() {
    let id = TeammateId::new();
    assert!(!id.as_str().is_empty());
}

#[test]
fn two_fresh_teammate_ids_are_distinct() {
    let a = TeammateId::new();
    let b = TeammateId::new();
    assert_ne!(a.as_str(), b.as_str());
}

#[test]
fn teammate_id_as_str_round_trip() {
    let id = TeammateId::new();
    let s = id.as_str().to_string();
    // Same id observed twice yields same string.
    assert_eq!(id.as_str(), s);
}

#[test]
fn teammate_id_clone_preserves_id_string() {
    let id = TeammateId::new();
    let cloned = id.clone();
    assert_eq!(cloned.as_str(), id.as_str());
}

#[test]
fn teammate_id_serde_round_trips() {
    let id = TeammateId::new();
    let json = serde_json::to_string(&id).expect("ser");
    let back: TeammateId = serde_json::from_str(&json).expect("de");
    assert_eq!(back.as_str(), id.as_str());
}

#[test]
fn teammate_id_partial_eq_distinguishes_distinct_ids() {
    let a = TeammateId::new();
    let b = TeammateId::new();
    assert_ne!(a, b);
}

#[test]
fn teammate_id_hash_supports_hashset_dedup() {
    use std::collections::HashSet;
    let a = TeammateId::new();
    let mut set = HashSet::new();
    set.insert(a.clone());
    set.insert(a.clone());
    set.insert(a);
    assert_eq!(set.len(), 1, "same id MUST dedup");
    set.insert(TeammateId::new());
    assert_eq!(set.len(), 2);
}

#[test]
fn teammate_id_default_constructs_via_new() {
    let _id = TeammateId::default();
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Pairing AgentColor + TeammateId in collections
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn agent_color_and_teammate_id_compose_in_hashmap() {
    use std::collections::HashMap;
    let mut map: HashMap<TeammateId, AgentColor> = HashMap::new();
    for i in 0..15 {
        let id = TeammateId::new();
        let color = AgentColor::for_index(i);
        map.insert(id, color);
    }
    assert_eq!(map.len(), 15);
}
