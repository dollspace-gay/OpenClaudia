//! TUI module for OpenClaudia
//!
//! Provides a rich terminal user interface similar to Claude Code,
//! with two-column layout, tips panel, and styled text.

use crossterm::{
    cursor,
    terminal::{self, Clear, ClearType},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::{self, stdout, Write};

/// Purple color for branding (from logo)
const PURPLE: Color = Color::Rgb(147, 112, 219);
/// Gold color for accents (from logo)
const GOLD: Color = Color::Rgb(218, 165, 32);
/// Dim gray for borders
const DIM: Color = Color::Rgb(128, 128, 128);

/// Get a random tip for the tips section
pub fn get_tips() -> Vec<&'static str> {
    vec![
        "Run /init to create a config file with instructions",
        "Use @filename to include file contents in your prompt",
        "Type /help for a list of all commands",
        "Use Tab to toggle between Build and Plan modes",
        "Press Ctrl+C to cancel a running request",
        "Use /export to save your conversation as markdown",
        "Type !command to run shell commands directly",
    ]
}

/// Welcome screen configuration
pub struct WelcomeScreen {
    pub version: String,
    pub provider: String,
    pub model: String,
    pub username: Option<String>,
}

impl WelcomeScreen {
    pub fn new(version: &str, provider: &str, model: &str) -> Self {
        Self {
            version: version.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            username: get_username(),
        }
    }

    /// Render the welcome screen using ratatui
    pub fn render(&self) -> io::Result<()> {
        let mut stdout = stdout();

        // Setup terminal for ratatui
        terminal::enable_raw_mode()?;

        // Use scope block to ensure terminal is dropped before reusing stdout
        let height = {
            let backend = CrosstermBackend::new(&mut stdout);
            let mut terminal = Terminal::new(backend)?;
            terminal.draw(|frame| self.draw(frame))?;
            let size = terminal::size()?;
            8.min(size.1)
        }; // terminal dropped here, releasing stdout borrow

        // Restore terminal
        terminal::disable_raw_mode()?;

        // Move cursor below the rendered area
        stdout.execute(cursor::MoveTo(0, height + 1))?;

        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let size = frame.area();

        // Limit box width
        let box_width = size.width.min(90);
        let box_height = 8;

        // Center the box if terminal is wider
        let x_offset = (size.width.saturating_sub(box_width)) / 2;
        let area = Rect::new(x_offset, 0, box_width, box_height);

        // Create the main block with title
        let title = Line::from(vec![
            Span::styled(
                "OpenClaudia",
                Style::default().fg(PURPLE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" v{}", self.version), Style::default().fg(GOLD)),
        ]);

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(DIM));

        // Split into two columns
        let inner = block.inner(area);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(inner);

        // Render block first
        frame.render_widget(block, area);

        // Left column content
        let greeting = if let Some(ref name) = self.username {
            format!("Welcome back, {}!", name)
        } else {
            "Welcome to OpenClaudia!".to_string()
        };

        let left_text = vec![
            Line::from(Span::styled(&greeting, Style::default().fg(Color::White))),
            Line::from(""),
            Line::from(Span::styled(
                format!("Provider: {}", capitalize_first(&self.provider)),
                Style::default().fg(PURPLE),
            )),
            Line::from(Span::styled(
                format!("Model: {}", &self.model),
                Style::default().fg(GOLD),
            )),
        ];
        let left_para = Paragraph::new(left_text).wrap(Wrap { trim: true });
        frame.render_widget(left_para, chunks[0]);

        // Right column content
        let right_text = vec![
            Line::from(Span::styled("Tips", Style::default().fg(GOLD))),
            Line::from(Span::styled(
                get_tips()[0],
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(Span::styled("Recent activity", Style::default().fg(GOLD))),
            Line::from(Span::styled("No recent activity", Style::default().fg(DIM))),
        ];
        let right_para = Paragraph::new(right_text).wrap(Wrap { trim: true });
        frame.render_widget(right_para, chunks[1]);
    }
}

/// Render the input prompt area with hints
pub fn render_input_prompt(mode: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    let (term_width, _) = terminal::size().unwrap_or((80, 24));

    let left_hint = "? for shortcuts";
    let right_hint = format!(
        "Tab for {} mode",
        if mode == "build" { "Plan" } else { "Build" }
    );

    // Use crossterm for simple colored text
    use crossterm::style::{ResetColor, SetForegroundColor};
    stdout.execute(SetForegroundColor(crossterm::style::Color::Rgb {
        r: 128,
        g: 128,
        b: 128,
    }))?;
    let padding = (term_width as usize).saturating_sub(left_hint.len() + right_hint.len() + 4);
    writeln!(
        stdout,
        "  {}{}  {}",
        left_hint,
        " ".repeat(padding),
        right_hint
    )?;
    stdout.execute(ResetColor)?;

    stdout.flush()?;
    Ok(())
}

/// Get the current username from environment
fn get_username() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
}

/// Capitalize the first letter of a string
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

/// Clear the screen and move cursor to top
pub fn clear_screen() -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.execute(Clear(ClearType::All))?;
    stdout.execute(cursor::MoveTo(0, 0))?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capitalize_first() {
        assert_eq!(capitalize_first("anthropic"), "Anthropic");
        assert_eq!(capitalize_first("openai"), "Openai");
        assert_eq!(capitalize_first(""), "");
    }

    #[test]
    fn test_get_tips() {
        let tips = get_tips();
        assert!(!tips.is_empty());
    }
}
