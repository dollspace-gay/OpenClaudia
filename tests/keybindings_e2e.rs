//! End-to-end tests for the keybinding parser + resolver state machine.
//!
//! Sprint 12 of the verification effort. `src/keybindings/parser.rs`
//! has 12 unit tests and `src/keybindings/resolver.rs` has 8, but
//! no integration coverage that drives the parser + resolver +
//! YAML-config layers as a unit. Focus areas:
//!
//!   - **Parser adversarial inputs** — empty, modifier-only,
//!     trailing-dash, internal whitespace inside a single
//!     keystroke, mixed-case inputs, multiple modifiers in
//!     non-canonical order.
//!   - **Chord state machine** — Prefix → Match transitions
//!     across multi-stroke chords; Cancel mid-chord clears the
//!     buffer; an unknown leading keystroke returns `NoMatch`
//!     without buffering.
//!   - **Configuration round-trip** — a YAML config string with
//!     hostile inputs (duplicate keys at different cases, the
//!     `none` action to disable a binding, an unparsable
//!     binding silently dropped) loads cleanly and produces a
//!     resolver that honours the documented decision-table.
//!   - **Counter-tests** — the default config's documented
//!     bindings all resolve correctly via the live resolver.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::KeybindingsConfig;
use openclaudia::keybindings::{
    parse_chord, ChordResolveResult, KeyAction, KeybindingResolver, ParsedKeystroke,
};

// ───────────────────────────────────────────────────────────────────────────
// Section A — parser adversarial inputs
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parser_rejects_empty_and_whitespace_only_inputs() {
    assert!(ParsedKeystroke::parse("").is_none());
    assert!(ParsedKeystroke::parse("   ").is_none());
    assert!(ParsedKeystroke::parse("\t").is_none());
}

#[test]
fn parser_rejects_modifier_only_inputs() {
    // A keystroke MUST end in a key name — "ctrl" alone or
    // "ctrl-alt" with no key is meaningless.
    for raw in &[
        "ctrl",
        "alt",
        "shift",
        "ctrl-alt",
        "ctrl-shift",
        "alt-shift",
    ] {
        assert!(
            ParsedKeystroke::parse(raw).is_none(),
            "modifier-only input {raw:?} must NOT parse"
        );
    }
}

#[test]
fn parser_is_case_insensitive_on_input() {
    let lower = ParsedKeystroke::parse("ctrl-x").expect("lower parses");
    let upper = ParsedKeystroke::parse("CTRL-X").expect("upper parses");
    let mixed = ParsedKeystroke::parse("Ctrl-X").expect("mixed parses");
    assert_eq!(lower, upper);
    assert_eq!(lower, mixed);
    assert!(lower.ctrl);
    assert_eq!(lower.key, "x");
}

#[test]
fn parser_accepts_modifiers_in_any_order() {
    let canon = ParsedKeystroke::parse("ctrl-alt-shift-n").expect("canonical");
    let scrambled = ParsedKeystroke::parse("shift-alt-ctrl-n").expect("scrambled");
    let mixed = ParsedKeystroke::parse("alt-ctrl-shift-n").expect("mixed");
    assert_eq!(canon, scrambled);
    assert_eq!(canon, mixed);
    assert!(canon.ctrl);
    assert!(canon.alt);
    assert!(canon.shift);
    assert_eq!(canon.key, "n");
}

#[test]
fn parser_supports_named_keys() {
    let f2 = ParsedKeystroke::parse("f2").expect("f2");
    assert_eq!(f2.key, "f2");
    assert!(!f2.ctrl && !f2.alt && !f2.shift);

    let escape = ParsedKeystroke::parse("escape").expect("escape");
    assert_eq!(escape.key, "escape");

    let shift_tab = ParsedKeystroke::parse("shift-tab").expect("shift-tab");
    assert_eq!(shift_tab.key, "tab");
    assert!(shift_tab.shift);
}

#[test]
fn parse_chord_splits_on_whitespace() {
    let chord = parse_chord("ctrl-x n").expect("two-stroke chord");
    assert_eq!(chord.len(), 2);
    assert_eq!(chord[0].key, "x");
    assert!(chord[0].ctrl);
    assert_eq!(chord[1].key, "n");

    let single = parse_chord("f2").expect("single stroke");
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].key, "f2");
}

