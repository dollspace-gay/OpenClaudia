//! Chord / keystroke parsing.
//!
//! Translates human-readable keybinding strings such as `"ctrl-x n"` or
//! `"alt-shift-tab"` into structured [`ParsedKeystroke`] sequences for the
//! runtime resolver.

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
    #[must_use]
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
    #[must_use]
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
#[must_use]
pub fn parse_chord(s: &str) -> Option<Vec<ParsedKeystroke>> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let keystrokes: Option<Vec<ParsedKeystroke>> =
        parts.iter().map(|p| ParsedKeystroke::parse(p)).collect();
    keystrokes.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
