//! Full-screen interactive TUI application.
//!
//! Launched via `openclaudia` (default) or `openclaudia --tui`.
//! Provides a scrollable message view, text input area, status bar,
//! and streaming response display wired to the real API pipeline.

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

/// Chat session state managed inside the TUI.
///
/// Mirrors the essential fields from `cli::repl::ChatSession` without
/// pulling in the rustyline-specific types.
pub struct TuiChatSession {
    pub messages: Vec<serde_json::Value>,
    pub model: String,
    pub provider: String,
    pub system_prompt: String,
}

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

    // ── API pipeline fields ──
    pub client: reqwest::Client,
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub effort_level: String,
    pub system_prompt: String,
    pub claude_code_token: Option<String>,
    /// Conversation messages in the provider's wire format.
    pub session_messages: Vec<serde_json::Value>,
    /// Async runtime handle for spawning API tasks from the sync event loop.
    runtime_handle: Option<tokio::runtime::Handle>,
}

impl App {
    #[must_use]
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
            client: reqwest::Client::new(),
            endpoint: String::new(),
            headers: Vec::new(),
            effort_level: "medium".to_string(),
            system_prompt: String::new(),
            claude_code_token: None,
            session_messages: Vec::new(),
            runtime_handle: None,
        }
    }

    /// Set the API connection details needed to make requests.
    pub fn set_api_config(
        &mut self,
        endpoint: String,
        headers: Vec<(String, String)>,
        system_prompt: String,
        claude_code_token: Option<String>,
    ) {
        self.endpoint = endpoint;
        self.headers = headers;
        self.system_prompt = system_prompt;
        self.claude_code_token = claude_code_token;
    }

    /// Get an event sender for pushing async API events into the TUI loop.
    #[must_use]
    pub fn event_sender(&self) -> Option<std::sync::mpsc::Sender<AppEvent>> {
        self.event_handler
            .as_ref()
            .map(super::events::EventHandler::sender)
    }

    /// Run the interactive TUI event loop.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal initialization or rendering fails.
    pub fn run(&mut self) -> io::Result<()> {
        // Capture the tokio runtime handle (must be called inside an async context).
        self.runtime_handle = tokio::runtime::Handle::try_current().ok();

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let events = EventHandler::new(Duration::from_millis(100));
        self.event_handler = Some(EventHandler::new(Duration::from_millis(100)));

        // Inject system prompt as the first message
        if !self.system_prompt.is_empty() {
            self.session_messages.push(serde_json::json!({
                "role": "system",
                "content": self.system_prompt
            }));
        }

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
                    self.messages
                        .append_streaming(&format!("[thinking] {text}"));
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
                        content: format!("Error: {msg}"),
                        tool_name: None,
                        is_error: true,
                        is_thinking: false,
                    });
                    self.is_waiting = false;
                }
                Ok(AppEvent::Resize(_, _)) => {} // terminal.draw handles it
                // Pipeline follow-up: tool results need another API call
                Ok(AppEvent::FollowUp) => {
                    self.spawn_api_turn();
                }
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

                    // ── Slash commands ──
                    if text == "/quit" || text == "/exit" {
                        self.should_quit = true;
                        return;
                    }

                    if text == "/help" || text == "?" {
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: "Commands: /quit, /exit, /clear, /help, /effort, /status\n\
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
                        // Reset session but keep system prompt
                        self.session_messages.retain(|m| {
                            m.get("role").and_then(|r| r.as_str()) == Some("system")
                        });
                        return;
                    }

                    if text == "/status" {
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!(
                                "Model: {}\nProvider: {}\nEffort: {}\nMessages: {}\n~{} tokens",
                                self.model,
                                self.provider,
                                self.effort_level,
                                self.session_messages.len(),
                                self.tokens,
                            ),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text.starts_with("/effort") {
                        let parts: Vec<&str> = text.splitn(2, ' ').collect();
                        if parts.len() == 2 {
                            let level = parts[1].trim();
                            if matches!(level, "low" | "medium" | "high") {
                                self.effort_level = level.to_string();
                            }
                        } else {
                            // Cycle: low -> medium -> high -> low
                            self.effort_level = match self.effort_level.as_str() {
                                "low" => "medium".to_string(),
                                "medium" => "high".to_string(),
                                _ => "low".to_string(),
                            };
                        }
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!("Effort level: {}", self.effort_level),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    if text.starts_with('/') {
                        self.messages.add(DisplayMessage {
                            role: "system".to_string(),
                            content: format!("Unknown command: {text}. Type /help for commands."),
                            tool_name: None,
                            is_error: false,
                            is_thinking: false,
                        });
                        return;
                    }

                    // ── Normal message: send to API ──
                    self.messages.add(DisplayMessage {
                        role: "user".to_string(),
                        content: text.clone(),
                        tool_name: None,
                        is_error: false,
                        is_thinking: false,
                    });

                    // Add to session history
                    self.session_messages.push(serde_json::json!({
                        "role": "user",
                        "content": text
                    }));

                    self.is_waiting = true;
                    self.spawn_api_turn();
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

    /// Spawn an async API turn on the tokio runtime.
    ///
    /// Sends events through the event handler's mpsc channel so the
    /// synchronous TUI event loop can display streaming output.
    fn spawn_api_turn(&mut self) {
        let Some(ref handle) = self.runtime_handle else {
            // No async runtime — show fallback message
            self.messages.add(DisplayMessage {
                role: "system".to_string(),
                content: "[No async runtime — cannot call API. Run with tokio.]".to_string(),
                tool_name: None,
                is_error: true,
                is_thinking: false,
            });
            self.is_waiting = false;
            return;
        };

        let Some(tx) = self.event_sender() else {
            self.is_waiting = false;
            return;
        };

        // Build request
        let request_body = crate::pipeline::build_request(
            &self.provider,
            &self.model,
            &self.session_messages,
            &self.effort_level,
            self.claude_code_token.as_deref(),
        );

        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let headers = self.headers.clone();
        let provider = self.provider.clone();
        let model = self.model.clone();
        let effort_level = self.effort_level.clone();
        let claude_code_token = self.claude_code_token.clone();
        // Clone session messages so the async task can build follow-up requests
        let mut session_messages = self.session_messages.clone();

        handle.spawn(async move {
            // Run the turn (may include tool execution)
            match crate::pipeline::run_turn(
                &client, &endpoint, &headers, &request_body, &provider, None, tx.clone(),
            )
            .await
            {
                Ok(turn_result) => {
                    // If the model returned tool calls, we need to append the
                    // assistant message + tool results and send a follow-up.
                    if turn_result.needs_followup {
                        // Build assistant message with tool calls
                        let assistant_msg =
                            crate::pipeline::build_assistant_message_with_tools(
                                &turn_result.content,
                                &turn_result.tool_calls,
                                &provider,
                            );
                        session_messages.push(assistant_msg);
                        // Append tool results
                        session_messages.extend(turn_result.tool_results.iter().cloned());

                        // Agentic loop: keep calling until no more tool calls
                        let max_iterations = 25u32;
                        let mut iteration = 0u32;
                        let mut current_messages = session_messages;
                        loop {
                            iteration += 1;
                            if iteration > max_iterations {
                                let _ = tx.send(AppEvent::ApiError(
                                    "Reached maximum tool iterations (25)".to_string(),
                                ));
                                break;
                            }

                            let followup_body = crate::pipeline::build_request(
                                &provider,
                                &model,
                                &current_messages,
                                &effort_level,
                                claude_code_token.as_deref(),
                            );

                            match crate::pipeline::run_turn(
                                &client,
                                &endpoint,
                                &headers,
                                &followup_body,
                                &provider,
                                None,
                                tx.clone(),
                            )
                            .await
                            {
                                Ok(followup) => {
                                    if followup.needs_followup {
                                        let asst_msg =
                                            crate::pipeline::build_assistant_message_with_tools(
                                                &followup.content,
                                                &followup.tool_calls,
                                                &provider,
                                            );
                                        current_messages.push(asst_msg);
                                        current_messages.extend(
                                            followup.tool_results.iter().cloned(),
                                        );
                                        // continue loop
                                    } else {
                                        // Done — final text response
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(AppEvent::ApiError(e));
                                    break;
                                }
                            }
                        }
                    }
                    // else: no tool calls, ResponseDone already sent by run_turn
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::ApiError(e));
                }
            }
        });
    }

    fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // Messages
                Constraint::Length(3), // Input
                Constraint::Length(1), // Status
            ])
            .split(frame.area());

        // ── Messages ──
        self.messages.render(frame, chunks[0]);

        // ── Input ──
        let title = if self.is_waiting {
            format!(" {} Waiting... ", SPINNER_FRAMES[self.spinner_frame])
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
            #[allow(clippy::cast_possible_truncation)] // cursor_pos bounded by terminal width
            let cx = chunks[1].x + 1 + self.input.cursor_pos as u16;
            let cy = chunks[1].y + 1;
            frame.set_cursor_position(Position::new(
                cx.min(chunks[1].right().saturating_sub(1)),
                cy,
            ));
        }

        // ── Status bar ──
        let left_text = "? for shortcuts";
        let effort_symbol = match self.effort_level.as_str() {
            "low" => "○",
            "high" => "●",
            _ => "◐",
        };
        let right_text = format!("{effort_symbol} {} · /effort", self.effort_level);

        // Pad to fill the bar width
        let bar_width = chunks[2].width as usize;
        let padding = bar_width.saturating_sub(left_text.len() + right_text.len() + 2);
        let status_text = format!(" {left_text}{:>width$} ", right_text, width = padding + right_text.len());

        let status = Paragraph::new(status_text)
            .style(Style::default().fg(Color::White).bg(Color::Rgb(40, 40, 60)));
        frame.render_widget(status, chunks[2]);
    }
}