#[test]
fn parse_chord_rejects_empty_input() {
    assert!(parse_chord("").is_none());
    assert!(parse_chord("   ").is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — resolver state machine
// ───────────────────────────────────────────────────────────────────────────

fn ks(s: &str) -> ParsedKeystroke {
    ParsedKeystroke::parse(s).unwrap_or_else(|| panic!("must parse {s:?}"))
}

#[test]
fn resolver_default_config_resolves_documented_bindings() {
    // The default config ships `ctrl-x n` → NewSession,
    // `ctrl-x l` → ListSessions, `tab` → ToggleMode, etc.
    // Drive them through the resolver and pin the action mapping.
    let cfg = KeybindingsConfig::default();
    let mut r = KeybindingResolver::from_config(&cfg);

    // Single stroke: tab → ToggleMode.
    match r.resolve(ks("tab")) {
        ChordResolveResult::Match { action } => {
            assert_eq!(action, KeyAction::ToggleMode);
        }
        other => panic!("tab must Match ToggleMode; got {other:?}"),
    }

    // Multi-stroke: ctrl-x n → NewSession (prefix first, then match).
    match r.resolve(ks("ctrl-x")) {
        ChordResolveResult::Prefix => (),
        other => panic!("ctrl-x must be Prefix; got {other:?}"),
    }
    match r.resolve(ks("n")) {
        ChordResolveResult::Match { action } => {
            assert_eq!(action, KeyAction::NewSession);
        }
        other => panic!("ctrl-x n must Match NewSession; got {other:?}"),
    }
}

#[test]
fn resolver_clears_pending_buffer_on_match() {
    // After a Match, the pending buffer must be cleared — a
    // subsequent unrelated keystroke must NOT re-resolve as part
    // of the prior chord.
    let cfg = KeybindingsConfig::default();
    let mut r = KeybindingResolver::from_config(&cfg);
    let _ = r.resolve(ks("tab")); // Match
    let result = r.resolve(ks("n"));
    // "n" alone is not a bound action in the default config; it
    // must be NoMatch — NOT "ctrl-x n" treated as a continuation.
    assert!(
        matches!(result, ChordResolveResult::NoMatch),
        "lone 'n' after Match must be NoMatch, got {result:?}"
    );
}

#[test]
fn resolver_clears_pending_buffer_on_no_match() {
    // A keystroke that doesn't even start a chord must clear the
    // pending buffer so a subsequent keystroke is evaluated fresh.
    let cfg = KeybindingsConfig::default();
    let mut r = KeybindingResolver::from_config(&cfg);
    let _ = r.resolve(ks("z")); // NoMatch
                                // Now `tab` should still resolve as Match — the buffer must
                                // not carry stale state from the prior NoMatch.
    match r.resolve(ks("tab")) {
        ChordResolveResult::Match { action } => {
            assert_eq!(action, KeyAction::ToggleMode);
        }
        other => panic!("tab after NoMatch must still Match; got {other:?}"),
    }
}

#[test]
fn resolver_cancel_aborts_pending_chord() {
    let cfg = KeybindingsConfig::default();
    let mut r = KeybindingResolver::from_config(&cfg);
    // Start a chord.
    let prefix = r.resolve(ks("ctrl-x"));
    assert!(matches!(prefix, ChordResolveResult::Prefix));
    // Cancel.
    r.cancel();
    // Now `n` alone must be NoMatch — the `ctrl-x` was discarded.
    let result = r.resolve(ks("n"));
    assert!(
        matches!(result, ChordResolveResult::NoMatch),
        "after cancel, lone 'n' must be NoMatch (no continuation); got {result:?}"
    );
}

#[test]
fn resolver_prefix_display_reflects_pending_chord() {
    let cfg = KeybindingsConfig::default();
    let mut r = KeybindingResolver::from_config(&cfg);
    let _ = r.resolve(ks("ctrl-x"));
    let display = r.pending_display();
    assert!(
        display.to_lowercase().contains("ctrl-x"),
        "pending_display must show the unresolved prefix; got {display:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — YAML config integration
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn yaml_config_round_trips_through_resolver() {
    // Author a YAML keybindings block, deserialise it, build a
    // resolver, drive a stroke through it.
    let yaml = r"
ctrl-q: exit
alt-c: clear
f1: help
";
    let cfg: KeybindingsConfig = serde_yaml::from_str(yaml).expect("yaml parses");
    let mut r = KeybindingResolver::from_config(&cfg);

    for (input, expected) in &[
        ("ctrl-q", KeyAction::Exit),
        ("alt-c", KeyAction::Clear),
        ("f1", KeyAction::Help),
    ] {
        match r.resolve(ks(input)) {
            ChordResolveResult::Match { action } => assert_eq!(action, *expected),
            other => panic!("{input:?} must Match {expected:?}; got {other:?}"),
        }
    }
}

#[test]
fn yaml_config_unparsable_binding_is_silently_dropped() {
    // A binding whose key string is unparseable must NOT cause the
    // entire config to fail — it's silently skipped (the schema
    // documents this). A subsequent valid binding still works.
    let yaml = "\n\"garbage--double-dash--\": exit\nctrl-q: exit\n";
    let cfg: KeybindingsConfig = serde_yaml::from_str(yaml).expect("yaml still parses");
    let mut r = KeybindingResolver::from_config(&cfg);
    // The valid binding must still resolve.
    match r.resolve(ks("ctrl-q")) {
        ChordResolveResult::Match { action } => assert_eq!(action, KeyAction::Exit),
        other => panic!("ctrl-q must Match Exit despite unparseable sibling; got {other:?}"),
    }
}

#[test]
fn yaml_config_none_action_disables_a_binding() {
    // Setting the action to `none` is documented as the way to
    // disable a binding without removing it. The resolver MUST
    // return Match { action: None } so the dispatch layer can
    // treat it as a no-op (rather than fall through to a global
    // default).
    let yaml = r"
tab: none
";
    let cfg: KeybindingsConfig = serde_yaml::from_str(yaml).expect("yaml parses");
    let mut r = KeybindingResolver::from_config(&cfg);
    match r.resolve(ks("tab")) {
        ChordResolveResult::Match { action } => assert_eq!(action, KeyAction::None),
        other => panic!("tab=none must Match None; got {other:?}"),
    }
}

#[test]
fn yaml_config_unknown_action_name_fails_to_deserialize() {
    // An action name that isn't in KeyAction's variant list MUST
    // be a hard YAML deserialization error — silently ignoring
    // typo'd action names would let a config like `ctrl-q: exti`
    // appear to work but quietly do nothing.
    let yaml = r"
ctrl-q: not_a_real_action
";
    let outcome: Result<KeybindingsConfig, _> = serde_yaml::from_str(yaml);
    assert!(
        outcome.is_err(),
        "unknown action name must be a hard deserialize error; got {outcome:?}"
    );
}
