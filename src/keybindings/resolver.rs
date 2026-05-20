//! Runtime keystroke resolver.
//!
//! Buffers incoming keystrokes and matches them against parsed chord
//! bindings loaded from a [`KeybindingsConfig`]. Lives outside `src/config/`
//! because it is runtime logic, not a YAML schema.

use super::actions::KeyAction;
use super::parser::{parse_chord, ParsedKeystroke};
use crate::config::KeybindingsConfig;

/// Contexts in which keybindings may apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyContext {
    Global,
    Chat,
    Help,
    Confirmation,
    Transcript,
    Autocomplete,
    ModelPicker,
    Settings,
}

/// Result of attempting to resolve a keystroke sequence against the bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordResolveResult {
    /// The pending keystrokes exactly match a binding.
    Match { action: KeyAction },
    /// The pending keystrokes are a prefix of at least one binding (waiting
    /// for more keys).
    Prefix,
    /// The pending keystrokes do not match or prefix any binding.
    NoMatch,
}

/// A single parsed binding: the chord (sequence of keystrokes) mapped to an
/// action.
#[derive(Debug, Clone)]
struct ParsedBinding {
    chord: Vec<ParsedKeystroke>,
    action: KeyAction,
}

/// Runtime resolver that buffers incoming keystrokes and matches them against
/// parsed chord bindings.
#[derive(Debug)]
pub struct KeybindingResolver {
    bindings: Vec<ParsedBinding>,
    pending: Vec<ParsedKeystroke>,
}

impl KeybindingResolver {
    /// Build a resolver from a `KeybindingsConfig`.
    ///
    /// Bindings whose key string cannot be parsed are silently skipped.
    #[must_use]
    pub fn from_config(config: &KeybindingsConfig) -> Self {
        let mut bindings = Vec::new();
        for (key_str, action) in &config.bindings {
            if let Some(chord) = parse_chord(key_str) {
                bindings.push(ParsedBinding {
                    chord,
                    action: action.clone(),
                });
            }
        }
        Self {
            bindings,
            pending: Vec::new(),
        }
    }

    /// Feed a keystroke into the resolver and get the result.
    ///
    /// - `Match` means the full chord is resolved; pending buffer is cleared.
    /// - `Prefix` means we matched the beginning of at least one chord; keep
    ///   waiting.
    /// - `NoMatch` means no binding starts with the current pending sequence;
    ///   pending buffer is cleared.
    pub fn resolve(&mut self, keystroke: ParsedKeystroke) -> ChordResolveResult {
        self.pending.push(keystroke);

        let mut exact_match: Option<KeyAction> = None;
        let mut has_prefix = false;

        for binding in &self.bindings {
            let chord = &binding.chord;

            if chord.len() < self.pending.len() {
                continue;
            }

            // Check whether the pending buffer matches the beginning of this chord.
            let prefix_matches = self.pending.iter().zip(chord.iter()).all(|(a, b)| a == b);

            if !prefix_matches {
                continue;
            }

            if chord.len() == self.pending.len() {
                exact_match = Some(binding.action.clone());
            } else {
                // chord is longer than pending -- this is a prefix match.
                has_prefix = true;
            }
        }

        if let Some(action) = exact_match {
            self.pending.clear();
            ChordResolveResult::Match { action }
        } else if has_prefix {
            ChordResolveResult::Prefix
        } else {
            self.pending.clear();
            ChordResolveResult::NoMatch
        }
    }

    /// Cancel any pending chord, clearing the buffer.
    pub fn cancel(&mut self) {
        self.pending.clear();
    }

