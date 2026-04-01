//! Full-screen interactive TUI application.
//!
//! Launched via `openclaudia --tui`. Provides a scrollable message view,
//! text input area, status bar, and streaming response display.

use super::events::{AppEvent, EventHandler};
use super::input::TextInput;
use super::messages::{DisplayMessage, MessageList};
use crossterm::{
    event::{KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};
use std::io;
use std::time::Duration;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Main TUI application state.
pub struct App {
    pub messages: MessageList,
    pub input: TextInput,
    pub model: String,
    pub provider: String,
    pub tokens: usize,
    pub mode: String,
    pub should_quit: bool,
    pub is_waiting: bool,
    spinner_frame: usize,
    event_handler: Option<EventHandler>,
}

impl App {
    pub fn new(model: &str, provider: &str) -> Self {
        Self {
            messages: MessageList::new(),
            input: TextInput::new(),
            model: model.to_string(),
            provider: provider.to_string(),
            tokens: 0,
            mode: "Build".to_string(),
            should_quit: false,
            is_waiting: false,
            spinner_frame: 0,
            event_handler: None,
        }
    }

    /// Get an event sender for pushing async API events into the TUI loop.
    pub fn event_sender(&self) -> Option<std::sync::mpsc::Sender<AppEvent>> {
        self.event_handler.as_ref().map(|h| h.sender())
    }

    /// Run the interactive TUI event loop.
    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let events = EventHandler::new(Duration::from_millis(100));
        self.event_handler = Some(EventHandler::new(Duration::from_millis(100)));

        // Welcome
        self.messages.add(DisplayMessage {
            role: "system".to_string(),
            content: format!(
                "OpenClaudia v{} — {} · {}\n\
                 Type your message and press Enter. Ctrl+C to quit.\n\
                 Up/Down to scroll. /help for commands.",
                env!("CARGO_PKG_VERSION"),
                self.provider,
                self.model,
            ),
            tool_name: None,
            is_error: false,
            is_thinking: false,
        });

        loop {
            terminal.draw(|frame| self.draw(frame))?;

            match events.next() {
                Ok(AppEvent::Key(key)) => self.handle_key(key),
                Ok(AppEvent::Tick) => {
                    self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
                }
                Ok(AppEvent::StreamText(text)) => {
                    self.messages.append_streaming(&text);
                    self.messages.scroll_to_bottom();
                }
                Ok(AppEvent::StreamThinking(text)) => {
                    self.messages.append_streaming(&format!("[thinking] {}", text));
                }
                Ok(AppEvent::ToolStart { name, description }) => {
                    self.messages.add(DisplayMessage {
                        role: "tool".to_string(),
                        content: description,
                        tool_name: Some(name),
                        is_error: false,
                        is_thinking: false,
                    });
                }
                Ok(AppEvent::ToolDone {
                    name,
                    success,
                    content,
                }) => {
                    let preview = if content.len() > 300 {
                        format!("{}...", &content[..297])
                    } else {
                        content
                    };
                    self.messages.add(DisplayMessage {
                        role: "tool".to_string(),
                        content: preview,
                        tool_name: Some(name),
                        is_error: !success,
                        is_thinking: false,
                    });
                }
                Ok(AppEvent::ResponseDone) => {
                    self.messages.finish_streaming();
                    self.is_waiting = false;
                }
                Ok(AppEvent::ApiError(msg)) => {
                    self.messages.finish_streaming();
                    self.messages.add(DisplayMessage {
                        role: "system".to_string(),
                        content: format!("Error: {}", msg),
                        tool_name: None,
                        is_error: true,
                        is_thinking: false,
                    });
                    self.is_waiting = false;
                }
                Ok(AppEvent::Resize(_, _)) => {} // terminal.draw handles it
                Err(_) => break,
            }

            if self.should_quit {
                break;
            }
        }

        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
        Ok(())
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        // Ctrl+C always quits
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // During streaming, Escape cancels
        if self.is_waiting {
            if key.code == KeyCode::Esc {
                self.is_waiting = false;
                self.messages.finish_streaming();
                self.messages.add(DisplayMessage {
                    role: "system".to_string(),
                    content: "[Response interrupted]".to_string(),
                    tool_name: None,
                    is_error: false,
                    is_thinking: false,
                });
            }
            return;
        }

        match key.code {
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    let text = self.input.take();

                    if text == "/quit" || text == "/exit" {
                        self.should_quit = true;
                        return;
                    }

                    if text == "/help" {
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: "Commands: /quit, /exit, /clear, /help\n\
                                      Scroll: Up/Down/PageUp/PageDown\n\
                                      Cancel: Escape (during streaming)\n\
                                      Quit: Ctrl+C"
                                .to_string(),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text == "/clear" {
                        self.messages = MessageList::new();
                        return;
                    }

                    // Show user message
                    self.messages.add(DisplayMessage {
                        role: "user".to_string(),
                        content: text.clone(),
                        tool_name: None,
                        is_error: false,
                        is_thinking: false,
                    });

                    // Placeholder — API integration will send via event_sender
                    self.is_waiting = true;
                    self.messages.add(DisplayMessage {
                        role: "system".to_string(),
                        content: format!(
                            "[TUI mode: message queued for {} — API wiring pending]",
                            self.model
                        ),
                        tool_name: None,
                        is_error: false,
                        is_thinking: false,
                    });
                    self.is_waiting = false;
                }
            }
            KeyCode::Char(c) => self.input.insert(c),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up => self.messages.scroll_up(3),
            KeyCode::Down => self.messages.scroll_down(3),
            KeyCode::PageUp => self.messages.scroll_up(15),
            KeyCode::PageDown => self.messages.scroll_down(15),
            _ => {}
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),   // Messages
                Constraint::Length(3), // Input
                Constraint::Length(1), // Status
            ])
            .split(frame.area());

        // ── Messages ──
        self.messages.render(frame, chunks[0]);

        // ── Input ──
        let title = if self.is_waiting {
            format!(
                " {} Waiting... ",
                SPINNER_FRAMES[self.spinner_frame]
            )
        } else {
            " Message ".to_string()
        };
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(title)
            .title_style(Style::default().fg(Color::Rgb(147, 112, 219)));

        let input_para = Paragraph::new(self.input.content.as_str())
            .block(input_block)
            .style(Style::default().fg(Color::White));
        frame.render_widget(input_para, chunks[1]);

        // Cursor
        if !self.is_waiting {
            let cx = chunks[1].x + 1 + self.input.cursor_pos as u16;
            let cy = chunks[1].y + 1;
            frame.set_cursor_position(Position::new(cx.min(chunks[1].right().saturating_sub(1)), cy));
        }

        // ── Status bar ──
        let spinner = if self.is_waiting {
            format!("{} ", SPINNER_FRAMES[self.spinner_frame])
        } else {
            String::new()
        };
        let status_text = format!(
            " {}{} | {} | ~{} tokens | {} ",
            spinner, self.model, self.provider, self.tokens, self.mode,
        );
        let status = Paragraph::new(status_text)
            .style(Style::default().fg(Color::White).bg(Color::Rgb(40, 40, 60)));
        frame.render_widget(status, chunks[2]);
    }
}
