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
    /// Sender for pushing API events into the event loop's channel.
    api_event_tx: Option<std::sync::mpsc::Sender<AppEvent>>,

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
            api_event_tx: None,
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
        self.api_event_tx.clone()
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

        // Single event handler — MUST NOT create two, or they steal each other's keypresses
        let events = EventHandler::new(Duration::from_millis(100));
        // Store a sender clone so spawn_api_turn can push events into the same channel
        self.api_event_tx = Some(events.sender());

        // Inject system prompt as the first message
        if !self.system_prompt.is_empty() {
            self.session_messages.push(serde_json::json!({
                "role": "system",
                "content": self.system_prompt
            }));
        }

        // No welcome message added to the message list — the welcome
        // box is rendered directly in draw() as a ratatui widget.

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
                Constraint::Length(8), // Welcome box
                Constraint::Min(3),    // Messages
                Constraint::Length(3), // Input
                Constraint::Length(1), // Status
            ])
            .split(frame.area());

        // ── Welcome box (two-column, bordered) ──
        self.draw_welcome_box(frame, chunks[0]);

        // ── Messages ──
        self.messages.render(frame, chunks[1]);

        // ── Input area ──
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Rgb(128, 128, 128)));

        let prompt_text = if self.is_waiting {
            format!("{} ", SPINNER_FRAMES[self.spinner_frame])
        } else {
            "\u{203A} ".to_string()
        };
        let display_text = format!("{prompt_text}{}", self.input.content);

        let input_para = Paragraph::new(display_text)
            .block(input_block)
            .style(Style::default().fg(Color::White));
        frame.render_widget(input_para, chunks[2]);

        // Cursor
        if !self.is_waiting {
            #[allow(clippy::cast_possible_truncation)]
            let prompt_width = 2u16;
            let cx = chunks[2].x + prompt_width + self.input.cursor_pos as u16;
            let cy = chunks[2].y + 1;
            frame.set_cursor_position(Position::new(
                cx.min(chunks[2].right().saturating_sub(1)),
                cy,
            ));
        }

        // ── Status bar ──
        let left_text = "? for shortcuts";
        let effort_symbol = match self.effort_level.as_str() {
            "low" => "\u{25CB}",
            "high" => "\u{25CF}",
            _ => "\u{25D0}",
        };
        let right_text = format!("{effort_symbol} {} \u{00B7} /effort", self.effort_level);

        let bar_width = chunks[3].width as usize;
        let content_len = left_text.len() + right_text.len() + 2;
        let padding = bar_width.saturating_sub(content_len);
        let status_text = format!(
            " {left_text}{}{right_text} ",
            " ".repeat(padding)
        );

        let status = Paragraph::new(status_text)
            .style(Style::default().fg(Color::Rgb(128, 128, 128)));
        frame.render_widget(status, chunks[3]);
    }

    /// Render the welcome box — two-column bordered widget matching the old inline UI.
    fn draw_welcome_box(&self, frame: &mut Frame, area: Rect) {
        use ratatui::widgets::Wrap;

        // Title in the border
        let title = Line::from(vec![
            Span::styled(
                "OpenClaudia",
                Style::default()
                    .fg(Color::Rgb(147, 112, 219))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" v{}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(Color::Rgb(218, 165, 32)),
            ),
        ]);

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(147, 112, 219)));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Two-column layout
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(inner);

        // Left column: greeting, provider, model, cwd
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let greeting = if username.is_empty() {
            "Welcome to OpenClaudia!".to_string()
        } else {
            format!("Welcome back, {username}!")
        };
        let cwd = std::env::current_dir()
            .map(|p| {
                if let Some(home) = dirs::home_dir() {
                    if let Ok(rel) = p.strip_prefix(&home) {
                        return format!("~/{}", rel.display());
                    }
                }
                p.display().to_string()
            })
            .unwrap_or_else(|_| ".".to_string());

        let left = Paragraph::new(vec![
            Line::from(Span::styled(
                greeting,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                format!("Provider: {}", super::capitalize_first(&self.provider)),
                Style::default().fg(Color::Rgb(147, 112, 219)),
            )),
            Line::from(Span::styled(
                format!("Model: {}", self.model),
                Style::default().fg(Color::Rgb(218, 165, 32)),
            )),
            Line::from(Span::styled(cwd, Style::default().fg(Color::DarkGray))),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(left, cols[0]);

        // Right column: tips and recent activity
        let tips = super::get_tips();
        let right = Paragraph::new(vec![
            Line::from(Span::styled(
                "Tips",
                Style::default().fg(Color::Rgb(218, 165, 32)),
            )),
            Line::from(Span::styled(
                tips[0].to_string(),
                Style::default().fg(Color::White),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Recent activity",
                Style::default().fg(Color::Rgb(218, 165, 32)),
            )),
            Line::from(Span::styled(
                "No recent activity",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(right, cols[1]);
    }
}
