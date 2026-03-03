//! TUI module for OpenClaudia
//!
//! Provides a rich terminal user interface similar to Claude Code,
//! with two-column layout, tips panel, styled text, markdown rendering,
//! status bar, and theme management.

use crossterm::{
    cursor,
    style::{Attribute, Color as CtColor, Print, ResetColor, SetAttribute, SetForegroundColor},
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
use std::path::{Path, PathBuf};

/// Purple color for branding (from logo)
const PURPLE: Color = Color::Rgb(147, 112, 219);
/// Gold color for accents (from logo)
const GOLD: Color = Color::Rgb(218, 165, 32);
/// Dim gray for borders
const DIM: Color = Color::Rgb(128, 128, 128);

// ─── Theme support ──────────────────────────────────────────────────────────

/// A color theme for the terminal UI
#[derive(Debug, Clone)]
pub struct Theme {
    /// Theme identifier
    pub name: String,
    /// Primary color (headings, status bar highlights)
    pub primary: CtColor,
    /// Secondary color (accents)
    pub secondary: CtColor,
    /// Code block / inline code color
    pub code_color: CtColor,
    /// Heading color
    pub heading_color: CtColor,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            primary: CtColor::Rgb {
                r: 147,
                g: 112,
                b: 219,
            },
            secondary: CtColor::Rgb {
                r: 218,
                g: 165,
                b: 32,
            },
            code_color: CtColor::Cyan,
            heading_color: CtColor::Rgb {
                r: 147,
                g: 112,
                b: 219,
            },
        }
    }
}

impl Theme {
    /// Build a theme from one of the built-in names
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "default" => Some(Self::default()),
            "ocean" => Some(Self {
                name: "ocean".to_string(),
                primary: CtColor::Rgb {
                    r: 0,
                    g: 150,
                    b: 255,
                },
                secondary: CtColor::Cyan,
                code_color: CtColor::Rgb {
                    r: 0,
                    g: 200,
                    b: 200,
                },
                heading_color: CtColor::Rgb {
                    r: 0,
                    g: 150,
                    b: 255,
                },
            }),
            "forest" => Some(Self {
                name: "forest".to_string(),
                primary: CtColor::Green,
                secondary: CtColor::Rgb {
                    r: 144,
                    g: 238,
                    b: 144,
                },
                code_color: CtColor::Rgb {
                    r: 0,
                    g: 200,
                    b: 100,
                },
                heading_color: CtColor::Green,
            }),
            "sunset" => Some(Self {
                name: "sunset".to_string(),
                primary: CtColor::Rgb {
                    r: 255,
                    g: 140,
                    b: 0,
                },
                secondary: CtColor::Rgb {
                    r: 255,
                    g: 69,
                    b: 0,
                },
                code_color: CtColor::Yellow,
                heading_color: CtColor::Rgb {
                    r: 255,
                    g: 140,
                    b: 0,
                },
            }),
            "mono" => Some(Self {
                name: "mono".to_string(),
                primary: CtColor::White,
                secondary: CtColor::Grey,
                code_color: CtColor::White,
                heading_color: CtColor::White,
            }),
            "neon" => Some(Self {
                name: "neon".to_string(),
                primary: CtColor::Magenta,
                secondary: CtColor::Cyan,
                code_color: CtColor::Rgb {
                    r: 0,
                    g: 255,
                    b: 255,
                },
                heading_color: CtColor::Magenta,
            }),
            _ => None,
        }
    }

    /// Save the current theme name to disk (in user config directory)
    pub fn save(&self) -> io::Result<()> {
        let path = theme_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, &self.name)?;
        Ok(())
    }

    /// Load the saved theme from disk, falling back to default
    pub fn load() -> Self {
        let path = theme_path();
        if let Ok(name) = std::fs::read_to_string(&path) {
            let name = name.trim();
            if let Some(theme) = Self::from_name(name) {
                return theme;
            }
        }
        Self::default()
    }
}

/// Return a stable path for the theme file, using the user's config directory
/// (e.g. `~/.config/openclaudia/theme` on Linux, `~/Library/Application Support/openclaudia/theme` on macOS).
/// Falls back to `.openclaudia/theme` relative to CWD if the platform config dir is unavailable.
fn theme_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("openclaudia")
        .join("theme")
}

