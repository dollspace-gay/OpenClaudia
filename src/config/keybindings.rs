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
}
