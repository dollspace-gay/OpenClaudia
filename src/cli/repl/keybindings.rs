use super::slash::SlashCommandResult;
use openclaudia::config;

/// Convert a crossterm `KeyEvent` to a keybinding string format
/// Examples: "escape", "f2", "ctrl-x", "ctrl-x n" (with leader key state)
pub fn key_event_to_string(
    event: &crossterm::event::KeyEvent,
    leader_active: bool,
) -> Option<String> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let key_str = match event.code {
        KeyCode::Esc => "escape".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Char(c) => {
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                format!("ctrl-{}", c.to_lowercase())
            } else if event.modifiers.contains(KeyModifiers::ALT) {
                format!("alt-{}", c.to_lowercase())
            } else {
                c.to_lowercase().to_string()
            }
        }
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        _ => return None,
    };

    if leader_active {
        Some(format!("ctrl-x {key_str}"))
    } else {
        Some(key_str)
    }
}

/// Execute a key action and return a result indicator
pub fn execute_key_action(action: &config::KeyAction) -> Option<SlashCommandResult> {
    use config::KeyAction;

    match action {
        KeyAction::Cancel | KeyAction::None => None,
        KeyAction::NewSession | KeyAction::Clear => Some(SlashCommandResult::Clear),
        KeyAction::Exit => Some(SlashCommandResult::Exit),
        KeyAction::Export => Some(SlashCommandResult::Export),
        KeyAction::Compact => Some(SlashCommandResult::Compact),
        KeyAction::Undo => Some(SlashCommandResult::Undo),
        KeyAction::Redo => Some(SlashCommandResult::Redo),
        KeyAction::ToggleMode => Some(SlashCommandResult::ToggleMode),
        KeyAction::Status => Some(SlashCommandResult::Status),
        KeyAction::Models => {
            println!("\nUse /models to see available models.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::ListSessions => {
            println!("\nUse /sessions to see saved sessions.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::CopyResponse => {
            println!("\nUse /copy to copy the last response.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::Editor => {
            println!("\nUse /editor to open external editor.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::Help => {
            println!("\nUse /help for commands.\n");
            Some(SlashCommandResult::Handled)
        }
    }
}

/// Display current keybindings configuration
pub fn display_keybindings(keybindings: &config::KeybindingsConfig) {
    use config::KeyAction;

    println!("\nConfigured Keybindings:");
    println!("========================\n");

    let actions = [
        (KeyAction::NewSession, "New session"),
        (KeyAction::ListSessions, "List sessions"),
        (KeyAction::Export, "Export conversation"),
        (KeyAction::CopyResponse, "Copy last response"),
        (KeyAction::Editor, "Open external editor"),
        (KeyAction::Models, "Show/switch models"),
        (KeyAction::ToggleMode, "Toggle Build/Plan mode"),
        (KeyAction::Cancel, "Cancel response"),
        (KeyAction::Status, "Show status"),
        (KeyAction::Help, "Show help"),
        (KeyAction::Clear, "Clear/new conversation"),
        (KeyAction::Undo, "Undo last exchange"),
        (KeyAction::Redo, "Redo last exchange"),
        (KeyAction::Compact, "Compact conversation"),
        (KeyAction::Exit, "Exit application"),
    ];

    for (action, description) in actions {
        let keys = keybindings.get_keys_for_action(&action);
        if !keys.is_empty() {
            let key_str = keys
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("  {key_str:20} {description}");
        }
    }

    let disabled = keybindings.get_keys_for_action(&KeyAction::None);
    if !disabled.is_empty() {
        println!("\nDisabled bindings:");
        for key in disabled {
            println!("  {key} (disabled)");
        }
    }

    println!("\nTo customize, add a 'keybindings' section to your config.yaml.");
    println!("Set any key to 'none' to disable it.\n");
}
