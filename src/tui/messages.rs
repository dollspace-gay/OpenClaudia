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
            let (icon, style) = match msg.role.as_str() {
                "user" => (
                    "› ",
                    Style::default()
                        .fg(Color::Rgb(100, 180, 255))
                        .add_modifier(Modifier::BOLD),
                ),
                "assistant" => (
                    "● ",
                    Style::default()
                        .fg(Color::Rgb(147, 112, 219))
                        .add_modifier(Modifier::BOLD),
                ),
                "tool" => ("⚙ ", Style::default().fg(Color::Rgb(218, 165, 32))),
                "system" => (
                    "  ",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ),
                _ => ("  ", Style::default().fg(Color::DarkGray)),
            };

            let header = msg.tool_name.as_ref().map_or_else(
                || format!("{}{}", icon, msg.role),
                |tool| format!("{icon}Tool: {tool}"),
            );
            lines.push(Line::from(Span::styled(header, style)));

            let content_style = if msg.is_error {
                Style::default().fg(Color::Red)
            } else if msg.is_thinking {
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

        // Streaming content
        if self.is_streaming && !self.streaming_text.is_empty() {
            lines.push(Line::from(Span::styled(
                "● assistant",
                Style::default()
                    .fg(Color::Rgb(147, 112, 219))
                    .add_modifier(Modifier::BOLD),
            )));
            for line in self.streaming_text.lines() {
                lines.push(Line::from(format!("  {line}")));
            }
            // Blinking cursor
            lines.push(Line::from(Span::styled(
                "  ▊",
                Style::default().fg(Color::Rgb(147, 112, 219)),
            )));
        }

        lines
    }

    /// Render the message list into a frame area.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let lines = self.build_lines();
        #[allow(clippy::cast_possible_truncation)] // line count bounded by terminal height
        let total = lines.len() as u16;
        let visible = area.height;
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
