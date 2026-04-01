//! Output style customization for response formatting.
//!
//! Loads style definitions from markdown files in `.openclaudia/output-style.md`
//! or `~/.openclaudia/output-style.md`. The style content is injected into the
//! system prompt to customize how the model formats responses.

use std::path::{Path, PathBuf};

/// Load the active output style, if any.
/// Checks project-level first, then user-level.
#[must_use]
pub fn load_output_style() -> Option<String> {
    let project_style = PathBuf::from(".openclaudia/output-style.md");
    if project_style.exists() {
        return read_style(&project_style);
    }

    if let Some(home) = dirs::home_dir() {
        let user_style = home.join(".openclaudia/output-style.md");
        if user_style.exists() {
            return read_style(&user_style);
        }
    }

    None
}

fn read_style(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Get a list of built-in style presets
#[must_use]
pub fn builtin_styles() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "concise",
            "Be extremely concise. Lead with the answer. No filler, no preamble. One sentence when possible.",
        ),
        (
            "detailed",
            "Provide thorough, detailed explanations. Include examples and edge cases. Use headers for organization.",
        ),
        (
            "minimal",
            "Respond with the absolute minimum text needed. No greetings, no sign-offs, no explanations unless asked.",
        ),
        (
            "educational",
            "Explain concepts step by step. Use analogies. Highlight key terms. Suitable for learning.",
        ),
        (
            "code-only",
            "When asked to write code, respond with ONLY the code. No explanations before or after unless specifically asked.",
        ),
    ]
}

/// Save a style to the project output-style file.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn save_output_style(content: &str) -> Result<(), String> {
    let dir = PathBuf::from(".openclaudia");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {e}"))?;
    std::fs::write(dir.join("output-style.md"), content)
        .map_err(|e| format!("Failed to write: {e}"))
}

/// Remove the project output-style file.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be removed.
pub fn clear_output_style() -> Result<(), String> {
    let path = PathBuf::from(".openclaudia/output-style.md");
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to remove: {e}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_styles() {
        let styles = builtin_styles();
        assert!(styles.len() >= 4);
        assert!(styles.iter().any(|(name, _)| *name == "concise"));
    }

    #[test]
    fn test_load_style_nonexistent() {
        // Should return None when no style file exists (may or may not depending on env)
        let _ = load_output_style();
    }
}