    /// Whether the resolver is waiting for more keystrokes to complete a chord.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Human-readable representation of the pending keystrokes so far (e.g.
    /// for status-bar display).
    #[must_use]
    pub fn pending_display(&self) -> String {
        self.pending
            .iter()
            .map(ParsedKeystroke::display)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper: build a config with specific bindings.
    fn test_config(bindings: Vec<(&str, KeyAction)>) -> KeybindingsConfig {
        let mut map = HashMap::new();
        for (k, a) in bindings {
            map.insert(k.to_string(), a);
        }
        KeybindingsConfig { bindings: map }
    }

    #[test]
    fn test_resolver_single_key_match() {
        let config = test_config(vec![("f2", KeyAction::Models)]);
        let mut resolver = KeybindingResolver::from_config(&config);

        let result = resolver.resolve(ParsedKeystroke::parse("f2").unwrap());
        assert_eq!(
            result,
            ChordResolveResult::Match {
                action: KeyAction::Models
            }
        );
        assert!(!resolver.is_pending());
    }

    #[test]
    fn test_resolver_chord_prefix() {
        let config = test_config(vec![("ctrl-x n", KeyAction::NewSession)]);
        let mut resolver = KeybindingResolver::from_config(&config);

        // First keystroke is a prefix
        let result = resolver.resolve(ParsedKeystroke::parse("ctrl-x").unwrap());
        assert_eq!(result, ChordResolveResult::Prefix);
        assert!(resolver.is_pending());
        assert_eq!(resolver.pending_display(), "ctrl-x");
    }

    #[test]
    fn test_resolver_chord_complete() {
        let config = test_config(vec![("ctrl-x n", KeyAction::NewSession)]);
        let mut resolver = KeybindingResolver::from_config(&config);

        // First keystroke: prefix
        let r1 = resolver.resolve(ParsedKeystroke::parse("ctrl-x").unwrap());
        assert_eq!(r1, ChordResolveResult::Prefix);

        // Second keystroke: match
        let r2 = resolver.resolve(ParsedKeystroke::parse("n").unwrap());
        assert_eq!(
            r2,
            ChordResolveResult::Match {
                action: KeyAction::NewSession
            }
        );
        assert!(!resolver.is_pending());
    }

    #[test]
    fn test_resolver_no_match() {
        let config = test_config(vec![("f2", KeyAction::Models)]);
        let mut resolver = KeybindingResolver::from_config(&config);

        let result = resolver.resolve(ParsedKeystroke::parse("f5").unwrap());
        assert_eq!(result, ChordResolveResult::NoMatch);
        assert!(!resolver.is_pending());
    }

    #[test]
    fn test_resolver_no_match_after_prefix() {
        let config = test_config(vec![("ctrl-x n", KeyAction::NewSession)]);
        let mut resolver = KeybindingResolver::from_config(&config);

        // First keystroke is a prefix
        let r1 = resolver.resolve(ParsedKeystroke::parse("ctrl-x").unwrap());
        assert_eq!(r1, ChordResolveResult::Prefix);

        // Second keystroke does not complete any chord
        let r2 = resolver.resolve(ParsedKeystroke::parse("z").unwrap());
        assert_eq!(r2, ChordResolveResult::NoMatch);
        assert!(!resolver.is_pending());
    }

    #[test]
    fn test_resolver_cancel() {
        let config = test_config(vec![("ctrl-x n", KeyAction::NewSession)]);
        let mut resolver = KeybindingResolver::from_config(&config);

        let _ = resolver.resolve(ParsedKeystroke::parse("ctrl-x").unwrap());
        assert!(resolver.is_pending());

        resolver.cancel();
        assert!(!resolver.is_pending());
        assert_eq!(resolver.pending_display(), "");
    }

    #[test]
    fn test_resolver_multiple_bindings() {
        let config = test_config(vec![
            ("ctrl-x n", KeyAction::NewSession),
            ("ctrl-x l", KeyAction::ListSessions),
            ("f2", KeyAction::Models),
        ]);
        let mut resolver = KeybindingResolver::from_config(&config);

        // f2 matches immediately
        let r = resolver.resolve(ParsedKeystroke::parse("f2").unwrap());
        assert_eq!(
            r,
            ChordResolveResult::Match {
                action: KeyAction::Models
            }
        );

        // ctrl-x is prefix for two chords
        let r = resolver.resolve(ParsedKeystroke::parse("ctrl-x").unwrap());
        assert_eq!(r, ChordResolveResult::Prefix);

        // l completes to ListSessions
        let r = resolver.resolve(ParsedKeystroke::parse("l").unwrap());
        assert_eq!(
            r,
            ChordResolveResult::Match {
                action: KeyAction::ListSessions
            }
        );
    }

    #[test]
    fn test_resolver_from_default_config() {
        let config = KeybindingsConfig::default();
        let mut resolver = KeybindingResolver::from_config(&config);

        // Default config has "ctrl-x n" -> NewSession
        let r1 = resolver.resolve(ParsedKeystroke::parse("ctrl-x").unwrap());
        assert_eq!(r1, ChordResolveResult::Prefix);

        let r2 = resolver.resolve(ParsedKeystroke::parse("n").unwrap());
        assert_eq!(
            r2,
            ChordResolveResult::Match {
                action: KeyAction::NewSession
            }
        );
    }
}
