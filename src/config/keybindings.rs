use serde::Deserialize;
use std::collections::HashMap;

/// Keybinding action names
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    /// Start a new session
    NewSession,
    /// List saved sessions
    ListSessions,
    /// Export conversation to markdown
    Export,
    /// Copy last response to clipboard
    CopyResponse,
    /// Open external editor
    Editor,
    /// Show/switch models
    Models,
    /// Toggle Build/Plan mode
    ToggleMode,
    /// Cancel in-progress response
    Cancel,
    /// Show session status
    Status,
    /// Show help
    Help,
    /// Clear/new conversation
    Clear,
    /// Exit the application
    Exit,
    /// Undo last exchange
    Undo,
    /// Redo last undone exchange
    Redo,
    /// Compact conversation
    Compact,
    /// No action (disabled keybinding)
    None,
}

/// Keybindings configuration
/// Maps key combinations to actions. Use "none" to disable a keybinding.
#[derive(Debug, Deserialize, Clone)]
pub struct KeybindingsConfig {
    /// Map of key combination strings to action names
    /// Example: { "ctrl-x n": "new_session", "f2": "models", "tab": "none" }
    #[serde(flatten)]
    pub bindings: HashMap<String, KeyAction>,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        let mut bindings = HashMap::new();
        // Default keybindings (Ctrl+X leader key pattern)
        bindings.insert("ctrl-x n".to_string(), KeyAction::NewSession);
        bindings.insert("ctrl-x l".to_string(), KeyAction::ListSessions);
        bindings.insert("ctrl-x x".to_string(), KeyAction::Export);
        bindings.insert("ctrl-x y".to_string(), KeyAction::CopyResponse);
        bindings.insert("ctrl-x e".to_string(), KeyAction::Editor);
        bindings.insert("ctrl-x m".to_string(), KeyAction::Models);
        bindings.insert("ctrl-x s".to_string(), KeyAction::Status);
        bindings.insert("ctrl-x h".to_string(), KeyAction::Help);
        bindings.insert("f2".to_string(), KeyAction::Models);
        bindings.insert("tab".to_string(), KeyAction::ToggleMode);
        bindings.insert("escape".to_string(), KeyAction::Cancel);
        Self { bindings }
    }
}

impl KeybindingsConfig {
    /// Get the action for a key combination
    pub fn get_action(&self, key: &str) -> Option<&KeyAction> {
        self.bindings.get(&key.to_lowercase())
    }

    /// Check if a key is bound (returns None for disabled or unbound keys)
    pub fn is_bound(&self, key: &str) -> bool {
        matches!(self.get_action(key), Some(action) if *action != KeyAction::None)
    }

    /// Get all bindings for a specific action
    pub fn get_keys_for_action(&self, action: &KeyAction) -> Vec<&String> {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(k, _)| k)
            .collect()
    }

    /// Get the action for a key, with default fallback
    /// Returns the configured action or the default action for that key
    pub fn get_action_or_default(&self, key: &str) -> KeyAction {
        self.get_action(key).cloned().unwrap_or(KeyAction::None)
    }
}

// ============================================================================
// Chord Parsing & Runtime Resolution
// ============================================================================

/// A single parsed keystroke such as `ctrl-x` or `alt-shift-n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKeystroke {
    /// The base key name (e.g. "x", "n", "f2", "tab").
    pub key: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl ParsedKeystroke {
    /// Parse a human-readable keystroke string.
    ///
    /// Modifiers (`ctrl`, `alt`, `shift`) are separated from the key name by
    /// `-`. Order of modifiers does not matter. The *last* non-modifier segment
    /// is the key name.
    ///
    /// Examples:
    /// - `"ctrl-x"` -> ctrl=true, key="x"
    /// - `"alt-shift-n"` -> alt=true, shift=true, key="n"
    /// - `"f2"` -> key="f2"
    /// - `"a"` -> key="a"
    /// - `"shift-tab"` -> shift=true, key="tab"
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_lowercase();
        if s.is_empty() {
            return None;
        }

        let parts: Vec<&str> = s.split('-').collect();

        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut key_parts: Vec<&str> = Vec::new();

        for (i, part) in parts.iter().enumerate() {
            match *part {
                "ctrl" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                _ => {
                    // Everything from here to the end is the key name
                    // (handles keys like "f2" which have no dash, but also
                    // allows for future keys that might contain dashes if
                    // they are not modifier names).
                    key_parts = parts[i..].to_vec();
                    break;
                }
            }
        }

        // If all segments were modifiers, there is no key name.
        if key_parts.is_empty() {
            return None;
        }

        let key = key_parts.join("-");

        Some(Self {
            key,
            ctrl,
            alt,
            shift,
        })
    }

    /// Human-readable representation, e.g. `"ctrl-x"` or `"alt-shift-n"`.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        parts.push(&self.key);
        parts.join("-")
    }
}