// ─── Markdown rendering ─────────────────────────────────────────────────────

/// Render markdown-formatted text to the terminal with styling.
///
/// Supports:
/// - **bold** and *italic* inline
/// - `inline code` in cyan/code color
/// - ```fenced code blocks``` with language header
/// - # Headings at various levels
/// - - / * / numbered list items
/// - > block quotes
/// - [link text](url)
pub fn render_markdown(text: &str) {
    render_markdown_themed(text, &Theme::load());
}

/// Render markdown with a specific theme
pub fn render_markdown_themed(text: &str, theme: &Theme) {
    let mut stdout = io::stdout();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    for line in text.lines() {
        if line.starts_with("```") {
            if in_code_block {
                // End code block
                in_code_block = false;
                code_lang.clear();
                let _ = stdout.execute(ResetColor);
                println!();
            } else {
                // Start code block
                in_code_block = true;
                code_lang = line.trim_start_matches('`').trim().to_string();
                if !code_lang.is_empty() {
                    let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
                    println!("  --- {} ---", code_lang);
                    let _ = stdout.execute(ResetColor);
                }
            }
            continue;
        }

        if in_code_block {
            // Render code block lines with indentation and color
            let _ = stdout.execute(SetForegroundColor(theme.code_color));
            println!("    {}", line);
            let _ = stdout.execute(ResetColor);
            continue;
        }

        // Heading detection
        if line.starts_with('#') {
            render_heading(&mut stdout, line, theme);
            continue;
        }

        // Blockquote
        if line.starts_with("> ") || line == ">" {
            let content = line.strip_prefix("> ").unwrap_or("");
            let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
            print!("  | ");
            let _ = stdout.execute(SetForegroundColor(CtColor::White));
            render_inline(&mut stdout, content, theme);
            println!();
            let _ = stdout.execute(ResetColor);
            continue;
        }

        // List items (unordered: -, *, and ordered: 1., 2., etc.)
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let indent = line.len() - trimmed.len();
            let content = &trimmed[2..];
            print!("{}  \u{2022} ", " ".repeat(indent));
            render_inline(&mut stdout, content, theme);
            println!();
            continue;
        }
        if let Some(rest) = strip_ordered_list_prefix(trimmed) {
            let indent = line.len() - trimmed.len();
            let num_part = &trimmed[..trimmed.len() - rest.len()];
            print!("{}  {}", " ".repeat(indent), num_part);
            render_inline(&mut stdout, rest, theme);
            println!();
            continue;
        }

        // Horizontal rule
        if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            let (cols, _) = terminal::size().unwrap_or((80, 24));
            let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
            println!("{}", "\u{2500}".repeat(cols as usize));
            let _ = stdout.execute(ResetColor);
            continue;
        }

        // Regular line with inline formatting
        render_inline(&mut stdout, line, theme);
        println!();
    }
    let _ = stdout.execute(ResetColor);
    stdout.flush().ok();
}

/// Render a heading line
fn render_heading(stdout: &mut io::Stdout, line: &str, theme: &Theme) {
    let level = line.chars().take_while(|c| *c == '#').count();
    let text = line[level..].trim_start();

    let _ = stdout.execute(SetAttribute(Attribute::Bold));
    if level <= 2 {
        let _ = stdout.execute(SetForegroundColor(theme.heading_color));
    }

    match level {
        1 => println!("\n{}\n", text.to_uppercase()),
        2 => println!("\n{}\n", text),
        3 => println!("{}", text),
        _ => println!("{}", text),
    }

    let _ = stdout.execute(SetAttribute(Attribute::Reset));
    let _ = stdout.execute(ResetColor);
}

