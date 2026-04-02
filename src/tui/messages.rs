//! Scrollable message list for the TUI.

use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

/// A single display message in the conversation.
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub is_error: bool,
    pub is_thinking: bool,
}

/// Scrollable message list with streaming support.
pub struct MessageList {
    pub messages: Vec<DisplayMessage>,
    pub scroll_offset: u16,
    pub streaming_text: String,
    pub is_streaming: bool,
}

impl MessageList {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            streaming_text: String::new(),
            is_streaming: false,
        }
    }

    /// Remove the last N messages from the display list.
    pub fn pop_last(&mut self, count: usize) {
        for _ in 0..count {
            self.messages.pop();
        }
    }

    /// Number of messages in the display list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn add(&mut self, msg: DisplayMessage) {
        self.messages.push(msg);
        self.scroll_to_bottom();
    }

    pub fn append_streaming(&mut self, text: &str) {
        self.streaming_text.push_str(text);
        self.is_streaming = true;
    }

    pub fn finish_streaming(&mut self) {
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage {
                role: "assistant".to_string(),
                content: std::mem::take(&mut self.streaming_text),
                tool_name: None,
                is_error: false,
                is_thinking: false,
            });
        }
        self.is_streaming = false;
    }

    pub const fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    pub const fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    pub const fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Build ratatui Lines for rendering.
    fn build_lines(&self) -> Vec<Line<'_>> {
        let mut lines: Vec<Line> = Vec::new();

        for msg in &self.messages {
            match msg.role.as_str() {
                "system" => {
                    // System messages: render with purple branding for welcome,
                    // dim for other system messages
                    let is_welcome = msg.content.contains("OpenClaudia v");
                    if is_welcome {
                        // Render welcome block with colored labels
                        for line in msg.content.lines() {
                            let styled = if line.starts_with("OpenClaudia v") {
                                Line::from(vec![
                                    Span::styled(
                                        "OpenClaudia",
                                        Style::default()
                                            .fg(Color::Rgb(147, 112, 219))
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        &line["OpenClaudia".len()..],
                                        Style::default().fg(Color::Rgb(218, 165, 32)),
                                    ),
                                ])
                            } else if line.starts_with("Provider:") {
                                Line::from(Span::styled(
                                    line,
                                    Style::default().fg(Color::Rgb(147, 112, 219)),
                                ))
                            } else if line.starts_with("Model:") {
                                Line::from(Span::styled(
                                    line,
                                    Style::default().fg(Color::Rgb(218, 165, 32)),
                                ))
                            } else if line.starts_with("Welcome") {
                                Line::from(Span::styled(
                                    line,
                                    Style::default()
                                        .fg(Color::White)
                                        .add_modifier(Modifier::BOLD),
                                ))
                            } else {
                                Line::from(Span::styled(
                                    line,
                                    Style::default().fg(Color::DarkGray),
                                ))
                            };
                            lines.push(styled);
                        }
                    } else {
                        // Regular system message
                        for line in msg.content.lines() {
                            lines.push(Line::from(Span::styled(
                                format!("  {line}"),
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::ITALIC),
                            )));
                        }
                    }
                    lines.push(Line::from(""));
                }
                "user" => {
                    lines.push(Line::from(Span::styled(
                        "\u{203A} user",
                        Style::default()
                            .fg(Color::Rgb(100, 180, 255))
                            .add_modifier(Modifier::BOLD),
                    )));
                    for line in msg.content.lines() {
                        lines.push(Line::from(format!("  {line}")));
                    }
                    lines.push(Line::from(""));
                }
                "assistant" => {
                    lines.push(Line::from(Span::styled(
                        "\u{23BF} Claudia",
                        Style::default()
                            .fg(Color::Rgb(147, 112, 219))
                            .add_modifier(Modifier::BOLD),
                    )));
                    let content_style = if msg.is_thinking {
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC)
                    } else {
                        Style::default()
                    };
                    for line in msg.content.lines() {
                        lines.push(Line::from(Span::styled(format!("  {line}"), content_style)));
                    }
                    lines.push(Line::from(""));
                }
                "tool" => {
                    let tool_name = msg.tool_name.as_deref().unwrap_or("tool");
                    if msg.is_error {
                        lines.push(Line::from(Span::styled(
                            format!("  \u{2717} {tool_name}"),
                            Style::default().fg(Color::Red),
                        )));
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!("  \u{2713} {tool_name}"),
                            Style::default().fg(Color::Green),
                        )));
                    }
                    // Show truncated content
                    let preview = if msg.content.len() > 200 {
                        format!("{}...", crate::tools::safe_truncate(&msg.content, 197))
                    } else {
                        msg.content.clone()
                    };
                    for line in preview.lines().take(5) {
                        lines.push(Line::from(Span::styled(
                            format!("    {line}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    lines.push(Line::from(""));
                }
                _ => {
                    for line in msg.content.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    lines.push(Line::from(""));
                }
            }
        }

        // Streaming content
        if self.is_streaming && !self.streaming_text.is_empty() {
            lines.push(Line::from(Span::styled(
                "\u{23BF} Claudia",
                Style::default()
                    .fg(Color::Rgb(147, 112, 219))
                    .add_modifier(Modifier::BOLD),
            )));
            for line in self.streaming_text.lines() {
                lines.push(Line::from(format!("  {line}")));
            }
            // Cursor indicator
            lines.push(Line::from(Span::styled(
                "  \u{2588}",
                Style::default().fg(Color::Rgb(147, 112, 219)),
            )));
        }

        lines
    }

    /// Render the message list into a frame area.
    /// Content is anchored to the bottom — empty space is at the top, not below.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mut lines = self.build_lines();
        #[allow(clippy::cast_possible_truncation)] // line count bounded by terminal height
        let total = lines.len() as u16;
        let visible = area.height;

        // Pad the top with empty lines so content anchors to the bottom
        if total < visible {
            let pad = (visible - total) as usize;
            let mut padded = vec![Line::from(""); pad];
            padded.append(&mut lines);
            lines = padded;
        }

        #[allow(clippy::cast_possible_truncation)]
        let total = lines.len() as u16;
        let scroll = if total > visible {
            (total - visible).saturating_sub(self.scroll_offset)
        } else {
            0
        };

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        frame.render_widget(paragraph, area);
    }
}

impl Default for MessageList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_count() {
        let mut ml = MessageList::new();
        ml.add(DisplayMessage {
            role: "user".into(),
            content: "hello".into(),
            tool_name: None,
            is_error: false,
            is_thinking: false,
        });
        assert_eq!(ml.messages.len(), 1);
    }

    #[test]
    fn test_streaming() {
        let mut ml = MessageList::new();
        ml.append_streaming("hello ");
        ml.append_streaming("world");
        assert!(ml.is_streaming);
        assert_eq!(ml.streaming_text, "hello world");
        ml.finish_streaming();
        assert!(!ml.is_streaming);
        assert_eq!(ml.messages.len(), 1);
        assert_eq!(ml.messages[0].content, "hello world");
    }

    #[test]
    fn test_scroll() {
        let mut ml = MessageList::new();
        ml.scroll_up(5);
        assert_eq!(ml.scroll_offset, 5);
        ml.scroll_down(3);
        assert_eq!(ml.scroll_offset, 2);
        ml.scroll_to_bottom();
        assert_eq!(ml.scroll_offset, 0);
    }
}