/// Parse a chord string (space-separated keystrokes) into a sequence.
///
/// For example `"ctrl-x n"` produces two `ParsedKeystroke` values, while
/// `"f2"` produces one.
pub fn parse_chord(s: &str) -> Option<Vec<ParsedKeystroke>> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let keystrokes: Option<Vec<ParsedKeystroke>> =
        parts.iter().map(|p| ParsedKeystroke::parse(p)).collect();
    keystrokes.filter(|v| !v.is_empty())
}

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
            let prefix_matches = self
                .pending
                .iter()
                .zip(chord.iter())
                .all(|(a, b)| a == b);

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
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Human-readable representation of the pending keystrokes so far (e.g.
    /// for status-bar display).
    pub fn pending_display(&self) -> String {
        self.pending
            .iter()
            .map(|k| k.display())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_action_serialization() {
        let action = KeyAction::NewSession;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"new_session\"");

        let action2: KeyAction = serde_json::from_str("\"toggle_mode\"").unwrap();
        assert_eq!(action2, KeyAction::ToggleMode);
    }

    #[test]
    fn test_key_action_all_variants() {
        // Ensure all variants can be serialized/deserialized
        let actions = vec![
            ("\"new_session\"", KeyAction::NewSession),
            ("\"list_sessions\"", KeyAction::ListSessions),
            ("\"export\"", KeyAction::Export),
            ("\"copy_response\"", KeyAction::CopyResponse),
            ("\"editor\"", KeyAction::Editor),
            ("\"models\"", KeyAction::Models),
            ("\"toggle_mode\"", KeyAction::ToggleMode),
            ("\"cancel\"", KeyAction::Cancel),
            ("\"status\"", KeyAction::Status),
            ("\"help\"", KeyAction::Help),
            ("\"clear\"", KeyAction::Clear),
            ("\"exit\"", KeyAction::Exit),
            ("\"undo\"", KeyAction::Undo),
            ("\"redo\"", KeyAction::Redo),
            ("\"compact\"", KeyAction::Compact),
            ("\"none\"", KeyAction::None),
        ];

        for (json, expected) in actions {
            let parsed: KeyAction = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_keybindings_config_default() {
        let config = KeybindingsConfig::default();

        // Verify default bindings exist
        assert_eq!(
            config.bindings.get("ctrl-x n"),
            Some(&KeyAction::NewSession)
        );
        assert_eq!(
            config.bindings.get("ctrl-x l"),
            Some(&KeyAction::ListSessions)
        );
        assert_eq!(config.bindings.get("ctrl-x x"), Some(&KeyAction::Export));
        assert_eq!(config.bindings.get("f2"), Some(&KeyAction::Models));
        assert_eq!(config.bindings.get("tab"), Some(&KeyAction::ToggleMode));
        assert_eq!(config.bindings.get("escape"), Some(&KeyAction::Cancel));
    }

    #[test]
    fn test_keybindings_get_action() {
        let config = KeybindingsConfig::default();

        // Case-insensitive lookup
        assert_eq!(config.get_action("ctrl-x n"), Some(&KeyAction::NewSession));
        assert_eq!(config.get_action("CTRL-X N"), Some(&KeyAction::NewSession));

        // Unknown key returns None
        assert_eq!(config.get_action("unknown-key"), None);
    }

    #[test]
    fn test_keybindings_is_bound() {
        let mut config = KeybindingsConfig::default();

        // Regular binding is bound
        assert!(config.is_bound("ctrl-x n"));

        // Unknown key is not bound
        assert!(!config.is_bound("unknown-key"));

        // Explicitly disabled key (set to None) is not bound
        config
            .bindings
            .insert("disabled-key".to_string(), KeyAction::None);
        assert!(!config.is_bound("disabled-key"));
    }

    #[test]
    fn test_keybindings_get_keys_for_action() {
        let config = KeybindingsConfig::default();

        // Models has two bindings in default config
        let model_keys = config.get_keys_for_action(&KeyAction::Models);
        assert!(model_keys.contains(&&"ctrl-x m".to_string()));
        assert!(model_keys.contains(&&"f2".to_string()));
    }

    #[test]
    fn test_keybindings_get_action_or_default() {
        let config = KeybindingsConfig::default();

        // Known key returns its action
        assert_eq!(
            config.get_action_or_default("ctrl-x n"),
            KeyAction::NewSession
        );

        // Unknown key returns None action
        assert_eq!(config.get_action_or_default("unknown"), KeyAction::None);
    }

    // ====================================================================
    // ParsedKeystroke tests
    // ====================================================================

    #[test]
    fn test_parsed_keystroke_ctrl_x() {
        let ks = ParsedKeystroke::parse("ctrl-x").unwrap();
        assert!(ks.ctrl);
        assert!(!ks.alt);
        assert!(!ks.shift);
        assert_eq!(ks.key, "x");
    }

    #[test]
    fn test_parsed_keystroke_alt_n() {
        let ks = ParsedKeystroke::parse("alt-n").unwrap();
        assert!(!ks.ctrl);
        assert!(ks.alt);
        assert!(!ks.shift);
        assert_eq!(ks.key, "n");
    }

    #[test]
    fn test_parsed_keystroke_shift_tab() {
        let ks = ParsedKeystroke::parse("shift-tab").unwrap();
        assert!(!ks.ctrl);
        assert!(!ks.alt);
        assert!(ks.shift);
        assert_eq!(ks.key, "tab");
    }

    #[test]
    fn test_parsed_keystroke_plain_a() {
        let ks = ParsedKeystroke::parse("a").unwrap();
        assert!(!ks.ctrl);
        assert!(!ks.alt);
        assert!(!ks.shift);
        assert_eq!(ks.key, "a");
    }

    #[test]
    fn test_parsed_keystroke_f2() {
        let ks = ParsedKeystroke::parse("f2").unwrap();
        assert!(!ks.ctrl);
        assert!(!ks.alt);
        assert!(!ks.shift);
        assert_eq!(ks.key, "f2");
    }

    #[test]
    fn test_parsed_keystroke_alt_shift_n() {
        let ks = ParsedKeystroke::parse("alt-shift-n").unwrap();
        assert!(!ks.ctrl);
        assert!(ks.alt);
        assert!(ks.shift);
        assert_eq!(ks.key, "n");
    }

    #[test]
    fn test_parsed_keystroke_display() {
        let ks = ParsedKeystroke::parse("ctrl-x").unwrap();
        assert_eq!(ks.display(), "ctrl-x");

        let ks2 = ParsedKeystroke::parse("alt-shift-n").unwrap();
        assert_eq!(ks2.display(), "alt-shift-n");

        let ks3 = ParsedKeystroke::parse("f2").unwrap();
        assert_eq!(ks3.display(), "f2");
    }

    #[test]
    fn test_parsed_keystroke_empty_returns_none() {
        assert!(ParsedKeystroke::parse("").is_none());
        assert!(ParsedKeystroke::parse("   ").is_none());
    }

    #[test]
    fn test_parsed_keystroke_only_modifiers_returns_none() {
        assert!(ParsedKeystroke::parse("ctrl").is_none());
        assert!(ParsedKeystroke::parse("ctrl-alt-shift").is_none());
    }

    // ====================================================================
    // parse_chord tests
    // ====================================================================

    #[test]
    fn test_parse_chord_two_keystrokes() {
        let chord = parse_chord("ctrl-x n").unwrap();
        assert_eq!(chord.len(), 2);

        assert!(chord[0].ctrl);
        assert_eq!(chord[0].key, "x");

        assert!(!chord[1].ctrl);
        assert_eq!(chord[1].key, "n");
    }

    #[test]
    fn test_parse_chord_single_keystroke() {
        let chord = parse_chord("f2").unwrap();
        assert_eq!(chord.len(), 1);
        assert_eq!(chord[0].key, "f2");
    }

    #[test]
    fn test_parse_chord_empty_returns_none() {
        assert!(parse_chord("").is_none());
        assert!(parse_chord("   ").is_none());
    }

    // ====================================================================
    // KeybindingResolver tests
    // ====================================================================

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