/// Render inline formatting: bold, italic, inline code, links
fn render_inline(stdout: &mut io::Stdout, text: &str, theme: &Theme) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Bold: **text**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_closing(&chars, i + 2, "**") {
                let _ = stdout.execute(SetAttribute(Attribute::Bold));
                let inner: String = chars[i + 2..end].iter().collect();
                print!("{}", inner);
                let _ = stdout.execute(SetAttribute(Attribute::NoBold));
                i = end + 2;
                continue;
            }
        }

        // Italic: *text* (but not **)
        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if let Some(end) = find_closing_char(&chars, i + 1, '*') {
                let _ = stdout.execute(SetAttribute(Attribute::Italic));
                let inner: String = chars[i + 1..end].iter().collect();
                print!("{}", inner);
                let _ = stdout.execute(SetAttribute(Attribute::NoItalic));
                i = end + 1;
                continue;
            }
        }

        // Inline code: `text`
        if chars[i] == '`' {
            if let Some(end) = find_closing_char(&chars, i + 1, '`') {
                let _ = stdout.execute(SetForegroundColor(theme.code_color));
                let inner: String = chars[i + 1..end].iter().collect();
                print!("{}", inner);
                let _ = stdout.execute(ResetColor);
                i = end + 1;
                continue;
            }
        }

        // Link: [text](url)
        if chars[i] == '[' {
            if let Some((link_text, url, end_pos)) = parse_link(&chars, i) {
                let _ = stdout.execute(SetAttribute(Attribute::Underlined));
                print!("{}", link_text);
                let _ = stdout.execute(SetAttribute(Attribute::NoUnderline));
                let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
                print!(" ({})", url);
                let _ = stdout.execute(ResetColor);
                i = end_pos;
                continue;
            }
        }

        // Regular character
        print!("{}", chars[i]);
        i += 1;
    }
}

