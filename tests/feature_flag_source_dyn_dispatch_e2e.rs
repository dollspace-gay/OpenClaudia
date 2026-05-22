//! End-to-end tests for `services::FeatureFlagSource` trait
//! dyn dispatch + `StaticFlags::with` chaining + custom
//! trait-object impl via test double.
//!
//! Sprint 221 of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::services::{FeatureFlagSource, StaticFlags};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Test-only flag source that always answers based on a fixed table.
struct FixedFlags {
    table: HashMap<String, bool>,
}

impl FeatureFlagSource for FixedFlags {
    fn is_enabled(&self, name: &str) -> bool {
        self.table.get(name).copied().unwrap_or(false)
    }
}

/// Test-only counting flag source — records each query.
struct CountingFlags {
    calls: Mutex<Vec<String>>,
}

impl FeatureFlagSource for CountingFlags {
    fn is_enabled(&self, name: &str) -> bool {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(name.to_string());
        false
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — StaticFlags::with chain
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn with_chain_sets_first_flag() {
    let flags = StaticFlags::new().with("alpha", true);
    assert!(flags.is_enabled("alpha"));
}

#[test]
fn with_chain_does_not_set_unmentioned_flags() {
    let flags = StaticFlags::new().with("alpha", true);
    assert!(!flags.is_enabled("beta"));
}

#[test]
fn with_chain_can_set_multiple_flags() {
    let flags = StaticFlags::new()
        .with("a", true)
        .with("b", true)
        .with("c", false);
    assert!(flags.is_enabled("a"));
    assert!(flags.is_enabled("b"));
    assert!(!flags.is_enabled("c"));
}

#[test]
fn with_chain_later_set_wins_for_same_key() {
    // PINS: later .with() call overwrites earlier value.
    let flags = StaticFlags::new().with("x", true).with("x", false);
    assert!(!flags.is_enabled("x"));
}

#[test]
fn with_chain_returns_owned_self_for_chaining() {
    // PINS: with consumes + returns Self for fluent chains.
    let _: StaticFlags = StaticFlags::new().with("a", true).with("b", true);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Custom FeatureFlagSource impl via trait object
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn custom_fixed_flags_via_trait_object_dispatch() {
    let mut table = HashMap::new();
    table.insert("custom_flag".to_string(), true);
    let src: Arc<dyn FeatureFlagSource> = Arc::new(FixedFlags { table });
    assert!(src.is_enabled("custom_flag"));
    assert!(!src.is_enabled("unknown"));
}

#[test]
fn custom_counting_flags_records_each_query() {
    let src: Arc<CountingFlags> = Arc::new(CountingFlags {
        calls: Mutex::new(Vec::new()),
    });
    src.is_enabled("a");
    src.is_enabled("b");
    src.is_enabled("c");
    let calls: Vec<String> = {
        let guard = src
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clone()
    };
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], "a");
    assert_eq!(calls[1], "b");
    assert_eq!(calls[2], "c");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Arc<dyn> cross-thread dispatch
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn arc_dyn_feature_flag_source_send_across_threads() {
    let mut table = HashMap::new();
    table.insert("a".to_string(), true);
    let src: Arc<dyn FeatureFlagSource> = Arc::new(FixedFlags { table });
    let s1 = Arc::clone(&src);
    let s2 = Arc::clone(&src);
    let h1 = std::thread::spawn(move || s1.is_enabled("a"));
    let h2 = std::thread::spawn(move || s2.is_enabled("unknown"));
    assert!(h1.join().unwrap());
    assert!(!h2.join().unwrap());
}

#[test]
fn static_flags_through_arc_dyn_dispatch() {
    let flags = StaticFlags::new().with("via_dyn", true);
    let src: Arc<dyn FeatureFlagSource> = Arc::new(flags);
    assert!(src.is_enabled("via_dyn"));
}

#[test]
fn dyn_feature_flag_source_send_sync_via_trait_bound() {
    // PINS DOC: FeatureFlagSource: Send + Sync trait bound.
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn FeatureFlagSource>();
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — StaticFlags is Send + Sync + Clone
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn static_flags_is_send_sync_for_arc_dispatch() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StaticFlags>();
}

#[test]
fn static_flags_clone_preserves_set_values() {
    let original = StaticFlags::new().with("flag-marker", true);
    let cloned = original.clone();
    assert!(cloned.is_enabled("flag-marker"));
    // Original still usable too.
    assert!(original.is_enabled("flag-marker"));
}

#[test]
fn static_flags_clone_independent_after_drop_of_one() {
    let original = StaticFlags::new().with("x", true);
    let cloned = original.clone();
    drop(original);
    // Cloned still usable.
    assert!(cloned.is_enabled("x"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — set() mutation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn set_method_mutates_in_place() {
    let mut flags = StaticFlags::new();
    flags.set("dynamic", true);
    assert!(flags.is_enabled("dynamic"));
}

#[test]
fn set_overwrites_previous_value() {
    let mut flags = StaticFlags::new();
    flags.set("flip", true);
    flags.set("flip", false);
    assert!(!flags.is_enabled("flip"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — is_enabled default false for unset flag
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unset_flag_returns_false_default() {
    let flags = StaticFlags::new();
    assert!(!flags.is_enabled("not_set"));
    assert!(!flags.is_enabled(""));
    assert!(!flags.is_enabled("anything_at_all_xyz_221"));
}

#[test]
fn is_enabled_is_pure_does_not_mutate_state() {
    let flags = StaticFlags::new().with("a", true);
    for _ in 0..50 {
        let _ = flags.is_enabled("a");
        let _ = flags.is_enabled("b");
    }
    // After 100 queries, state is unchanged.
    assert!(flags.is_enabled("a"));
    assert!(!flags.is_enabled("b"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Determinism
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn is_enabled_is_deterministic_across_repeated_calls() {
    let flags = StaticFlags::new().with("det", true);
    for _ in 0..10 {
        assert!(flags.is_enabled("det"));
    }
}
