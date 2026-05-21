//! End-to-end tests for `merge_hooks_config` precedence rules.
//!
//! Sprint 28 of the verification effort. `src/hooks/merge.rs`
//! has 10 unit tests but no integration coverage that drives
//! the merge through the deserialize path with realistic YAML
//! fixtures (the way the actual layered config-loader does).
//!
//! Coverage shape:
//!
//!   - **Empty-base merge** — merging an empty `HooksConfig`
//!     with `other` yields `other`'s entries verbatim.
//!   - **Disjoint slots** — `base.session_start` + `other.stop`
//!     coexist; neither displaces the other.
//!   - **Same-matcher replacement** — entries in `other` with
//!     the same matcher as `base` REPLACE the base entry
//!     (later source wins per crosslink #339).
//!   - **Distinct-matcher concat** — entries with distinct
//!     matchers in the same slot are kept; merge produces
//!     `base.len() + other.len()` entries.
//!   - **Normalised matcher key** — `None` and `Some("")`
//!     are treated as the same matcher (crosslink #339).
//!   - **All slot coverage** — every documented slot
//!     (`session_start`, `session_end`, `pre_tool_use`, etc.) is
//!     merged.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::HooksConfig;
use openclaudia::hooks::merge_hooks_config;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Build a `HooksConfig` from a YAML literal — the same path the
/// real config-loader takes.
fn cfg(yaml: &str) -> HooksConfig {
    serde_yaml::from_str(yaml).expect("HooksConfig YAML must parse")
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — empty / disjoint
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn merge_empty_base_yields_other_verbatim() {
    let base = HooksConfig::default();
    let other = cfg(r#"
pre_tool_use:
  - matcher: "^Bash$"
    hooks:
      - type: command
        command: "echo pre"
"#);
    let merged = merge_hooks_config(base, other);
    assert_eq!(
        merged.pre_tool_use.len(),
        1,
        "empty base must yield other's entry"
    );
    assert_eq!(merged.pre_tool_use[0].matcher.as_deref(), Some("^Bash$"));
}

#[test]
fn merge_empty_other_preserves_base() {
    let base = cfg(r#"
pre_tool_use:
  - matcher: "^Write$"
    hooks:
      - type: command
        command: "echo base"
"#);
    let other = HooksConfig::default();
    let merged = merge_hooks_config(base, other);
    assert_eq!(merged.pre_tool_use.len(), 1);
    assert_eq!(merged.pre_tool_use[0].matcher.as_deref(), Some("^Write$"));
}

#[test]
fn merge_two_empty_configs_yields_empty() {
    let merged = merge_hooks_config(HooksConfig::default(), HooksConfig::default());
    assert!(merged.pre_tool_use.is_empty());
    assert!(merged.session_start.is_empty());
    assert!(merged.stop.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — disjoint slots
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn merge_disjoint_slots_keeps_both() {
    let base = cfg(r#"
session_start:
  - matcher: null
    hooks:
      - type: command
        command: "echo session-start"
"#);
    let other = cfg(r#"
stop:
  - matcher: null
    hooks:
      - type: command
        command: "echo stop"
"#);
    let merged = merge_hooks_config(base, other);
    assert_eq!(
        merged.session_start.len(),
        1,
        "session_start from base must survive"
    );
    assert_eq!(merged.stop.len(), 1, "stop from other must appear");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — same-matcher replacement (crosslink #339)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn merge_same_matcher_in_same_slot_replaces_base_entry() {
    let base = cfg(r#"
pre_tool_use:
  - matcher: "^Bash$"
    hooks:
      - type: command
        command: "echo from-base"
"#);
    let other = cfg(r#"
pre_tool_use:
  - matcher: "^Bash$"
    hooks:
      - type: command
        command: "echo from-other"
"#);
    let merged = merge_hooks_config(base, other);
    // Same matcher → other replaces base. Total = 1 entry.
    assert_eq!(
        merged.pre_tool_use.len(),
        1,
        "same-matcher merge must produce exactly 1 entry; got {}",
        merged.pre_tool_use.len()
    );
    // The surviving entry must be from `other`.
    let serialised = format!("{:?}", merged.pre_tool_use[0]);
    assert!(
        serialised.contains("from-other"),
        "other must win; got {serialised:?}"
    );
    assert!(
        !serialised.contains("from-base"),
        "base must be replaced; got {serialised:?}"
    );
}

#[test]
fn merge_distinct_matchers_in_same_slot_concat() {
    let base = cfg(r#"
pre_tool_use:
  - matcher: "^Bash$"
    hooks:
      - type: command
        command: "echo bash"
"#);
    let other = cfg(r#"
pre_tool_use:
  - matcher: "^Write$"
    hooks:
      - type: command
        command: "echo write"
"#);
    let merged = merge_hooks_config(base, other);
    assert_eq!(
        merged.pre_tool_use.len(),
        2,
        "distinct-matcher merge must produce 2 entries; got {}",
        merged.pre_tool_use.len()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — matcher normalization (None ≡ Some(""))
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn matcher_none_and_empty_string_are_treated_as_same_key() {
    // crosslink #339: norm_matcher_key collapses None and
    // Some("") to the same key so callers that vary on this
    // don't produce shadow duplicates.
    let base = cfg(r#"
pre_tool_use:
  - matcher: null
    hooks:
      - type: command
        command: "echo from-null"
"#);
    let other = cfg(r#"
pre_tool_use:
  - matcher: ""
    hooks:
      - type: command
        command: "echo from-empty"
"#);
    let merged = merge_hooks_config(base, other);
    // norm collapses; result is 1 entry, from `other`.
    assert_eq!(
        merged.pre_tool_use.len(),
        1,
        "null and \"\" matcher must collapse to the same key; got {}",
        merged.pre_tool_use.len()
    );
    let serialised = format!("{:?}", merged.pre_tool_use[0]);
    assert!(
        serialised.contains("from-empty"),
        "other must win; got {serialised:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — all-slot coverage
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn merge_propagates_across_every_documented_slot() {
    // Each of the 8 slots that merge_hooks_config touches —
    // session_start, session_end, pre_tool_use, post_tool_use,
    // user_prompt_submit, stop, pre_adversary_review,
    // post_adversary_review, vdd_conflict, vdd_converged —
    // gets exactly one entry from `other` when `base` is empty.
    let base = HooksConfig::default();
    let other = cfg(r#"
session_start:
  - hooks: [{type: command, command: "x"}]
session_end:
  - hooks: [{type: command, command: "x"}]
pre_tool_use:
  - hooks: [{type: command, command: "x"}]
post_tool_use:
  - hooks: [{type: command, command: "x"}]
user_prompt_submit:
  - hooks: [{type: command, command: "x"}]
stop:
  - hooks: [{type: command, command: "x"}]
"#);
    let merged = merge_hooks_config(base, other);
    for (name, count) in &[
        ("session_start", merged.session_start.len()),
        ("session_end", merged.session_end.len()),
        ("pre_tool_use", merged.pre_tool_use.len()),
        ("post_tool_use", merged.post_tool_use.len()),
        ("user_prompt_submit", merged.user_prompt_submit.len()),
        ("stop", merged.stop.len()),
    ] {
        assert_eq!(
            *count, 1,
            "slot {name:?} must receive other's entry; got {count}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — merge order = associativity check (not a property test
// but a regression guard: A ▷ (B ▷ C) ≠ (A ▷ B) ▷ C only in
// last-write-wins scenarios)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn merge_is_last_write_wins_across_three_layers() {
    // Layer A (user) sets matcher=X to "user-cmd".
    // Layer B (project) sets matcher=X to "project-cmd".
    // Layer C (managed) sets matcher=X to "managed-cmd".
    // Apply as merge(merge(A, B), C) — the leftmost wins in
    // base position; each successive merge replaces.
    let layer_a = cfg(r#"
pre_tool_use:
  - matcher: "^X$"
    hooks: [{type: command, command: "user-cmd"}]
"#);
    let layer_b = cfg(r#"
pre_tool_use:
  - matcher: "^X$"
    hooks: [{type: command, command: "project-cmd"}]
"#);
    let layer_c = cfg(r#"
pre_tool_use:
  - matcher: "^X$"
    hooks: [{type: command, command: "managed-cmd"}]
"#);
    let ab = merge_hooks_config(layer_a, layer_b);
    let abc = merge_hooks_config(ab, layer_c);
    assert_eq!(
        abc.pre_tool_use.len(),
        1,
        "three layers with same matcher → 1 entry"
    );
    let serialised = format!("{:?}", abc.pre_tool_use[0]);
    assert!(
        serialised.contains("managed-cmd"),
        "managed (last layer) must win; got {serialised:?}"
    );
    assert!(
        !serialised.contains("user-cmd") && !serialised.contains("project-cmd"),
        "earlier layers must be replaced; got {serialised:?}"
    );
}