/// Find closing delimiter in char slice (e.g., "**")
fn find_closing(chars: &[char], start: usize, delim: &str) -> Option<usize> {
    let delim_chars: Vec<char> = delim.chars().collect();
    let dlen = delim_chars.len();
    if dlen == 0 {
        return None;
    }
    let mut i = start;
    while i + dlen <= chars.len() {
        if chars[i..i + dlen] == delim_chars[..] {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find closing single character delimiter
fn find_closing_char(chars: &[char], start: usize, delim: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == delim)
}

/// Parse a markdown link [text](url) starting at position i ('[')
fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    // Find closing ']'
    let text_end = find_closing_char(chars, start + 1, ']')?;
    let link_text: String = chars[start + 1..text_end].iter().collect();

    // Expect '(' immediately after ']'
    let paren_start = text_end + 1;
    if paren_start >= chars.len() || chars[paren_start] != '(' {
        return None;
    }

    let url_end = find_closing_char(chars, paren_start + 1, ')')?;
    let url: String = chars[paren_start + 1..url_end].iter().collect();

    Some((link_text, url, url_end + 1))
}

/// Strip an ordered list prefix like "1. ", "12. " and return the remainder
fn strip_ordered_list_prefix(s: &str) -> Option<&str> {
    let mut chars = s.chars();
    // Must start with a digit
    let first = chars.next()?;
    if !first.is_ascii_digit() {
        return None;
    }
    // Consume remaining digits
    let mut dot_pos = 1;
    for ch in chars {
        if ch.is_ascii_digit() {
            dot_pos += 1;
        } else if ch == '.' {
            dot_pos += 1;
            break;
        } else {
            return None;
        }
    }
    // Must have ". " after digits
    if dot_pos < s.len() && s.as_bytes().get(dot_pos) == Some(&b' ') {
        return Some(&s[dot_pos + 1..]);
    }
    None
}

// ─── Status bar ─────────────────────────────────────────────────────────────

/// Draw a persistent status bar at the bottom of the terminal.
///
/// Shows: model name, token count, cost, mode, session duration
pub fn draw_status_bar(model: &str, tokens: usize, cost: Option<f64>, mode: &str, duration: &str) {
    let mut stdout = io::stdout();
    let (cols, rows) = terminal::size().unwrap_or((80, 24));

    let cost_str = match cost {
        Some(c) if c >= 0.01 => format!("${:.2}", c),
        Some(c) => format!("${:.4}", c),
        None => String::new(),
    };

    let token_str = if tokens >= 1_000_000 {
        format!("{:.1}M tokens", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k tokens", tokens as f64 / 1_000.0)
    } else {
        format!("{} tokens", tokens)
    };

    let status = if cost_str.is_empty() {
        format!(" {} | {} | {} | {} ", model, token_str, mode, duration)
    } else {
        format!(
            " {} | {} | {} | {} | {} ",
            model, cost_str, token_str, mode, duration
        )
    };

    // Pad to fill the terminal width
    let padded = format!("{:<width$}", status, width = cols as usize);

    let _ = crossterm::execute!(
        stdout,
        cursor::SavePosition,
        cursor::MoveTo(0, rows.saturating_sub(1)),
        SetForegroundColor(CtColor::White),
        SetAttribute(Attribute::Reverse),
        Print(&padded),
        SetAttribute(Attribute::Reset),
        ResetColor,
        cursor::RestorePosition,
    );
    stdout.flush().ok();
}

// ─── Thinking display ───────────────────────────────────────────────────────

/// Print a thinking/reasoning chunk in dim styling
pub fn print_thinking_chunk(text: &str) {
    let mut stdout = io::stdout();
    let _ = stdout.execute(SetAttribute(Attribute::Dim));
    let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
    print!("{}", text);
    let _ = stdout.execute(SetAttribute(Attribute::Reset));
    let _ = stdout.execute(ResetColor);
    stdout.flush().ok();
}

/// Print the thinking header when a thinking block starts
pub fn print_thinking_start() {
    let mut stdout = io::stdout();
    let _ = stdout.execute(SetAttribute(Attribute::Dim));
    let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
    print!("Thinking: ");
    let _ = stdout.execute(SetAttribute(Attribute::Reset));
    let _ = stdout.execute(ResetColor);
    stdout.flush().ok();
}

/// Print a summary when a thinking block ends
pub fn print_thinking_end(duration_secs: f64) {
    let mut stdout = io::stdout();
    let _ = stdout.execute(SetAttribute(Attribute::Dim));
    let _ = stdout.execute(SetForegroundColor(CtColor::DarkGrey));
    if duration_secs > 0.0 {
        println!("\n  (thought for {:.1}s)", duration_secs);
    } else {
        println!();
    }
    let _ = stdout.execute(SetAttribute(Attribute::Reset));
    let _ = stdout.execute(ResetColor);
    stdout.flush().ok();
}

// ─── Original TUI components ────────────────────────────────────────────────

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
    stdout.execute(SetForegroundColor(CtColor::Rgb {
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
pub(crate) fn capitalize_first(s: &str) -> String {
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
        assert!(
            tips.len() >= 3,
            "Should have at least 3 tips, got {}",
            tips.len()
        );
        // Verify tips contain actual user-facing guidance
        assert!(
            tips.iter().any(|t| t.contains("/init")),
            "Tips should mention /init command"
        );
        assert!(
            tips.iter().any(|t| t.contains("/help")),
            "Tips should mention /help command"
        );
    }

    #[test]
    fn test_theme_from_name() {
        assert!(Theme::from_name("default").is_some());
        assert!(Theme::from_name("ocean").is_some());
        assert!(Theme::from_name("forest").is_some());
        assert!(Theme::from_name("sunset").is_some());
        assert!(Theme::from_name("mono").is_some());
        assert!(Theme::from_name("neon").is_some());
        assert!(Theme::from_name("nonexistent").is_none());
    }

    #[test]
    fn test_theme_default() {
        let theme = Theme::default();
        assert_eq!(theme.name, "default");
    }

    #[test]
    fn test_strip_ordered_list_prefix() {
        assert_eq!(strip_ordered_list_prefix("1. hello"), Some("hello"));
        assert_eq!(strip_ordered_list_prefix("12. world"), Some("world"));
        assert_eq!(strip_ordered_list_prefix("not a list"), None);
        assert_eq!(strip_ordered_list_prefix("- dash"), None);
    }

    #[test]
    fn test_find_closing() {
        let chars: Vec<char> = "hello**world".chars().collect();
        assert_eq!(find_closing(&chars, 0, "**"), Some(5));
    }

    #[test]
    fn test_find_closing_char() {
        let chars: Vec<char> = "hello`world".chars().collect();
        assert_eq!(find_closing_char(&chars, 0, '`'), Some(5));
    }

    #[test]
    fn test_parse_link() {
        let chars: Vec<char> = "[click here](https://example.com) rest".chars().collect();
        let result = parse_link(&chars, 0);
        assert!(result.is_some());
        let (text, url, end) = result.unwrap();
        assert_eq!(text, "click here");
        assert_eq!(url, "https://example.com");
        assert_eq!(end, 33);
    }
}
