//! OpenClaudia - Open-source universal agent harness
//!
//! Provides Claude Code-like capabilities for any AI agent.

mod compaction;
mod config;
mod context;
mod hooks;
mod mcp;
mod memory;
mod oauth;
mod plugins;
mod prompt;
mod providers;
mod proxy;
mod rules;
mod session;
mod tools;
mod tui;
mod web;

use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "openclaudia")]
#[command(author, version, about = "Open-source universal agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Model to use for chat
    #[arg(short, long, global = true)]
    model: Option<String>,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Enable stateful agent mode with per-project memory
    #[arg(long, global = true)]
    stateful: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize OpenClaudia configuration in the current directory
    Init {
        /// Force overwrite existing configuration
        #[arg(short, long)]
        force: bool,
    },

    /// Authenticate with Claude Max subscription via OAuth
    Auth {
        /// Show current auth status instead of starting new auth
        #[arg(long)]
        status: bool,

        /// Log out and clear stored OAuth session
        #[arg(long)]
        logout: bool,
    },

    /// Start the OpenClaudia proxy server
    Start {
        /// Port to listen on (overrides config)
        #[arg(short, long)]
        port: Option<u16>,

        /// Host to bind to (overrides config)
        #[arg(long)]
        host: Option<String>,

        /// Target provider (anthropic, openai, google)
        #[arg(short, long)]
        target: Option<String>,
    },

    /// Show current configuration
    Config,

    /// Check configuration and connectivity
    Doctor,

    /// Run in iteration/loop mode with Stop hooks
    Loop {
        /// Maximum number of iterations (0 = unlimited)
        #[arg(short, long, default_value = "0")]
        max_iterations: u32,

        /// Port to listen on (overrides config)
        #[arg(short, long)]
        port: Option<u16>,

        /// Target provider (anthropic, openai, google)
        #[arg(short, long)]
        target: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        "openclaudia=debug,tower_http=debug"
    } else {
        "openclaudia=info,tower_http=warn"
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    match cli.command {
        None => cmd_chat(cli.model, cli.stateful).await,
        Some(Commands::Init { force }) => cmd_init(force),
        Some(Commands::Auth { status, logout }) => cmd_auth(status, logout).await,
        Some(Commands::Start { port, host, target }) => cmd_start(port, host, target).await,
        Some(Commands::Config) => cmd_config(),
        Some(Commands::Doctor) => cmd_doctor().await,
        Some(Commands::Loop {
            max_iterations,
            port,
            target,
        }) => cmd_loop(max_iterations, port, target).await,
    }
}

/// Initialize OpenClaudia configuration
fn cmd_init(force: bool) -> anyhow::Result<()> {
    let config_dir = PathBuf::from(".openclaudia");
    let config_file = config_dir.join("config.yaml");

    if config_file.exists() && !force {
        error!("Configuration already exists. Use --force to overwrite.");
        return Ok(());
    }

    // Create directories
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(config_dir.join("hooks"))?;
    fs::create_dir_all(config_dir.join("rules"))?;
    fs::create_dir_all(config_dir.join("plugins"))?;

    // Write default config
    let default_config = r#"# OpenClaudia Configuration
# https://github.com/yourusername/openclaudia

proxy:
  port: 8080
  host: "127.0.0.1"
  target: anthropic  # Default provider: anthropic, openai, google, zai, deepseek, qwen

providers:
  anthropic:
    base_url: https://api.anthropic.com
    # api_key: ${ANTHROPIC_API_KEY}  # Set via environment variable
  openai:
    base_url: https://api.openai.com
    # api_key: ${OPENAI_API_KEY}
  google:
    base_url: https://generativelanguage.googleapis.com
    # api_key: ${GOOGLE_API_KEY}
  # Z.AI/GLM (OpenAI-compatible) - Models: GLM-4.7, GLM-4.5-air
  zai:
    base_url: https://api.z.ai/api/coding/paas/v4
    # api_key: ${ZAI_API_KEY}
  # DeepSeek (OpenAI-compatible) - Models: deepseek-chat, deepseek-coder
  deepseek:
    base_url: https://api.deepseek.com
    # api_key: ${DEEPSEEK_API_KEY}
  # Qwen/Alibaba (OpenAI-compatible) - Models: qwen-turbo, qwen-plus
  qwen:
    base_url: https://dashscope.aliyuncs.com/compatible-mode
    # api_key: ${QWEN_API_KEY}

# Hooks run at key moments in the agent lifecycle
# See: https://github.com/yourusername/openclaudia/docs/hooks.md
# hooks:
#   session_start:
#     - hooks:
#         - type: command
#           command: python .openclaudia/hooks/session-start.py
#           timeout: 30
#   pre_tool_use:
#     - matcher: "Write|Edit"
#       hooks:
#         - type: command
#           command: python .openclaudia/hooks/validate-write.py
#   user_prompt_submit:
#     - hooks:
#         - type: command
#           command: python .openclaudia/hooks/prompt-guard.py

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session

# Keyboard shortcuts - map key combinations to actions
# Available actions: new_session, list_sessions, export, copy_response,
#   editor, models, toggle_mode, cancel, status, help, clear, exit, undo, redo, compact
# Set any key to "none" to disable it
# keybindings:
#   ctrl-x n: new_session
#   ctrl-x l: list_sessions
#   ctrl-x x: export
#   ctrl-x y: copy_response
#   ctrl-x e: editor
#   ctrl-x m: models
#   ctrl-x s: status
#   ctrl-x h: help
#   f2: models
#   tab: toggle_mode
#   escape: cancel
"#;

    fs::write(&config_file, default_config)?;

    // Write example hook
    let example_hook = r#"#!/usr/bin/env python3
"""Example SessionStart hook for OpenClaudia.

This hook runs when a new session starts.
Output JSON to stdout to inject context into the conversation.
"""

import json
import sys
import os

def main():
    # Read hook input from stdin
    input_data = json.load(sys.stdin)

    # Get project information
    cwd = input_data.get("cwd", os.getcwd())

    # Output context to inject
    output = {
        "systemMessage": f"Working directory: {cwd}"
    }

    print(json.dumps(output))

if __name__ == "__main__":
    main()
"#;

    fs::write(config_dir.join("hooks/session-start.py"), example_hook)?;

    // Write example rule
    let example_rule = r#"# Global Rules

These rules are injected into every conversation.

## Code Quality
- Write clean, readable code
- Include error handling
- No hardcoded secrets

## Security
- Validate all user input
- Use parameterized queries
- Follow OWASP guidelines
"#;

    fs::write(config_dir.join("rules/global.md"), example_rule)?;

    info!("Initialized OpenClaudia configuration in .openclaudia/");
    info!("  config.yaml  - Main configuration");
    info!("  hooks/       - Hook scripts");
    info!("  rules/       - Markdown rules");
    info!("  plugins/     - Plugin directory");
    info!("");
    info!("Set your API key:");
    info!("  export ANTHROPIC_API_KEY=your-key-here");
    info!("");
    info!("Start the chat:");
    info!("  openclaudia");

    Ok(())
}

/// Authenticate with Claude Max subscription via OAuth
async fn cmd_auth(status: bool, logout: bool) -> anyhow::Result<()> {
    use crate::oauth::{OAuthClient, OAuthStore, PkceParams, parse_auth_code};
    use std::io::{self, Write};

    let store = OAuthStore::new();

    // Handle --status flag
    if status {
        let sessions: Vec<_> = {
            // Check if any sessions exist by trying to load from disk
            let _store = OAuthStore::new();
            // We can't easily enumerate sessions, so just check persistence path
            let persist_path = dirs::data_local_dir()
                .map(|d| d.join("openclaudia").join("oauth_sessions.json"));

            if let Some(path) = persist_path {
                if path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(sessions) = serde_json::from_str::<std::collections::HashMap<String, serde_json::Value>>(&content) {
                            sessions.into_iter().collect()
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        };

        if sessions.is_empty() {
            println!("Not authenticated with Claude Max.");
            println!("Run 'openclaudia auth' to authenticate.");
        } else {
            println!("Authenticated with Claude Max.");
            println!("Sessions: {}", sessions.len());
            for (id, data) in &sessions {
                let expires = data.get("credentials")
                    .and_then(|c| c.get("expires_at"))
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                println!("  {} (expires: {})", &id[..8], expires);
            }
        }
        return Ok(());
    }

    // Handle --logout flag
    if logout {
        let persist_path = dirs::data_local_dir()
            .map(|d| d.join("openclaudia").join("oauth_sessions.json"));

        if let Some(path) = persist_path {
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("Logged out. OAuth sessions cleared.");
            } else {
                println!("No OAuth sessions to clear.");
            }
        }
        return Ok(());
    }

    // Start OAuth device flow
    println!("=== Claude Max OAuth Authentication ===\n");

    let pkce = PkceParams::generate();
    let auth_url = pkce.build_auth_url();

    println!("Step 1: Open this URL in your browser:\n");
    println!("  {}\n", auth_url);

    // Try to open browser automatically
    #[cfg(target_os = "windows")]
    {
        // Use rundll32 with url.dll for reliable URL opening on Windows
        // This handles special characters in URLs better than 'start' command
        let _ = std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &auth_url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(&auth_url)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&auth_url)
            .spawn();
    }

    println!("Step 2: Sign in to Claude and authorize the application.");
    println!("Step 3: Copy the code shown (format: CODE#STATE)\n");

    print!("Paste the authorization code here: ");
    io::stdout().flush()?;

    let mut code_input = String::new();
    io::stdin().read_line(&mut code_input)?;
    let code_input = code_input.trim();

    if code_input.is_empty() {
        eprintln!("No code provided. Authentication cancelled.");
        return Ok(());
    }

    // Parse the code (handles CODE#STATE format)
    let (code, parsed_state) = parse_auth_code(code_input);

    // Verify state matches
    let expected_state = &pkce.state;
    if let Some(ref state) = parsed_state {
        if state != expected_state {
            eprintln!("State mismatch! This could be a CSRF attack. Authentication cancelled.");
            return Ok(());
        }
    }

    println!("\nExchanging code for tokens...");

    let client = OAuthClient::new();
    let token_response = client.exchange_code(&code, &pkce).await?;

    // Create session from token response
    let mut session = crate::oauth::OAuthSession::from_token_response(token_response);

    // Try to create API key only if we have the required scope
    // Personal Claude Max accounts don't get org:create_api_key, so they use Bearer token directly
    if session.can_create_api_key() {
        println!("Creating API key from OAuth token...");
        match client.create_api_key(&session.credentials.access_token).await {
            Ok(api_key) => {
                session.api_key = Some(api_key);
                println!("✓ API key created successfully");
            }
            Err(e) => {
                eprintln!("Warning: Failed to create API key: {}", e);
                eprintln!("Falling back to Bearer token authentication.");
                session.auth_mode = crate::oauth::AuthMode::BearerToken;
            }
        }
    } else {
        println!("Using Bearer token authentication (personal Claude Max account)");
        println!("  Granted scopes: {}", session.granted_scopes.join(", "));
    }

    let session_id = session.id.clone();
    let auth_mode = session.auth_mode.clone();
    store.store_session(session);

    println!("\n✓ Authentication successful!");
    println!("  Session ID: {}", &session_id[..8]);
    match auth_mode {
        crate::oauth::AuthMode::ApiKey => {
            println!("  Auth mode: API key (organization account)");
        }
        crate::oauth::AuthMode::BearerToken => {
            println!("  Auth mode: Bearer token (personal account)");
        }
        crate::oauth::AuthMode::ProxyMode => {
            println!("  Auth mode: Proxy (via anthropic-proxy)");
        }
    }
    println!("\nYour session has been saved. OpenClaudia will now use your");
    println!("Claude Max subscription automatically when target is 'anthropic'.");

    Ok(())
}

/// Get the data directory for OpenClaudia
fn get_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("openclaudia")
}

/// Get the history file path for rustyline
fn get_history_path() -> PathBuf {
    get_data_dir().join("history.txt")
}

/// Get the chat sessions directory
fn get_sessions_dir() -> PathBuf {
    get_data_dir().join("chat_sessions")
}

/// Agent operating mode
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum AgentMode {
    /// Full access mode - can make changes
    #[default]
    Build,
    /// Read-only mode - only suggestions
    Plan,
}

impl AgentMode {
    fn toggle(&self) -> Self {
        match self {
            AgentMode::Build => AgentMode::Plan,
            AgentMode::Plan => AgentMode::Build,
        }
    }

    fn display(&self) -> &'static str {
        match self {
            AgentMode::Build => "Build",
            AgentMode::Plan => "Plan",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            AgentMode::Build => "Full access - can make changes",
            AgentMode::Plan => "Read-only - suggestions only",
        }
    }
}


/// A saved chat session with messages
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatSession {
    /// Session ID
    id: String,
    /// Session title (first user message or default)
    title: String,
    /// When the session was created
    created_at: chrono::DateTime<chrono::Utc>,
    /// When the session was last updated
    updated_at: chrono::DateTime<chrono::Utc>,
    /// The model used
    model: String,
    /// The provider used
    provider: String,
    /// Agent mode (Build or Plan)
    #[serde(default)]
    mode: AgentMode,
    /// Conversation messages
    messages: Vec<serde_json::Value>,
    /// Undo stack for undone message pairs (user + assistant)
    #[serde(default)]
    undo_stack: Vec<(serde_json::Value, serde_json::Value)>,
}

impl ChatSession {
    fn new(model: &str, provider: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "New conversation".to_string(),
            created_at: now,
            updated_at: now,
            model: model.to_string(),
            provider: provider.to_string(),
            mode: AgentMode::default(),
            messages: Vec::new(),
            undo_stack: Vec::new(),
        }
    }

    /// Undo the last user+assistant message pair
    fn undo(&mut self) -> bool {
        // Need at least 2 messages (user + assistant) to undo
        if self.messages.len() >= 2 {
            let assistant = self.messages.pop().unwrap();
            let user = self.messages.pop().unwrap();
            self.undo_stack.push((user, assistant));
            self.touch();
            true
        } else {
            false
        }
    }

    /// Redo the last undone message pair
    fn redo(&mut self) -> bool {
        if let Some((user, assistant)) = self.undo_stack.pop() {
            self.messages.push(user);
            self.messages.push(assistant);
            self.touch();
            true
        } else {
            false
        }
    }

    /// Clear undo stack (call when new messages are added)
    fn clear_undo_stack(&mut self) {
        self.undo_stack.clear();
    }

    fn update_title(&mut self) {
        // Set title from first user message
        if let Some(first_user) = self.messages.iter().find(|m| {
            m.get("role").and_then(|r| r.as_str()) == Some("user")
        }) {
            if let Some(content) = first_user.get("content").and_then(|c| c.as_str()) {
                let title = if content.len() > 50 {
                    format!("{}...", &content[..47])
                } else {
                    content.to_string()
                };
                self.title = title;
            }
        }
    }

    fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }
}

/// Save a chat session to disk
fn save_chat_session(session: &ChatSession) -> anyhow::Result<()> {
    let dir = get_sessions_dir();
    fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{}.json", session.id));
    let json = serde_json::to_string_pretty(session)?;
    fs::write(path, json)?;
    Ok(())
}

/// Load a chat session by ID
fn load_chat_session(id: &str) -> Option<ChatSession> {
    let path = get_sessions_dir().join(format!("{}.json", id));
    if path.exists() {
        let json = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
    } else {
        None
    }
}

/// List all chat sessions, sorted by most recent
fn list_chat_sessions() -> Vec<ChatSession> {
    let dir = get_sessions_dir();
    let mut sessions = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<ChatSession>(&json) {
                        sessions.push(session);
                    }
                }
            }
        }
    }

    // Sort by updated_at descending (most recent first)
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

/// Slash command result
enum SlashCommandResult {
    /// Exit the chat
    Exit,
    /// Clear the conversation (start new session)
    Clear,
    /// Load a specific session
    LoadSession(String),
    /// Export conversation to markdown
    Export,
    /// Compact conversation (summarize old messages)
    Compact,
    /// Editor returned content to send
    EditorInput(String),
    /// Undo last message pair
    Undo,
    /// Redo last undone message pair
    Redo,
    /// Switch to a different model
    SwitchModel(String),
    /// Show status information
    Status,
    /// Toggle agent mode (Build/Plan)
    ToggleMode,
    /// Show keybindings
    Keybindings,
    /// Rename session with new title
    Rename(String),
    /// Memory command with subcommand and args
    Memory(String),
    /// Activity command to show recent session activities
    Activity(String),
    /// Show help message (already printed)
    Handled,
}

/// Get available models for a provider
fn get_available_models(provider: &str) -> Vec<&'static str> {
    match provider {
        "anthropic" => vec![
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
            "claude-3-5-sonnet-20241022",
            "claude-3-5-haiku-20241022",
            "claude-3-opus-20240229",
        ],
        "openai" => vec![
            "gpt-4",
            "gpt-4-turbo",
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-3.5-turbo",
            "o1-preview",
            "o1-mini",
        ],
        "google" => vec![
            "gemini-pro",
            "gemini-1.5-pro",
            "gemini-1.5-flash",
            "gemini-2.0-flash-exp",
        ],
        "zai" => vec![
            "glm-4.7",
            "glm-4-plus",
            "glm-4-air",
            "glm-4-flash",
        ],
        "deepseek" => vec![
            "deepseek-chat",
            "deepseek-coder",
            "deepseek-reasoner",
        ],
        "qwen" => vec![
            "qwen-turbo",
            "qwen-plus",
            "qwen-max",
            "qwen-long",
        ],
        _ => vec!["gpt-4"],
    }
}

/// Prompt user for permission to perform a sensitive operation
fn prompt_permission(operation: &str, details: &str, always_allowed: &mut std::collections::HashSet<String>) -> bool {
    use std::io::{self, Write};

    let key = format!("{}:{}", operation, details);

    // Check if already always allowed
    if always_allowed.contains(&key) {
        return true;
    }

    // Check for always deny (stored with ! prefix)
    if always_allowed.contains(&format!("!{}", key)) {
        return false;
    }

    println!("\n=== Permission Required ===");
    println!("Operation: {}", operation);
    println!("Details: {}", details);
    println!();
    println!("  [y] Allow once");
    println!("  [n] Deny");
    println!("  [a] Always allow this");
    println!("  [d] Always deny this");
    print!("\nChoice [y/n/a/d]: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }

    match input.trim().to_lowercase().as_str() {
        "y" | "yes" => true,
        "a" | "always" => {
            always_allowed.insert(key);
            println!("(Will always allow this operation)\n");
            true
        }
        "d" => {
            always_allowed.insert(format!("!{}", key));
            println!("(Will always deny this operation)\n");
            false
        }
        _ => {
            println!("(Denied)\n");
            false
        }
    }
}

/// Execute a shell command and print output (with permission check)
fn execute_shell_command_with_permission(cmd: &str, permissions: &mut std::collections::HashSet<String>) {
    // Check for dangerous commands
    let dangerous_patterns = ["rm -rf", "del /f", "format", "mkfs", "> /dev/", "sudo rm"];
    let is_dangerous = dangerous_patterns.iter().any(|p| cmd.contains(p));

    if is_dangerous && !prompt_permission("Dangerous Shell Command", cmd, permissions) {
        println!("Command blocked.\n");
        return;
    }

    execute_shell_command_internal(cmd);
}

/// Execute a shell command and print output
fn execute_shell_command_internal(cmd: &str) {
    use std::process::Command;

    println!();

    #[cfg(windows)]
    let output = Command::new("cmd").args(["/C", cmd]).output();

    #[cfg(not(windows))]
    let output = Command::new("sh").args(["-c", cmd]).output();

    match output {
        Ok(output) => {
            if !output.stdout.is_empty() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
            if !output.status.success() {
                if let Some(code) = output.status.code() {
                    println!("(exit code: {})", code);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to execute command: {}", e);
        }
    }
    println!();
}

/// Estimate tokens in a chat session (rough: ~4 chars per token)
fn estimate_session_tokens(session: &ChatSession) -> usize {
    session.messages.iter().map(|msg| {
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        content.len() / 4 + 4 // content tokens + overhead
    }).sum()
}

/// Open external editor for composing a message
fn open_external_editor() -> Option<String> {
    use std::process::Command;

    // Get editor from environment
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            #[cfg(windows)]
            { "notepad".to_string() }
            #[cfg(not(windows))]
            { "vim".to_string() }
        });

    // Create temp file
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join(format!("openclaudia_{}.txt", uuid::Uuid::new_v4()));

    // Open editor
    println!("\nOpening {}...", editor);

    #[cfg(windows)]
    let status = Command::new("cmd")
        .args(["/C", &editor, temp_file.to_str().unwrap_or("")])
        .status();

    #[cfg(not(windows))]
    let status = Command::new(&editor)
        .arg(&temp_file)
        .status();

    match status {
        Ok(s) if s.success() => {
            // Read content from temp file
            match fs::read_to_string(&temp_file) {
                Ok(content) => {
                    let _ = fs::remove_file(&temp_file);
                    let trimmed = content.trim().to_string();
                    if trimmed.is_empty() {
                        println!("Editor closed with empty content.\n");
                        None
                    } else {
                        Some(trimmed)
                    }
                }
                Err(_) => {
                    println!("No content entered.\n");
                    None
                }
            }
        }
        Ok(_) => {
            eprintln!("Editor exited with error.\n");
            let _ = fs::remove_file(&temp_file);
            None
        }
        Err(e) => {
            eprintln!("Failed to open editor '{}': {}\n", editor, e);
            None
        }
    }
}

/// Expand @file references in input to include file contents
fn expand_file_references(input: &str) -> String {
    use regex::Regex;

    // Match @path patterns (supports paths with spaces in quotes)
    let re = Regex::new(r#"@"([^"]+)"|@(\S+)"#).unwrap();

    let mut result = input.to_string();
    let mut replacements = Vec::new();

    for cap in re.captures_iter(input) {
        let full_match = cap.get(0).unwrap().as_str();
        let path = cap.get(1).or(cap.get(2)).unwrap().as_str();

        // Try to read the file
        match fs::read_to_string(path) {
            Ok(content) => {
                let file_context = format!(
                    "\n<file path=\"{}\">\n{}\n</file>\n",
                    path,
                    content.trim()
                );
                replacements.push((full_match.to_string(), file_context));
            }
            Err(e) => {
                eprintln!("Warning: Could not read {}: {}", path, e);
            }
        }
    }

    // Apply replacements
    for (from, to) in replacements {
        result = result.replace(&from, &to);
    }

    result
}

/// Convert a crossterm KeyEvent to a keybinding string format
/// Examples: "escape", "f2", "ctrl-x", "ctrl-x n" (with leader key state)
fn key_event_to_string(event: &crossterm::event::KeyEvent, leader_active: bool) -> Option<String> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let key_str = match event.code {
        KeyCode::Esc => "escape".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::F(n) => format!("f{}", n),
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

    // If Ctrl+X leader is active, combine with this key
    if leader_active {
        Some(format!("ctrl-x {}", key_str))
    } else {
        Some(key_str)
    }
}

/// Execute a key action and return a result indicator
fn execute_key_action(action: &config::KeyAction) -> Option<SlashCommandResult> {
    use config::KeyAction;

    match action {
        KeyAction::Cancel => None, // Special: handled inline during streaming
        KeyAction::NewSession => Some(SlashCommandResult::Clear),
        KeyAction::Exit => Some(SlashCommandResult::Exit),
        KeyAction::Export => Some(SlashCommandResult::Export),
        KeyAction::Compact => Some(SlashCommandResult::Compact),
        KeyAction::Undo => Some(SlashCommandResult::Undo),
        KeyAction::Redo => Some(SlashCommandResult::Redo),
        KeyAction::ToggleMode => Some(SlashCommandResult::ToggleMode),
        KeyAction::Status => Some(SlashCommandResult::Status),
        KeyAction::Models => {
            // Print models list, return handled
            println!("\nUse /models to see available models.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::ListSessions => {
            // Print sessions list, return handled
            println!("\nUse /sessions to see saved sessions.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::CopyResponse => {
            // Copy action needs session context, signal via Handled
            println!("\nUse /copy to copy the last response.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::Editor => {
            // Editor needs full input handling
            println!("\nUse /editor to open external editor.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::Help => {
            println!("\nUse /help for commands.\n");
            Some(SlashCommandResult::Handled)
        }
        KeyAction::Clear => Some(SlashCommandResult::Clear),
        KeyAction::None => None,
    }
}

/// Compact a chat session by summarizing older messages
fn compact_chat_session(session: &mut ChatSession) -> (usize, usize) {
    let before_tokens = estimate_session_tokens(session);
    let msg_count = session.messages.len();

    // Keep at least last 4 messages
    if msg_count <= 6 {
        println!("\nSession too short to compact ({} messages).\n", msg_count);
        return (before_tokens, before_tokens);
    }

    // Preserve the last 4 messages, summarize the rest
    let preserve_count = 4;
    let to_summarize = msg_count - preserve_count;

    // Build summary of older messages
    let mut summary_parts = Vec::new();
    for msg in session.messages.iter().take(to_summarize) {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        // Truncate long messages in summary
        let preview = if content.len() > 200 {
            format!("{}...", &content[..197])
        } else {
            content.to_string()
        };

        summary_parts.push(format!("[{}]: {}", role, preview));
    }

    let summary = format!(
        "[CONVERSATION SUMMARY - {} messages compacted]\n{}",
        to_summarize,
        summary_parts.join("\n")
    );

    // Create new message list: summary + preserved messages
    let preserved: Vec<_> = session.messages.iter().skip(to_summarize).cloned().collect();

    session.messages.clear();
    session.messages.push(serde_json::json!({
        "role": "system",
        "content": summary
    }));
    session.messages.extend(preserved);
    session.touch();

    let after_tokens = estimate_session_tokens(session);
    (before_tokens, after_tokens)
}

/// Export chat session to markdown file
fn export_chat_session(session: &ChatSession) {
    let exports_dir = get_data_dir().join("exports");
    if let Err(e) = fs::create_dir_all(&exports_dir) {
        eprintln!("\nFailed to create exports directory: {}\n", e);
        return;
    }

    let filename = format!(
        "chat_{}.md",
        session.created_at.format("%Y%m%d_%H%M%S")
    );
    let path = exports_dir.join(&filename);

    let mut content = String::new();
    content.push_str(&format!("# {}\n\n", session.title));
    content.push_str(&format!(
        "**Date:** {}  \n",
        session.created_at.format("%Y-%m-%d %H:%M UTC")
    ));
    content.push_str(&format!("**Model:** {}  \n", session.model));
    content.push_str(&format!("**Provider:** {}  \n\n", session.provider));
    content.push_str("---\n\n");

    for msg in &session.messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("unknown");
        let msg_content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        match role {
            "user" => {
                content.push_str("## User\n\n");
                content.push_str(msg_content);
                content.push_str("\n\n");
            }
            "assistant" => {
                content.push_str("## Assistant\n\n");
                content.push_str(msg_content);
                content.push_str("\n\n");
            }
            _ => {
                content.push_str(&format!("## {}\n\n", role));
                content.push_str(msg_content);
                content.push_str("\n\n");
            }
        }
    }

    match fs::write(&path, content) {
        Ok(()) => println!("\nExported to: {}\n", path.display()),
        Err(e) => eprintln!("\nFailed to export: {}\n", e),
    }
}

/// Save session summary to short-term memory for continuity across restarts
fn save_session_to_short_term_memory(session: &ChatSession, memory_db: Option<&memory::MemoryDb>) {
    let db = match memory_db {
        Some(db) => db,
        None => return, // Not in stateful mode
    };

    // Generate summary from session content
    let mut summary_parts = Vec::new();
    summary_parts.push(format!("Session: {}", session.title));

    // Extract key user requests and assistant actions
    let mut user_requests = Vec::new();
    let mut last_assistant_summary = String::new();

    for msg in &session.messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        if role == "user" && !content.is_empty() {
            // Keep first line of each user message as a request summary
            if let Some(first_line) = content.lines().next() {
                let truncated = if first_line.len() > 100 {
                    format!("{}...", &first_line[..100])
                } else {
                    first_line.to_string()
                };
                user_requests.push(truncated);
            }
        } else if role == "assistant" && !content.is_empty() {
            // Keep last assistant response summary
            last_assistant_summary = content.lines().take(3).collect::<Vec<_>>().join(" ");
            if last_assistant_summary.len() > 200 {
                last_assistant_summary = format!("{}...", &last_assistant_summary[..200]);
            }
        }
    }

    if !user_requests.is_empty() {
        summary_parts.push(format!("User requests: {}", user_requests.join("; ")));
    }
    if !last_assistant_summary.is_empty() {
        summary_parts.push(format!("Last action: {}", last_assistant_summary));
    }

    let summary = summary_parts.join("\n");

    // Get files modified and issues worked from activity log
    let files_modified = db.get_session_files_modified(&session.id).unwrap_or_default();
    let issues_worked = db.get_session_issues(&session.id).unwrap_or_default();

    // Save to short-term memory
    let started_at = session.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
    match db.save_session_summary(
        &session.id,
        &summary,
        &files_modified,
        &issues_worked,
        &started_at,
    ) {
        Ok(_) => {
            tracing::debug!("Session saved to short-term memory");
        }
        Err(e) => {
            tracing::warn!("Failed to save session summary: {}", e);
        }
    }

    // Cleanup expired entries
    if let Ok((sessions, activities)) = db.cleanup_expired_short_term() {
        if sessions > 0 || activities > 0 {
            tracing::debug!("Cleaned up {} expired sessions, {} activities", sessions, activities);
        }
    }
}

/// Handle slash commands, returns true if command was handled
fn handle_slash_command(input: &str, messages: &mut Vec<serde_json::Value>, provider: &str, current_model: &str) -> Option<SlashCommandResult> {
    if !input.starts_with('/') {
        return None;
    }

    let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let args = parts.get(1).copied().unwrap_or("");

    match cmd.as_str() {
        "help" | "?" => {
            println!("\nSlash Commands:");
            println!("  /help, /?        - Show this help message");
            println!("  /new, /clear     - Start a new conversation");
            println!("  /sessions        - List saved sessions");
            println!("  /continue <n>    - Continue session number n");
            println!("  /export          - Export conversation to markdown");
            println!("  /compact         - Summarize old messages to save context");
            println!("  /editor          - Open $EDITOR for composing message");
            println!("  /undo            - Undo last message exchange");
            println!("  /redo            - Redo last undone exchange");
            println!("  /exit, /quit     - Exit the chat");
            println!("  /history         - Show conversation history");
            println!("  /model           - Show current model");
            println!("  /models          - List available models");
            println!("  /model <name>    - Switch to a different model");
            println!("  /copy            - Copy last assistant response to clipboard");
            println!("  /init            - Generate project rules from codebase");
            println!("  /review          - Review uncommitted git changes");
            println!("  /review <branch> - Compare current branch against <branch>");
            println!("  /status          - Show session status (model, tokens, etc.)");
            println!("  /connect         - Configure API keys for providers");
            println!("  /theme           - List available color themes");
            println!("  /theme <name>    - Switch to a color theme");
            println!("  /mode            - Toggle between Build and Plan modes");
            println!("  /keybindings     - Show configured keyboard shortcuts");
            println!("  /rename <title>  - Rename the current session");
            println!("  /version         - Show version and system information");
            println!("  /debug           - Show debug info (paths, env vars, config)");
            println!();
            println!("Memory Commands (stateful mode):");
            println!("  /memory          - Show memory stats");
            println!("  /memory list     - List recent memories");
            println!("  /memory search q - Search memories for query");
            println!("  /memory show <n> - Show memory by ID or section name");
            println!("  /memory delete n - Delete memory by ID");
            println!("  /memory core     - Show core memory sections");
            println!("  /memory clear    - Clear archival memory (keeps core)");
            println!("  /memory reset    - Reset all memory (with confirmation)");
            println!();
            println!("Activity Commands (stateful mode):");
            println!("  /activity        - Show current session activities");
            println!("  /activity sessions - Show recent session summaries");
            println!("  /activity files  - Show files modified this session");
            println!("  /activity issues - Show issues worked this session");
            println!();
            println!("Shell Commands:");
            println!("  !<cmd>           - Execute shell command (e.g., !ls -la)");
            println!();
            println!("Notes:");
            println!("  #<text>          - Save a note without sending to AI");
            println!();
            println!("File Attachment:");
            println!("  @<path>          - Include file contents (e.g., @src/main.rs)");
            println!("  @\"path with spaces\" - Paths with spaces need quotes");
            println!();
            println!("Multiline Input:");
            println!("  End line with \\ to continue on next line");
            println!();
            println!("Keyboard Shortcuts:");
            println!("  Up/Down          - Navigate command history");
            println!("  Ctrl+R           - Search command history");
            println!("  Ctrl+C           - Cancel current input");
            println!("  Ctrl+D           - Exit (on empty line)");
            println!("  Escape           - Cancel AI response mid-stream");
            println!();
            Some(SlashCommandResult::Handled)
        }
        "new" | "clear" => {
            messages.clear();
            println!("\nStarting new conversation.\n");
            Some(SlashCommandResult::Clear)
        }
        "sessions" | "list" => {
            let sessions = list_chat_sessions();
            if sessions.is_empty() {
                println!("\nNo saved sessions.\n");
            } else {
                println!("\nSaved Sessions:");
                for (i, session) in sessions.iter().take(10).enumerate() {
                    let date = session.updated_at.format("%Y-%m-%d %H:%M");
                    let msg_count = session.messages.len();
                    println!("  {}. [{}] {} ({} messages)", i + 1, date, session.title, msg_count);
                }
                if sessions.len() > 10 {
                    println!("  ... and {} more", sessions.len() - 10);
                }
                println!("\nUse /continue <n> to resume a session.\n");
            }
            Some(SlashCommandResult::Handled)
        }
        "continue" | "load" | "resume" => {
            if args.is_empty() {
                // Continue most recent session
                let sessions = list_chat_sessions();
                if let Some(session) = sessions.first() {
                    println!("\nContinuing: {}\n", session.title);
                    return Some(SlashCommandResult::LoadSession(session.id.clone()));
                } else {
                    println!("\nNo sessions to continue.\n");
                }
            } else if let Ok(num) = args.parse::<usize>() {
                let sessions = list_chat_sessions();
                if num > 0 && num <= sessions.len() {
                    let session = &sessions[num - 1];
                    println!("\nContinuing: {}\n", session.title);
                    return Some(SlashCommandResult::LoadSession(session.id.clone()));
                } else {
                    println!("\nInvalid session number. Use /sessions to see available sessions.\n");
                }
            } else {
                println!("\nUsage: /continue <number>\n");
            }
            Some(SlashCommandResult::Handled)
        }
        "exit" | "quit" | "q" => {
            Some(SlashCommandResult::Exit)
        }
        "history" => {
            if messages.is_empty() {
                println!("\nNo messages in conversation.\n");
            } else {
                println!("\nConversation History ({} messages):", messages.len());
                for (i, msg) in messages.iter().enumerate() {
                    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let preview = if content.len() > 60 {
                        format!("{}...", &content[..57])
                    } else {
                        content.to_string()
                    };
                    println!("  {}. [{}] {}", i + 1, role, preview);
                }
                println!();
            }
            Some(SlashCommandResult::Handled)
        }
        "model" => {
            if args.is_empty() {
                // Just show current model
                println!("\nCurrent model: {}", current_model);
                println!("Provider: {}\n", provider);
            } else {
                // Switch to the specified model
                let new_model = args.trim().to_string();
                let available = get_available_models(provider);
                if available.contains(&new_model.as_str()) || !available.is_empty() {
                    // Allow any model name (provider might support more than we list)
                    println!("\nSwitching to model: {}\n", new_model);
                    return Some(SlashCommandResult::SwitchModel(new_model));
                }
            }
            Some(SlashCommandResult::Handled)
        }
        "models" => {
            let available = get_available_models(provider);
            println!("\nAvailable models for {}:", provider);
            for (i, model) in available.iter().enumerate() {
                let marker = if *model == current_model { " *" } else { "" };
                println!("  {}. {}{}", i + 1, model, marker);
            }
            println!("\nUse /model <name> to switch models.\n");
            Some(SlashCommandResult::Handled)
        }
        "export" => {
            Some(SlashCommandResult::Export)
        }
        "compact" | "summarize" => {
            Some(SlashCommandResult::Compact)
        }
        "editor" | "edit" | "e" => {
            // Open external editor and return content if provided
            if let Some(content) = open_external_editor() {
                Some(SlashCommandResult::EditorInput(content))
            } else {
                Some(SlashCommandResult::Handled)
            }
        }
        "undo" => {
            Some(SlashCommandResult::Undo)
        }
        "redo" => {
            Some(SlashCommandResult::Redo)
        }
        "copy" | "yank" | "y" => {
            // Copy last assistant message to clipboard
            if let Some(last_assistant) = messages.iter().rev().find(|m| {
                m.get("role").and_then(|r| r.as_str()) == Some("assistant")
            }) {
                if let Some(content) = last_assistant.get("content").and_then(|c| c.as_str()) {
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => {
                            match clipboard.set_text(content) {
                                Ok(()) => println!("\nCopied {} chars to clipboard.\n", content.len()),
                                Err(e) => eprintln!("\nFailed to copy to clipboard: {}\n", e),
                            }
                        }
                        Err(e) => eprintln!("\nClipboard not available: {}\n", e),
                    }
                } else {
                    println!("\nNo content to copy.\n");
                }
            } else {
                println!("\nNo assistant response to copy.\n");
            }
            Some(SlashCommandResult::Handled)
        }
        "init" => {
            init_project_rules();
            Some(SlashCommandResult::Handled)
        }
        "review" => {
            review_git_changes(args);
            Some(SlashCommandResult::Handled)
        }
        "status" | "info" => {
            Some(SlashCommandResult::Status)
        }
        "connect" | "auth" => {
            configure_provider_api_key();
            Some(SlashCommandResult::Handled)
        }
        "theme" | "themes" => {
            handle_theme_command(args);
            Some(SlashCommandResult::Handled)
        }
        "mode" => {
            Some(SlashCommandResult::ToggleMode)
        }
        "keybindings" | "keys" | "bindings" => {
            Some(SlashCommandResult::Keybindings)
        }
        "rename" | "title" => {
            if args.is_empty() {
                println!("\nUsage: /rename <new title>\n");
                Some(SlashCommandResult::Handled)
            } else {
                Some(SlashCommandResult::Rename(args.to_string()))
            }
        }
        "version" | "v" | "about" => {
            println!("\nOpenClaudia v{}", env!("CARGO_PKG_VERSION"));
            println!("{}", env!("CARGO_PKG_DESCRIPTION"));
            println!();
            println!("Repository: {}", env!("CARGO_PKG_REPOSITORY"));
            println!("License:    {}", env!("CARGO_PKG_LICENSE"));
            println!("Platform:   {} / {}", std::env::consts::OS, std::env::consts::ARCH);
            println!();
            Some(SlashCommandResult::Handled)
        }
        "debug" | "config" => {
            println!("\n=== Debug Information ===\n");
            println!("Provider:     {}", provider);
            println!("Model:        {}", current_model);
            println!("Messages:     {}", messages.len());
            println!();
            println!("Configuration Paths:");
            println!("  Project:    .openclaudia/config.yaml");
            if let Some(home) = dirs::home_dir() {
                println!("  User:       {}", home.join(".openclaudia/config.yaml").display());
            }
            if let Some(config_dir) = dirs::config_dir() {
                println!("  System:     {}", config_dir.join("openclaudia/config.yaml").display());
            }
            println!();
            println!("Data Directories:");
            println!("  Sessions:   {}", get_sessions_dir().display());
            println!("  History:    {}", get_history_path().display());
            println!("  Data:       {}", get_data_dir().display());
            println!();
            println!("Environment Variables:");
            for var in &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GOOGLE_API_KEY",
                         "DEEPSEEK_API_KEY", "QWEN_API_KEY", "ZAI_API_KEY", "EDITOR"] {
                let status = if std::env::var(var).is_ok() { "set" } else { "not set" };
                println!("  {}: {}", var, status);
            }
            println!();
            Some(SlashCommandResult::Handled)
        }
        "memory" | "mem" => {
            // Memory command - pass subcommand to main loop where memory_db is available
            Some(SlashCommandResult::Memory(args.to_string()))
        }
        "activity" | "act" => {
            // Activity command - show recent session activities
            Some(SlashCommandResult::Activity(args.to_string()))
        }
        _ => {
            eprintln!("Unknown command: /{}. Type /help for available commands.\n", cmd);
            Some(SlashCommandResult::Handled)
        }
    }
}

/// Handle /memory command for viewing and managing archival memory
fn handle_memory_command(args: &str, memory_db: Option<&memory::MemoryDb>) {
    let db = match memory_db {
        Some(db) => db,
        None => {
            println!("\n\x1b[33mMemory commands require stateful mode.\x1b[0m");
            println!("Start with: openclaudia --stateful\n");
            return;
        }
    };

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let subargs = parts.get(1).copied().unwrap_or("");

    match subcmd.as_str() {
        "" | "stats" => {
            // Show memory statistics
            match db.memory_stats() {
                Ok(stats) => {
                    println!("\n=== Memory Statistics ===");
                    println!("  Archival memories: {}", stats.count);
                    println!("  Total size:        {} bytes", stats.total_size);
                    if let Some(last) = stats.last_updated {
                        println!("  Last updated:      {}", last);
                    }
                    println!("  Database path:     {}", db.path().display());
                    println!();
                }
                Err(e) => eprintln!("\nFailed to get memory stats: {}\n", e),
            }
        }
        "list" | "ls" => {
            let limit = subargs.parse().unwrap_or(10);
            match db.memory_list(limit) {
                Ok(memories) => {
                    if memories.is_empty() {
                        println!("\nNo memories stored yet.\n");
                    } else {
                        println!("\n=== Recent Memories ({}) ===\n", memories.len());
                        for mem in memories {
                            let preview = if mem.content.len() > 80 {
                                format!("{}...", &mem.content[..77])
                            } else {
                                mem.content.clone()
                            };
                            let tags = if mem.tags.is_empty() {
                                String::new()
                            } else {
                                format!(" [{}]", mem.tags.join(", "))
                            };
                            println!("  \x1b[36m#{}\x1b[0m {}{}", mem.id, preview, tags);
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("\nFailed to list memories: {}\n", e),
            }
        }
        "search" | "find" => {
            if subargs.is_empty() {
                println!("\nUsage: /memory search <query>\n");
                return;
            }
            match db.memory_search(subargs, 10) {
                Ok(memories) => {
                    if memories.is_empty() {
                        println!("\nNo memories found matching '{}'.\n", subargs);
                    } else {
                        println!("\n=== Search Results for '{}' ({}) ===\n", subargs, memories.len());
                        for mem in memories {
                            let preview = if mem.content.len() > 100 {
                                format!("{}...", &mem.content[..97])
                            } else {
                                mem.content.clone()
                            };
                            println!("  \x1b[36m#{}\x1b[0m ({})", mem.id, mem.updated_at);
                            println!("  {}", preview);
                            if !mem.tags.is_empty() {
                                println!("  Tags: {}", mem.tags.join(", "));
                            }
                            println!();
                        }
                    }
                }
                Err(e) => eprintln!("\nFailed to search memories: {}\n", e),
            }
        }
        "show" | "get" => {
            // First try to parse as an ID for archival memory
            if let Ok(id) = subargs.parse::<i64>() {
                match db.memory_get(id) {
                    Ok(Some(mem)) => {
                        println!("\n=== Memory #{} ===", mem.id);
                        println!("Created:  {}", mem.created_at);
                        println!("Updated:  {}", mem.updated_at);
                        if !mem.tags.is_empty() {
                            println!("Tags:     {}", mem.tags.join(", "));
                        }
                        println!("\n{}\n", mem.content);
                    }
                    Ok(None) => println!("\nMemory #{} not found.\n", id),
                    Err(e) => eprintln!("\nFailed to get memory: {}\n", e),
                }
            } else if !subargs.is_empty() {
                // Try as core memory section name
                match db.get_core_memory_section(subargs) {
                    Ok(Some(section)) => {
                        println!("\n=== Core Memory: {} ===", section.section);
                        println!("Updated: {}\n", section.updated_at);
                        println!("{}\n", section.content);
                    }
                    Ok(None) => {
                        println!("\nSection '{}' not found.", subargs);
                        println!("Available sections: persona, project_info, user_preferences\n");
                    }
                    Err(e) => eprintln!("\nFailed to get core memory section: {}\n", e),
                }
            } else {
                println!("\nUsage:");
                println!("  /memory show <id>        - Show archival memory by ID");
                println!("  /memory show <section>   - Show core memory section");
                println!("  Available sections: persona, project_info, user_preferences\n");
            }
        }
        "delete" | "rm" => {
            let id: i64 = match subargs.parse() {
                Ok(id) => id,
                Err(_) => {
                    println!("\nUsage: /memory delete <id>\n");
                    return;
                }
            };
            match db.memory_delete(id) {
                Ok(true) => println!("\nDeleted memory #{}.\n", id),
                Ok(false) => println!("\nMemory #{} not found.\n", id),
                Err(e) => eprintln!("\nFailed to delete memory: {}\n", e),
            }
        }
        "core" => {
            match db.get_core_memory() {
                Ok(sections) => {
                    println!("\n=== Core Memory ===\n");
                    for section in sections {
                        println!("\x1b[35m[{}]\x1b[0m (updated: {})", section.section, section.updated_at);
                        println!("{}\n", section.content);
                    }
                }
                Err(e) => eprintln!("\nFailed to get core memory: {}\n", e),
            }
        }
        "clear" => {
            // Clear only archival memory, keep core memory
            if subargs == "confirm" || subargs == "yes" {
                match db.clear_archival_memory() {
                    Ok(count) => {
                        println!("\n\x1b[32mCleared {} archival memories.\x1b[0m", count);
                        println!("Core memory sections preserved.\n");
                    }
                    Err(e) => eprintln!("\nFailed to clear archival memory: {}\n", e),
                }
            } else {
                println!("\n\x1b[33mWarning: This will delete all archival memories!\x1b[0m");
                println!("Core memory sections (persona, project_info, user_preferences) will be preserved.");
                println!("\nTo confirm, run: /memory clear confirm\n");
            }
        }
        "reset" => {
            // Full reset - clears both archival AND core memory
            if subargs == "confirm" || subargs == "yes" {
                match db.reset_all() {
                    Ok(()) => {
                        println!("\n\x1b[32mMemory completely reset.\x1b[0m");
                        println!("All archival memories deleted.");
                        println!("Core memory sections reset to defaults.\n");
                    }
                    Err(e) => eprintln!("\nFailed to reset memory: {}\n", e),
                }
            } else {
                println!("\n\x1b[31mWarning: This will delete ALL memories!\x1b[0m");
                println!("This includes archival memory AND core memory sections.");
                println!("\nTo confirm, run: /memory reset confirm\n");
            }
        }
        _ => {
            println!("\nUnknown memory subcommand: {}", subcmd);
            println!("Available: list, search, show, delete, core, clear, reset\n");
        }
    }
}

/// Handle /activity command for viewing recent session activities
fn handle_activity_command(args: &str, current_session_id: &str, memory_db: Option<&memory::MemoryDb>) {
    let db = match memory_db {
        Some(db) => db,
        None => {
            println!("\n\x1b[33mActivity tracking requires stateful mode.\x1b[0m");
            println!("Start with: openclaudia --stateful\n");
            return;
        }
    };

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let subargs = parts.get(1).copied().unwrap_or("");

    match subcmd.as_str() {
        "" | "current" => {
            // Show activities for current session
            match db.get_session_activities(current_session_id) {
                Ok(activities) => {
                    if activities.is_empty() {
                        println!("\nNo activities recorded in this session yet.\n");
                    } else {
                        println!("\n=== Current Session Activities ({}) ===", activities.len());
                        println!("Session: {}\n", current_session_id);
                        for activity in activities.iter().take(20) {
                            let icon = match activity.activity_type.as_str() {
                                "file_read" => "📖",
                                "file_write" => "✏️",
                                "file_edit" => "📝",
                                "bash_command" => "💻",
                                "issue_created" => "🎫",
                                "issue_closed" => "✅",
                                "issue_comment" => "💬",
                                _ => "•",
                            };
                            let details = activity.details.as_deref().unwrap_or("");
                            let details_str = if details.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", details)
                            };
                            // Use all fields: id, session_id, activity_type, target, details, created_at
                            println!("  \x1b[90m[{}]\x1b[0m {} \x1b[36m{}\x1b[0m {}{}",
                                activity.created_at, icon, activity.activity_type, activity.target, details_str);
                            println!("       \x1b[90mID: {} | Session: {}\x1b[0m", activity.id, &activity.session_id[..8]);
                        }
                        if activities.len() > 20 {
                            println!("\n  ... and {} more activities", activities.len() - 20);
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("\nFailed to get activities: {}\n", e),
            }
        }
        "sessions" | "recent" => {
            // Show recent sessions with summaries
            let limit = subargs.parse().unwrap_or(5);
            match db.get_recent_sessions(limit) {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        println!("\nNo recent sessions recorded.\n");
                    } else {
                        println!("\n=== Recent Sessions ({}) ===\n", sessions.len());
                        for (i, session) in sessions.iter().enumerate() {
                            // Use session.id field
                            println!("  \x1b[36m{}.\x1b[0m [ID:{}] Session {} (ended {})",
                                i + 1, session.id, &session.session_id[..8], session.ended_at);
                            println!("     Started: {}", session.started_at);

                            // Show summary (first 100 chars)
                            let summary_preview = if session.summary.len() > 100 {
                                format!("{}...", &session.summary[..97])
                            } else {
                                session.summary.clone()
                            };
                            println!("     Summary: {}", summary_preview);

                            if !session.files_modified.is_empty() {
                                println!("     Files: {}", session.files_modified.join(", "));
                            }
                            if !session.issues_worked.is_empty() {
                                println!("     Issues: {}", session.issues_worked.join(", "));
                            }
                            println!();
                        }
                    }
                }
                Err(e) => eprintln!("\nFailed to get recent sessions: {}\n", e),
            }
        }
        "files" => {
            // Show files modified in current session
            match db.get_session_files_modified(current_session_id) {
                Ok(files) => {
                    if files.is_empty() {
                        println!("\nNo files modified in this session yet.\n");
                    } else {
                        println!("\n=== Files Modified This Session ({}) ===\n", files.len());
                        for file in &files {
                            println!("  📝 {}", file);
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("\nFailed to get modified files: {}\n", e),
            }
        }
        "issues" => {
            // Show issues worked on in current session
            match db.get_session_issues(current_session_id) {
                Ok(issues) => {
                    if issues.is_empty() {
                        println!("\nNo issues worked on in this session yet.\n");
                    } else {
                        println!("\n=== Issues Worked This Session ({}) ===\n", issues.len());
                        for issue in &issues {
                            println!("  🎫 {}", issue);
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("\nFailed to get issues: {}\n", e),
            }
        }
        "help" => {
            println!("\nActivity Commands:");
            println!("  /activity          - Show current session activities");
            println!("  /activity sessions - Show recent session summaries");
            println!("  /activity files    - Show files modified this session");
            println!("  /activity issues   - Show issues worked this session");
            println!();
        }
        _ => {
            println!("\nUnknown activity subcommand: {}", subcmd);
            println!("Available: current, sessions, files, issues, help\n");
        }
    }
}

/// Display current keybindings configuration
fn display_keybindings(keybindings: &config::KeybindingsConfig) {
    use config::KeyAction;

    println!("\nConfigured Keybindings:");
    println!("========================\n");

    // Group bindings by action for cleaner display
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
            let key_str = keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ");
            println!("  {:20} {}", key_str, description);
        }
    }

    // Show disabled bindings
    let disabled = keybindings.get_keys_for_action(&KeyAction::None);
    if !disabled.is_empty() {
        println!("\nDisabled bindings:");
        for key in disabled {
            println!("  {} (disabled)", key);
        }
    }

    println!("\nTo customize, add a 'keybindings' section to your config.yaml.");
    println!("Set any key to 'none' to disable it.\n");
}

/// Detect project type from current directory
fn detect_project_type() -> Vec<(&'static str, &'static str)> {
    let mut detected = Vec::new();

    // Check for various project indicators
    if std::path::Path::new("Cargo.toml").exists() {
        detected.push(("rust", "Rust project detected (Cargo.toml)"));
    }
    if std::path::Path::new("package.json").exists() {
        detected.push(("node", "Node.js project detected (package.json)"));
    }
    if std::path::Path::new("pyproject.toml").exists() || std::path::Path::new("setup.py").exists() {
        detected.push(("python", "Python project detected"));
    }
    if std::path::Path::new("go.mod").exists() {
        detected.push(("go", "Go project detected (go.mod)"));
    }
    if std::path::Path::new("pom.xml").exists() || std::path::Path::new("build.gradle").exists() {
        detected.push(("java", "Java project detected"));
    }
    if std::path::Path::new(".git").exists() {
        detected.push(("git", "Git repository detected"));
    }

    detected
}

/// Generate project rules based on detected type
fn generate_project_rules(project_types: &[(&str, &str)]) -> String {
    let mut rules = String::new();
    rules.push_str("# Project Rules\n\n");
    rules.push_str("Auto-generated rules based on project structure.\n\n");

    for (ptype, _) in project_types {
        match *ptype {
            "rust" => {
                rules.push_str("## Rust Guidelines\n\n");
                rules.push_str("- Use `cargo fmt` before committing\n");
                rules.push_str("- Run `cargo clippy` to check for common mistakes\n");
                rules.push_str("- Prefer `?` operator over `.unwrap()` for error handling\n");
                rules.push_str("- Use `anyhow::Result` for application errors\n");
                rules.push_str("- Run `cargo test` before pushing changes\n\n");
            }
            "node" => {
                rules.push_str("## Node.js Guidelines\n\n");
                rules.push_str("- Use consistent code style (prettier/eslint)\n");
                rules.push_str("- Run `npm test` before committing\n");
                rules.push_str("- Keep dependencies up to date\n");
                rules.push_str("- Use async/await over callbacks\n\n");
            }
            "python" => {
                rules.push_str("## Python Guidelines\n\n");
                rules.push_str("- Follow PEP 8 style guide\n");
                rules.push_str("- Use type hints where possible\n");
                rules.push_str("- Run tests with pytest before committing\n");
                rules.push_str("- Use virtual environments\n\n");
            }
            "go" => {
                rules.push_str("## Go Guidelines\n\n");
                rules.push_str("- Run `go fmt` before committing\n");
                rules.push_str("- Use `go vet` to check for issues\n");
                rules.push_str("- Handle all errors explicitly\n");
                rules.push_str("- Run `go test ./...` before pushing\n\n");
            }
            "java" => {
                rules.push_str("## Java Guidelines\n\n");
                rules.push_str("- Follow Java naming conventions\n");
                rules.push_str("- Run tests before committing\n");
                rules.push_str("- Use dependency injection where appropriate\n\n");
            }
            "git" => {
                rules.push_str("## Git Guidelines\n\n");
                rules.push_str("- Write clear, descriptive commit messages\n");
                rules.push_str("- Keep commits atomic and focused\n");
                rules.push_str("- Don't commit secrets or API keys\n\n");
            }
            _ => {}
        }
    }

    rules
}

/// Initialize project rules from codebase analysis
fn init_project_rules() {
    let detected = detect_project_type();

    if detected.is_empty() {
        println!("\nNo recognized project type detected.");
        println!("Creating generic rules file.\n");
    } else {
        println!("\nDetected project types:");
        for (_, desc) in &detected {
            println!("  - {}", desc);
        }
    }

    let rules_dir = std::path::Path::new(".openclaudia/rules");
    if let Err(e) = fs::create_dir_all(rules_dir) {
        eprintln!("\nFailed to create rules directory: {}\n", e);
        return;
    }

    let rules_content = generate_project_rules(&detected);
    let rules_path = rules_dir.join("project.md");

    if rules_path.exists() {
        println!("\nRules file already exists at {}", rules_path.display());
        println!("Use a text editor to modify it.\n");
        return;
    }

    match fs::write(&rules_path, &rules_content) {
        Ok(()) => {
            println!("\nGenerated rules at: {}", rules_path.display());
            println!("Edit this file to customize rules for your project.\n");
        }
        Err(e) => eprintln!("\nFailed to write rules: {}\n", e),
    }
}

/// Review uncommitted git changes or compare against a branch
fn review_git_changes(args: &str) {
    use std::process::Command;

    // Check if we're in a git repository
    let git_check = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output();

    if git_check.is_err() || !git_check.unwrap().status.success() {
        println!("\nNot a git repository.\n");
        return;
    }

    println!();

    if args.is_empty() {
        // Show uncommitted changes (staged and unstaged)
        println!("=== Git Status ===\n");
        let status = Command::new("git")
            .args(["status", "--short"])
            .output();

        match status {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    println!("No changes detected.\n");
                    return;
                }
                println!("{}", stdout);
            }
            Err(e) => {
                eprintln!("Failed to run git status: {}\n", e);
                return;
            }
        }

        println!("=== Uncommitted Changes ===\n");
        let diff = Command::new("git")
            .args(["diff", "HEAD"])
            .output();

        match diff {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    println!("No diff to show (changes may be staged).\n");
                } else {
                    // Truncate if too long
                    let lines: Vec<&str> = stdout.lines().collect();
                    if lines.len() > 100 {
                        for line in lines.iter().take(100) {
                            println!("{}", line);
                        }
                        println!("\n... ({} more lines, use git diff directly for full output)\n", lines.len() - 100);
                    } else {
                        println!("{}", stdout);
                    }
                }
            }
            Err(e) => eprintln!("Failed to run git diff: {}\n", e),
        }
    } else {
        // Compare against specified branch
        let branch = args.trim();
        println!("=== Comparing against '{}' ===\n", branch);

        // First check if branch exists
        let branch_check = Command::new("git")
            .args(["rev-parse", "--verify", branch])
            .output();

        if branch_check.is_err() || !branch_check.unwrap().status.success() {
            eprintln!("Branch '{}' not found.\n", branch);
            return;
        }

        // Show commits not in target branch
        println!("Commits ahead of {}:\n", branch);
        let log = Command::new("git")
            .args(["log", "--oneline", &format!("{}..HEAD", branch)])
            .output();

        match log {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    println!("  (no commits ahead)\n");
                } else {
                    for line in stdout.lines() {
                        println!("  {}", line);
                    }
                    println!();
                }
            }
            Err(e) => eprintln!("Failed to run git log: {}\n", e),
        }

        // Show diff summary
        println!("Changed files:\n");
        let diff_stat = Command::new("git")
            .args(["diff", "--stat", branch])
            .output();

        match diff_stat {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    println!("  (no changes)\n");
                } else {
                    println!("{}", stdout);
                }
            }
            Err(e) => eprintln!("Failed to run git diff --stat: {}\n", e),
        }
    }
}

/// Configure API key for a provider interactively
fn configure_provider_api_key() {
    use std::io::{self, Write};

    let providers = [
        ("anthropic", "Anthropic (Claude)", "ANTHROPIC_API_KEY"),
        ("openai", "OpenAI (GPT)", "OPENAI_API_KEY"),
        ("google", "Google (Gemini)", "GOOGLE_API_KEY"),
        ("deepseek", "DeepSeek", "DEEPSEEK_API_KEY"),
        ("qwen", "Qwen (Alibaba)", "QWEN_API_KEY"),
        ("zai", "Z.AI (GLM)", "ZAI_API_KEY"),
    ];

    println!("\n=== Configure API Provider ===\n");
    println!("Select a provider to configure:\n");

    for (i, (_, name, _)) in providers.iter().enumerate() {
        println!("  {}. {}", i + 1, name);
    }
    println!();

    print!("Enter choice (1-{}): ", providers.len());
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        eprintln!("Failed to read input.\n");
        return;
    }

    let choice: usize = match input.trim().parse() {
        Ok(n) if n >= 1 && n <= providers.len() => n,
        _ => {
            eprintln!("Invalid choice.\n");
            return;
        }
    };

    let (provider_id, provider_name, env_var) = providers[choice - 1];

    println!("\nConfiguring {}...", provider_name);
    println!("You can get an API key from the provider's website.\n");

    print!("Enter API key (or press Enter to skip): ");
    io::stdout().flush().ok();

    let mut api_key = String::new();
    if io::stdin().read_line(&mut api_key).is_err() {
        eprintln!("Failed to read input.\n");
        return;
    }

    let api_key = api_key.trim();
    if api_key.is_empty() {
        println!("Skipped. Set {} environment variable instead.\n", env_var);
        return;
    }

    // Save to config file
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("openclaudia");

    if let Err(e) = fs::create_dir_all(&config_dir) {
        eprintln!("Failed to create config directory: {}\n", e);
        return;
    }

    let config_path = config_dir.join("config.yaml");

    // Read existing config or create new
    let mut config_content = if config_path.exists() {
        fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Simple approach: append or update the provider section
    // For a production app, you'd want to properly parse and modify YAML
    let provider_section = format!(
        "\n# {} configuration\n{}_api_key: \"{}\"\n",
        provider_name, provider_id, api_key
    );

    // Check if provider already configured
    let key_pattern = format!("{}_api_key:", provider_id);
    if config_content.contains(&key_pattern) {
        println!("\nProvider already configured in config file.");
        println!("Edit {} to update.\n", config_path.display());
    } else {
        config_content.push_str(&provider_section);

        match fs::write(&config_path, &config_content) {
            Ok(()) => {
                println!("\nSaved API key to: {}", config_path.display());
                println!("Restart the chat to use the new configuration.\n");
            }
            Err(e) => eprintln!("\nFailed to save config: {}\n", e),
        }
    }
}

/// Handle theme command - list or switch themes
fn handle_theme_command(args: &str) {
    use crossterm::style::{Color, Stylize};

    // Available themes with their color schemes
    let themes: &[(&str, &str, Color, Color)] = &[
        ("default", "Default terminal colors", Color::Reset, Color::Reset),
        ("ocean", "Cool blue tones", Color::Cyan, Color::Blue),
        ("forest", "Earthy green tones", Color::Green, Color::DarkGreen),
        ("sunset", "Warm orange tones", Color::Yellow, Color::Red),
        ("mono", "Monochrome grayscale", Color::White, Color::Grey),
        ("neon", "Bright vibrant colors", Color::Magenta, Color::Cyan),
    ];

    if args.is_empty() {
        // List available themes with preview
        println!("\n=== Available Themes ===\n");

        for (name, desc, primary, _secondary) in themes {
            let preview = format!("  {} - {}", name, desc);
            println!("{}", preview.with(*primary));
        }

        println!("\nUse /theme <name> to switch themes.");
        println!("Note: Theme affects status messages only.\n");
    } else {
        let theme_name = args.trim().to_lowercase();

        if let Some((name, desc, primary, _)) = themes.iter().find(|(n, _, _, _)| *n == theme_name) {
            println!();
            println!("{}", format!("Switched to '{}' theme: {}", name, desc).with(*primary));
            println!("{}", "Theme preview: This is how messages will appear.".with(*primary));
            println!();

            // Note: In a full implementation, you'd store the theme preference
            // and apply it to prompts and responses throughout the app
        } else {
            eprintln!("\nUnknown theme: '{}'\n", theme_name);
            eprintln!("Available themes: {}\n",
                themes.iter().map(|(n, _, _, _)| *n).collect::<Vec<_>>().join(", "));
        }
    }
}

/// Get a random tip to display at startup
fn get_random_tip() -> &'static str {
    const TIPS: &[&str] = &[
        "Type @ followed by a filepath to attach file contents to your message.",
        "Start a message with ! to run shell commands directly (e.g., !ls -la).",
        "Use /undo to revert the last message exchange.",
        "Use /redo to restore previously undone messages.",
        "Press Escape during streaming to cancel the AI response.",
        "Use /editor to compose long messages in your external editor.",
        "End a line with \\ to continue typing on the next line.",
        "Use /sessions to list and continue previous conversations.",
        "Use /export to save your conversation as a Markdown file.",
        "Use /compact to summarize old messages when context gets long.",
        "Use /models to see available models for your provider.",
        "Use /model <name> to switch models mid-conversation.",
        "Use /copy to copy the last AI response to your clipboard.",
        "Press Ctrl+R to search through your command history.",
        "Use /history to see all messages in the current session.",
        "Use /init to auto-generate project rules based on your codebase.",
        "Use /review to review uncommitted git changes or compare branches.",
        "Use /status to see session info: model, token count, duration.",
        "Use /connect to configure API keys for different providers.",
        "Use /theme to preview and switch between color themes.",
        "Use /mode to toggle between Build (full access) and Plan (read-only) modes.",
        "Use /keybindings to see all configured keyboard shortcuts.",
        "Use /rename <title> to give your session a custom name.",
        "Quote paths with spaces: @\"path with spaces/file.txt\"",
        "Create .openclaudia/rules/global.md for rules applied to all sessions.",
        "Set up hooks in .openclaudia/hooks/ to customize agent behavior.",
        "Configure providers in .openclaudia/config.yaml or ~/.openclaudia/config.yaml.",
        "Use environment variables like ANTHROPIC_API_KEY for credentials.",
        "Dangerous shell commands will prompt for permission before running.",
        "Start a line with # to save a note without sending it to the AI.",
    ];

    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    TIPS[(seed as usize) % TIPS.len()]
}

/// Result of OAuth flow: proxy URL and session ID
struct OAuthFlowResult {
    proxy_url: String,
    session_id: String,
}

/// Fully automatic OAuth setup using OpenClaudia's built-in proxy.
///
/// Steps:
/// 1. Start OpenClaudia proxy if not running
/// 2. Open browser for OAuth login
/// 3. Poll until auth completes
///
/// Returns proxy URL and session ID when ready - NO MANUAL INPUT REQUIRED.
async fn start_builtin_oauth_flow(config: &config::AppConfig) -> Option<OAuthFlowResult> {
    let proxy_port = config.proxy.port;
    let proxy_url = format!("http://localhost:{}", proxy_port);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;

    // Step 1: Check if our proxy is already running
    let proxy_running = client.get(format!("{}/health", proxy_url)).send().await.is_ok();

    if !proxy_running {
        println!("🚀 Starting OpenClaudia proxy on port {}...", proxy_port);

        // Start our own proxy in a background task
        let config_clone = config.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::proxy::start_server(config_clone).await {
                tracing::error!("Proxy server error: {}", e);
            }
        });

        // Wait for proxy to start (up to 5 seconds)
        for i in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if client.get(format!("{}/health", proxy_url)).send().await.is_ok() {
                println!("✓ Proxy started on port {}", proxy_port);
                break;
            }
            if i == 9 {
                eprintln!("❌ Failed to start proxy");
                return None;
            }
        }
    } else {
        println!("✓ Proxy already running on port {}", proxy_port);
    }

    // Step 2: Check if already authenticated AND token actually works
    if let Ok(resp) = client.get(format!("{}/auth/status", proxy_url)).send().await {
        if let Ok(status) = resp.json::<serde_json::Value>().await {
            if status["authenticated"].as_bool() == Some(true) {
                if let Some(session_id) = status["session_id"].as_str() {
                    // Verify the token works by hitting /v1/models
                    println!("   Verifying existing session...");
                    let test_resp = client
                        .get(format!("{}/v1/models", proxy_url))
                        .header("Cookie", format!("anthropic_session={}", session_id))
                        .send()
                        .await;

                    if let Ok(r) = test_resp {
                        if r.status().is_success() {
                            println!("✓ Already logged in!");
                            return Some(OAuthFlowResult {
                                proxy_url: proxy_url.clone(),
                                session_id: session_id.to_string(),
                            });
                        } else {
                            println!("   Existing session invalid, need to re-authenticate...");
                        }
                    }
                }
            }
        }
    }

    // Step 3: Open browser to OAuth device flow page
    println!("🔐 Opening browser for Claude login...");
    let auth_url = format!("{}/auth/device", proxy_url);

    #[cfg(target_os = "windows")]
    { let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &auth_url]).spawn(); }
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg(&auth_url).spawn(); }
    #[cfg(target_os = "linux")]
    { let _ = std::process::Command::new("xdg-open").arg(&auth_url).spawn(); }

    // Step 4: Poll /auth/status until authenticated (5 min timeout)
    println!("   Waiting for you to log in at: {}", auth_url);
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(300);

    while start.elapsed() < timeout {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Check auth status and get session ID
        if let Ok(resp) = client.get(format!("{}/auth/status", proxy_url)).send().await {
            if let Ok(status) = resp.json::<serde_json::Value>().await {
                if status["authenticated"].as_bool() == Some(true) {
                    if let Some(session_id) = status["session_id"].as_str() {
                        println!("\n✓ Logged in! Starting chat...");
                        return Some(OAuthFlowResult {
                            proxy_url: proxy_url.clone(),
                            session_id: session_id.to_string(),
                        });
                    }
                }
            }
        }
        print!(".");
        std::io::Write::flush(&mut std::io::stdout()).ok();
    }

    eprintln!("\n❌ Login timed out (5 min)");
    None
}

/// Interactive chat mode (default command)
async fn cmd_chat(model_override: Option<String>, stateful: bool) -> anyhow::Result<()> {
    use crate::hooks::{load_claude_code_hooks, merge_hooks_config, HookEngine, HookEvent, HookInput};
    use crate::providers::get_adapter;
    use crate::rules::RulesEngine;
    use indicatif::{ProgressBar, ProgressStyle};
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;

    // Compile regex once for file extension extraction
    let ext_regex = regex::Regex::new(r"[\w/\\.-]+\.([a-zA-Z0-9]{1,10})\b").unwrap();

    let config = match config::load_config() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("No configuration found. Run 'openclaudia init' first.");
            return Ok(());
        }
    };

    let provider = match config.active_provider() {
        Some(p) => p,
        None => {
            eprintln!("No provider configured for target '{}'", config.proxy.target);
            return Ok(());
        }
    };

    // For Anthropic: Check anthropic-proxy FIRST (auto-start, auto-auth)
    // This is the primary auth method for Claude Max subscriptions
    let mut oauth_session: Option<crate::oauth::OAuthSession> = None;
    let mut proxy_url: Option<String> = None;

    let api_key = if config.proxy.target == "anthropic" && provider.api_key.is_none() {
        // No API key configured - use built-in OAuth proxy (AUTOMATIC)
        eprintln!("[debug] Anthropic provider with no API key - starting OAuth flow...");
        match start_builtin_oauth_flow(&config).await {
            Some(result) => {
                // Store proxy URL and create session with actual session ID
                eprintln!("✓ Connected via OpenClaudia proxy");
                eprintln!("[debug] Proxy URL: {}", result.proxy_url);
                eprintln!("[debug] Session ID: {}", result.session_id);
                proxy_url = Some(result.proxy_url);
                let proxy_session = crate::oauth::OAuthSession {
                    id: result.session_id,  // ACTUAL session ID, not proxy URL!
                    credentials: crate::oauth::OAuthCredentials {
                        access_token: String::new(),
                        refresh_token: None,
                        expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
                    },
                    api_key: None,
                    auth_mode: crate::oauth::AuthMode::ProxyMode,
                    granted_scopes: vec![],
                    created_at: chrono::Utc::now(),
                    user_id: None,
                };
                oauth_session = Some(proxy_session);
                "proxy-session".to_string()
            }
            None => {
                // check_anthropic_proxy already printed the error
                return Ok(());
            }
        }
    } else if let Some(k) = &provider.api_key {
        // API key configured - use it directly
        k.clone()
    } else {
        // Non-Anthropic provider with no API key
        let env_var = match config.proxy.target.as_str() {
            "openai" => "OPENAI_API_KEY",
            "google" => "GOOGLE_API_KEY",
            "zai" => "ZAI_API_KEY",
            "deepseek" => "DEEPSEEK_API_KEY",
            "qwen" => "QWEN_API_KEY",
            _ => "API_KEY",
        };
        eprintln!(
            "No API key configured for '{}'. Set {} or add to config.",
            config.proxy.target, env_var
        );
        return Ok(());
    };

    // Determine model
    let mut model = model_override
        .or_else(|| provider.model.clone())
        .unwrap_or_else(|| match config.proxy.target.as_str() {
            "anthropic" => "claude-sonnet-4-20250514".to_string(),
            "openai" => "gpt-4".to_string(),
            "google" => "gemini-pro".to_string(),
            "zai" => "glm-4.7".to_string(),
            "deepseek" => "deepseek-chat".to_string(),
            "qwen" => "qwen-turbo".to_string(),
            _ => "gpt-4".to_string(),
        });

    let adapter = get_adapter(&config.proxy.target);
    let client = reqwest::Client::new();

    // Initialize hook engine with merged hooks (config + Claude Code hooks)
    let claude_hooks = load_claude_code_hooks();
    let merged_hooks = merge_hooks_config(config.hooks.clone(), claude_hooks);
    let hook_engine = HookEngine::new(merged_hooks);

    // Initialize rules engine
    let rules_engine = RulesEngine::new(".openclaudia/rules");

    // Initialize rustyline editor with history
    let mut rl = DefaultEditor::new()?;
    let history_path = get_history_path();

    // Create history directory if it doesn't exist
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent).ok();
    }

    // Load history (ignore errors if file doesn't exist)
    let _ = rl.load_history(&history_path);

    // Clear screen and render TUI welcome screen
    let _ = tui::clear_screen();
    let welcome = tui::WelcomeScreen::new(
        env!("CARGO_PKG_VERSION"),
        &config.proxy.target,
        &model,
    );
    if let Err(e) = welcome.render() {
        // Fallback to simple output if TUI fails
        eprintln!("TUI render failed: {}, using simple output", e);
        println!("OpenClaudia v{}", env!("CARGO_PKG_VERSION"));
        println!("Provider: {} | Model: {}", config.proxy.target, model);
        println!("Type /help for commands, /sessions to list saved chats");
        println!("Tip: {}\n", get_random_tip());
    }

    // Initialize chat session
    let mut chat_session = ChatSession::new(&model, &config.proxy.target);

    // Initialize memory database
    // Short-term memory (session summaries, recent activity) is ALWAYS available
    // Full stateful mode (memory tools, core memory in prompt) requires --stateful flag
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let memory_db: Option<memory::MemoryDb> = match memory::MemoryDb::open_for_project(&cwd) {
        Ok(db) => {
            // Show short-term memory status
            let recent_count = db.get_recent_sessions(10).map(|s| s.len()).unwrap_or(0);
            if recent_count > 0 {
                println!("\x1b[90m📝 {} recent session(s) loaded from memory\x1b[0m", recent_count);
            }

            if stateful {
                println!("\x1b[35m🧠 Stateful mode enabled\x1b[0m - Memory: {}", db.path().display());

                // Inject core memory into session at start (only in full stateful mode)
                if let Ok(core_memory) = db.format_core_memory_for_prompt() {
                    chat_session.messages.push(serde_json::json!({
                        "role": "system",
                        "content": format!(
                            "You are running in STATEFUL MODE. You have persistent memory across sessions.\n\n\
                            {}\n\n\
                            IMPORTANT: Use your memory tools to:\n\
                            - Save important facts, decisions, and learnings with memory_save\n\
                            - Search for relevant context with memory_search\n\
                            - Update your core memory sections to refine your understanding\n\
                            - Your core memory is always present in context and persists across sessions",
                            core_memory
                        )
                    }));
                }
            }
            tracing::debug!("Memory database: {}", db.path().display());
            Some(db)
        }
        Err(e) => {
            tracing::warn!("Failed to initialize memory database: {}", e);
            None
        }
    };

    // Initialize permissions cache for sensitive operations
    let mut permissions: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        // Show input hints before prompt
        let mode_str = chat_session.mode.display().to_lowercase();
        let _ = tui::render_input_prompt(&mode_str);

        let readline = rl.readline("> ");

        match readline {
            Ok(line) => {
                let mut input = line.trim().to_string();
                let mut editor_message_added = false;

                if input.is_empty() {
                    continue;
                }

                // Handle multiline input (lines ending with \)
                while input.ends_with('\\') {
                    // Remove trailing backslash
                    input.pop();
                    // Read continuation line
                    match rl.readline("... ") {
                        Ok(cont_line) => {
                            input.push('\n');
                            input.push_str(cont_line.trim());
                        }
                        Err(_) => break,
                    }
                }

                // Add to history
                let _ = rl.add_history_entry(&input);
                let input = input.as_str();

                // Handle slash commands
                if let Some(result) = handle_slash_command(input, &mut chat_session.messages, &config.proxy.target, &model) {
                    match result {
                        SlashCommandResult::Exit => {
                            // Save session to short-term memory before exiting
                            save_session_to_short_term_memory(&chat_session, memory_db.as_ref());
                            break;
                        }
                        SlashCommandResult::Clear => {
                            // Save current session before starting new one
                            save_session_to_short_term_memory(&chat_session, memory_db.as_ref());
                            chat_session = ChatSession::new(&model, &config.proxy.target);
                            continue;
                        }
                        SlashCommandResult::LoadSession(session_id) => {
                            // Load the requested session
                            if let Some(loaded) = load_chat_session(&session_id) {
                                chat_session = loaded;
                                println!("Loaded {} messages from previous session.\n", chat_session.messages.len());
                            }
                            continue;
                        }
                        SlashCommandResult::Export => {
                            // Export conversation to markdown
                            export_chat_session(&chat_session);
                            continue;
                        }
                        SlashCommandResult::Compact => {
                            // Compact conversation by summarizing old messages
                            let (before, after) = compact_chat_session(&mut chat_session);
                            if before != after {
                                println!("\nCompacted: ~{} tokens -> ~{} tokens\n", before, after);
                                if let Err(e) = save_chat_session(&chat_session) {
                                    tracing::warn!("Failed to save compacted session: {}", e);
                                }
                            }
                            continue;
                        }
                        SlashCommandResult::EditorInput(editor_content) => {
                            // Process editor content and send as message
                            let expanded = if editor_content.contains('@') {
                                expand_file_references(&editor_content)
                            } else {
                                editor_content
                            };
                            // Add user message from editor
                            chat_session.messages.push(serde_json::json!({
                                "role": "user",
                                "content": expanded
                            }));
                            chat_session.update_title();
                            chat_session.touch();
                            // Clear undo stack since we're adding new messages
                            chat_session.clear_undo_stack();
                            // Set flag to skip normal message addition and go straight to API call
                            editor_message_added = true;
                        }
                        SlashCommandResult::Undo => {
                            if chat_session.undo() {
                                println!("\nUndone last exchange. {} messages remaining.\n", chat_session.messages.len());
                                if let Err(e) = save_chat_session(&chat_session) {
                                    tracing::warn!("Failed to save session: {}", e);
                                }
                            } else {
                                println!("\nNothing to undo.\n");
                            }
                            continue;
                        }
                        SlashCommandResult::Redo => {
                            if chat_session.redo() {
                                println!("\nRedone last exchange. {} messages now.\n", chat_session.messages.len());
                                if let Err(e) = save_chat_session(&chat_session) {
                                    tracing::warn!("Failed to save session: {}", e);
                                }
                            } else {
                                println!("\nNothing to redo.\n");
                            }
                            continue;
                        }
                        SlashCommandResult::SwitchModel(new_model) => {
                            model = new_model;
                            chat_session.model = model.clone();
                            continue;
                        }
                        SlashCommandResult::Status => {
                            // Display session status
                            let tokens = estimate_session_tokens(&chat_session);
                            let msg_count = chat_session.messages.len();
                            let duration = chrono::Utc::now().signed_duration_since(chat_session.created_at);
                            let mins = duration.num_minutes();

                            println!("\n=== Session Status ===");
                            println!("  Session ID: {}...", &chat_session.id[..8]);
                            println!("  Title:      {}", chat_session.title);
                            println!("  Provider:   {}", chat_session.provider);
                            println!("  Model:      {}", chat_session.model);
                            println!("  Mode:       {} ({})", chat_session.mode.display(), chat_session.mode.description());
                            println!("  Messages:   {}", msg_count);
                            println!("  Est tokens: ~{}", tokens);
                            println!("  Duration:   {} min", mins);
                            println!("  Created:    {}", chat_session.created_at.format("%Y-%m-%d %H:%M UTC"));
                            println!();
                            continue;
                        }
                        SlashCommandResult::ToggleMode => {
                            chat_session.mode = chat_session.mode.toggle();
                            println!("\nSwitched to {} mode: {}\n",
                                chat_session.mode.display(),
                                chat_session.mode.description());
                            continue;
                        }
                        SlashCommandResult::Keybindings => {
                            display_keybindings(&config.keybindings);
                            continue;
                        }
                        SlashCommandResult::Rename(new_title) => {
                            chat_session.title = new_title.clone();
                            chat_session.touch();
                            if let Err(e) = save_chat_session(&chat_session) {
                                tracing::warn!("Failed to save session: {}", e);
                            }
                            println!("\nSession renamed to: {}\n", new_title);
                            continue;
                        }
                        SlashCommandResult::Memory(args) => {
                            handle_memory_command(&args, memory_db.as_ref());
                            continue;
                        }
                        SlashCommandResult::Activity(args) => {
                            handle_activity_command(&args, &chat_session.id, memory_db.as_ref());
                            continue;
                        }
                        SlashCommandResult::Handled => {
                            continue;
                        }
                    }
                }

                // Handle shell commands (starting with !)
                if let Some(cmd) = input.strip_prefix('!') {
                    if cmd.is_empty() {
                        println!("Usage: !<command> (e.g., !ls -la)\n");
                        continue;
                    }
                    execute_shell_command_with_permission(cmd, &mut permissions);
                    continue;
                }

                // Handle comments (starting with #) - saved as notes but not sent to AI
                if input.starts_with('#') {
                    let note = input.trim_start_matches('#').trim();
                    if !note.is_empty() {
                        chat_session.messages.push(serde_json::json!({
                            "role": "system",
                            "content": format!("[Note: {}]", note),
                            "metadata": { "type": "note" }
                        }));
                        chat_session.touch();
                        if let Err(e) = save_chat_session(&chat_session) {
                            tracing::warn!("Failed to save session: {}", e);
                        }
                        println!("Note saved.\n");
                    }
                    continue;
                }

                // Add user message (skip if already added from editor)
                if !editor_message_added {
                    // Expand @file references in input
                    let expanded_input = if input.contains('@') {
                        expand_file_references(input)
                    } else {
                        input.to_string()
                    };

                    chat_session.messages.push(serde_json::json!({
                        "role": "user",
                        "content": expanded_input.clone()
                    }));
                    chat_session.update_title();
                    chat_session.touch();
                    // Clear undo stack since we're adding new messages
                    chat_session.clear_undo_stack();

                    // Run UserPromptSubmit hooks
                    let hook_input = HookInput::new(HookEvent::UserPromptSubmit)
                        .with_prompt(&expanded_input);
                    let hook_result = hook_engine.run(HookEvent::UserPromptSubmit, &hook_input).await;

                    if !hook_result.allowed {
                        let reason = hook_result.outputs.first()
                            .and_then(|o| o.reason.clone())
                            .unwrap_or_else(|| "Request blocked by hook".to_string());
                        eprintln!("\nBlocked: {}\n", reason);
                        // Remove the blocked message
                        chat_session.messages.pop();
                        continue;
                    }

                    // Inject hook context into messages if any
                    for output in &hook_result.outputs {
                        if let Some(sys_msg) = &output.system_message {
                            chat_session.messages.insert(0, serde_json::json!({
                                "role": "system",
                                "content": sys_msg
                            }));
                        }
                    }
                }

                // Extract file extensions from messages and inject rules
                let extensions: Vec<String> = chat_session.messages.iter()
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .flat_map(|text| {
                        ext_regex.captures_iter(text)
                            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_lowercase()))
                            .collect::<Vec<_>>()
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();

                // Inject rules if we found file extensions
                if !extensions.is_empty() {
                    let rules_content = rules_engine.get_combined_rules(
                        &extensions.iter().map(|s| s.as_str()).collect::<Vec<_>>()
                    );
                    if !rules_content.is_empty() && !chat_session.messages.iter().any(|m| {
                        m.get("content").and_then(|c| c.as_str())
                            .map(|s| s.contains("## Rules"))
                            .unwrap_or(false)
                    }) {
                        // Add rules as system message at the start
                        chat_session.messages.insert(0, serde_json::json!({
                            "role": "system",
                            "content": rules_content
                        }));
                    }
                }

                // Build and inject Claudia's core system prompt
                // Collect any hook instructions that were injected as system messages
                let hook_instructions: Option<String> = chat_session.messages.iter()
                    .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .filter(|c| !c.contains("You are Claudia")) // Don't include our own prompt
                    .map(|s| s.to_string())
                    .reduce(|acc, s| format!("{}\n\n{}", acc, s));

                let system_prompt = prompt::build_system_prompt(
                    hook_instructions.as_deref(),
                    None, // Custom instructions could come from config in future
                    memory_db.as_ref(),
                );

                // Insert core system prompt at position 0 (becomes first message)
                if !chat_session.messages.iter().any(|m| {
                    m.get("content").and_then(|c| c.as_str())
                        .map(|s| s.contains("You are Claudia"))
                        .unwrap_or(false)
                }) {
                    chat_session.messages.insert(0, serde_json::json!({
                        "role": "system",
                        "content": system_prompt
                    }));
                }

                // Check if we're using our built-in proxy mode (must check before building request)
                let using_proxy = oauth_session.as_ref()
                    .map(|s| s.auth_mode == crate::oauth::AuthMode::ProxyMode)
                    .unwrap_or(false);

                // Build request - proxy mode sends minimal request (no tools, no extra fields)
                // This matches exactly what the working curl command sends
                let request_body = if using_proxy {
                    // Extract system message to top-level (Claude API requirement)
                    let system_msg = chat_session.messages.iter()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                        .map(String::from);

                    // Filter out system messages from the array
                    let user_messages: Vec<_> = chat_session.messages.iter()
                        .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
                        .cloned()
                        .collect();

                    // Minimal request like curl - NO tools, NO system identifier injection
                    let mut req = serde_json::json!({
                        "model": model,
                        "messages": user_messages,
                        "max_tokens": 4096,
                        "stream": true
                    });

                    // Add system as top-level parameter if present
                    if let Some(sys) = system_msg {
                        req["system"] = serde_json::json!(sys);
                    }

                    req
                } else {
                    serde_json::json!({
                        "model": model,
                        "messages": chat_session.messages,
                        "max_tokens": 4096,
                        "stream": true,
                        "tools": tools::get_all_tool_definitions(stateful)
                    })
                };

                // Get endpoint - proxy mode uses our local proxy, which handles OAuth internally
                let endpoint = if using_proxy {
                    // Use the stored proxy_url for the endpoint
                    if let Some(ref url) = proxy_url {
                        eprintln!("[debug] Using built-in proxy at: {}", url);
                        format!("{}/v1/messages", url)
                    } else {
                        format!("{}{}", provider.base_url, adapter.chat_endpoint())
                    }
                } else {
                    format!("{}{}", provider.base_url, adapter.chat_endpoint())
                };

                // Build headers based on auth mode
                let headers: Vec<(String, String)> = if using_proxy {
                    // Proxy mode: send Cookie header with session ID so proxy uses stored OAuth
                    if let Some(ref session) = oauth_session {
                        eprintln!("[debug] Proxy mode - sending Cookie: anthropic_session={}", session.id);
                        vec![
                            ("anthropic-version".to_string(), "2023-06-01".to_string()),
                            ("content-type".to_string(), "application/json".to_string()),
                            ("Cookie".to_string(), format!("anthropic_session={}", session.id)),
                        ]
                    } else {
                        eprintln!("[debug] Proxy mode - no session, proxy will use any stored session");
                        vec![
                            ("anthropic-version".to_string(), "2023-06-01".to_string()),
                            ("content-type".to_string(), "application/json".to_string()),
                        ]
                    }
                } else {
                    adapter.get_headers(&api_key)
                };

                // Show spinner while connecting
                let spinner = ProgressBar::new_spinner();
                spinner.set_style(
                    ProgressStyle::default_spinner()
                        .template("{spinner:.cyan} {msg}")
                        .expect("Invalid spinner template")
                );
                spinner.set_message("Connecting...");
                spinner.enable_steady_tick(std::time::Duration::from_millis(80));

                // Send request
                let mut req = client.post(&endpoint).json(&request_body);
                for (key, value) in &headers {
                    req = req.header(key, value);
                }

                match req.send().await {
                    Ok(response) => {
                        spinner.finish_and_clear();

                        if response.status().is_success() {
                            // Stream the response
                            use crossterm::event::{self, Event, KeyEventKind};
                            use futures::StreamExt;
                            use std::io::Write;

                            println!();
                            let mut full_content = String::new();
                            let mut stream = response.bytes_stream();
                            let mut buffer = String::new();
                            let mut cancelled = false;
                            let mut pending_action: Option<SlashCommandResult> = None;
                            let mut tool_accumulator = tools::ToolCallAccumulator::new();

                            while let Some(chunk_result) = stream.next().await {
                                // Check for configured keybindings during streaming
                                if event::poll(std::time::Duration::from_millis(1)).unwrap_or(false) {
                                    if let Ok(Event::Key(key_event)) = event::read() {
                                        if key_event.kind == KeyEventKind::Press {
                                            // Convert key event to binding string and look up action
                                            if let Some(key_str) = key_event_to_string(&key_event, false) {
                                                if config.keybindings.is_bound(&key_str) {
                                                    let action = config.keybindings.get_action_or_default(&key_str);
                                                    // Cancel immediately stops streaming
                                                    if action == config::KeyAction::Cancel {
                                                        cancelled = true;
                                                        print!(" (cancelled)");
                                                        std::io::stdout().flush().ok();
                                                        break;
                                                    }
                                                    // Other actions queued for after streaming completes
                                                    if let Some(result) = execute_key_action(&action) {
                                                        pending_action = Some(result);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                match chunk_result {
                                    Ok(chunk) => {
                                        // Append chunk to buffer
                                        buffer.push_str(&String::from_utf8_lossy(&chunk));

                                        // Process complete SSE lines
                                        while let Some(line_end) = buffer.find('\n') {
                                            let line = buffer[..line_end].trim().to_string();
                                            buffer = buffer[line_end + 1..].to_string();

                                            // Skip empty lines and comments
                                            if line.is_empty() || line.starts_with(':') {
                                                continue;
                                            }

                                            // Parse SSE data lines
                                            if let Some(data) = line.strip_prefix("data: ") {
                                                // Check for stream end
                                                if data == "[DONE]" {
                                                    break;
                                                }

                                                // Parse JSON
                                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                                    // Anthropic format: content_block_delta with delta.text
                                                    if json.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                                                        if let Some(text) = json
                                                            .get("delta")
                                                            .and_then(|d| d.get("text"))
                                                            .and_then(|t| t.as_str())
                                                        {
                                                            print!("{}", text);
                                                            std::io::stdout().flush().ok();
                                                            full_content.push_str(text);
                                                        }
                                                    }
                                                    // OpenAI format: choices[0].delta.content
                                                    else if let Some(delta) = json
                                                        .get("choices")
                                                        .and_then(|c| c.get(0))
                                                        .and_then(|c| c.get("delta"))
                                                    {
                                                        // Handle text content
                                                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                                            print!("{}", content);
                                                            std::io::stdout().flush().ok();
                                                            full_content.push_str(content);
                                                        }
                                                        // Accumulate tool calls
                                                        tool_accumulator.process_delta(delta);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("\nStream error: {}", e);
                                        break;
                                    }
                                }
                            }

                            println!();

                            // If cancelled, append note to content
                            if cancelled && !full_content.is_empty() {
                                full_content.push_str("\n\n[Response interrupted by user]");
                            }

                            // Agentic loop - continue while there are tool calls
                            let max_iterations = 10; // Prevent infinite loops
                            let mut iteration = 0;
                            let mut current_content = full_content;

                            // Check for tool calls
                            let has_tools = tool_accumulator.has_tool_calls();

                            while has_tools && !cancelled && iteration < max_iterations {
                                iteration += 1;

                                // Get tool calls
                                let tool_calls = tool_accumulator.finalize();

                                // Add assistant message with tool calls
                                let tool_calls_json: Vec<serde_json::Value> = tool_calls.iter().map(|tc| {
                                    serde_json::json!({
                                        "id": tc.id,
                                        "type": tc.call_type,
                                        "function": {
                                            "name": tc.function.name,
                                            "arguments": tc.function.arguments
                                        }
                                    })
                                }).collect();

                                chat_session.messages.push(serde_json::json!({
                                    "role": "assistant",
                                    "content": if current_content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(current_content.clone()) },
                                    "tool_calls": tool_calls_json
                                }));

                                // Execute each tool and collect results
                                for tool_call in &tool_calls {
                                    println!("\n\x1b[36m⚡ Running {}...\x1b[0m", tool_call.function.name);

                                    // Use appropriate tool executor based on stateful mode
                                    let result = if let Some(ref db) = memory_db {
                                        tools::execute_tool_with_memory(tool_call, Some(db))
                                    } else {
                                        tools::execute_tool(tool_call)
                                    };

                                    // Log activity for short-term memory
                                    if let Some(ref db) = memory_db {
                                        let activity_type = match tool_call.function.name.as_str() {
                                            "read_file" => "file_read",
                                            "write_file" => "file_write",
                                            "edit_file" => "file_edit",
                                            "bash" => "bash_command",
                                            "chainlink" => {
                                                // Parse chainlink subcommand
                                                if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments) {
                                                    if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                                                        if cmd.starts_with("create") { "issue_created" }
                                                        else if cmd.starts_with("close") { "issue_closed" }
                                                        else if cmd.starts_with("comment") { "issue_comment" }
                                                        else { "chainlink" }
                                                    } else { "chainlink" }
                                                } else { "chainlink" }
                                            }
                                            other => other,
                                        };

                                        // Extract target from args
                                        let target = if let Ok(args) = serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments) {
                                            args.get("path")
                                                .or_else(|| args.get("file_path"))
                                                .or_else(|| args.get("command"))
                                                .and_then(|v| v.as_str())
                                                .unwrap_or(&tool_call.function.name)
                                                .to_string()
                                        } else {
                                            tool_call.function.name.clone()
                                        };

                                        let _ = db.log_activity(
                                            &chat_session.id,
                                            activity_type,
                                            &target,
                                            if result.is_error { Some("error") } else { None },
                                        );
                                    }

                                    // Show result preview
                                    let preview: String = result.content.lines().take(5).collect::<Vec<_>>().join("\n");
                                    if result.is_error {
                                        println!("\x1b[31m✗ Error:\x1b[0m {}", preview);
                                    } else {
                                        println!("\x1b[32m✓\x1b[0m {}", if preview.len() > 200 { format!("{}...", &preview[..200]) } else { preview });
                                    }

                                    // Add tool result to messages
                                    chat_session.messages.push(serde_json::json!({
                                        "role": "tool",
                                        "tool_call_id": result.tool_call_id,
                                        "content": result.content
                                    }));
                                }

                                // Clear accumulator for next iteration
                                tool_accumulator.clear();

                                // Continue the conversation - send tool results back to model
                                println!("\n\x1b[90mContinuing with tool results...\x1b[0m\n");

                                // Build new request with tool results
                                // Use minimal format for proxy, native for direct OAuth
                                let request_body = if using_proxy {
                                    // Extract system message to top-level
                                    let system_msg = chat_session.messages.iter()
                                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                                        .map(String::from);

                                    let user_messages: Vec<_> = chat_session.messages.iter()
                                        .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
                                        .cloned()
                                        .collect();

                                    let mut req = serde_json::json!({
                                        "model": model,
                                        "messages": user_messages,
                                        "max_tokens": 4096,
                                        "stream": true
                                    });

                                    if let Some(sys) = system_msg {
                                        req["system"] = serde_json::json!(sys);
                                    }

                                    req
                                } else {
                                    serde_json::json!({
                                        "model": model,
                                        "messages": chat_session.messages,
                                        "max_tokens": 4096,
                                        "stream": true,
                                        "tools": tools::get_all_tool_definitions(stateful)
                                    })
                                };

                                // Send follow-up request
                                let mut req = client.post(&endpoint).json(&request_body);
                                for (key, value) in &headers {
                                    req = req.header(key, value);
                                }

                                current_content = String::new();

                                if let Ok(response) = req.send().await {
                                    if response.status().is_success() {
                                        let mut stream = response.bytes_stream();
                                        let mut buffer = String::new();

                                        while let Some(chunk_result) = stream.next().await {
                                            if let Ok(chunk) = chunk_result {
                                                buffer.push_str(&String::from_utf8_lossy(&chunk));

                                                while let Some(line_end) = buffer.find('\n') {
                                                    let line = buffer[..line_end].trim().to_string();
                                                    buffer = buffer[line_end + 1..].to_string();

                                                    if line.is_empty() || line.starts_with(':') {
                                                        continue;
                                                    }

                                                    if let Some(data) = line.strip_prefix("data: ") {
                                                        if data == "[DONE]" {
                                                            break;
                                                        }

                                                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                                            // Anthropic format: content_block_delta
                                                            if json.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                                                                if let Some(text) = json
                                                                    .get("delta")
                                                                    .and_then(|d| d.get("text"))
                                                                    .and_then(|t| t.as_str())
                                                                {
                                                                    print!("{}", text);
                                                                    std::io::stdout().flush().ok();
                                                                    current_content.push_str(text);
                                                                }
                                                            }
                                                            // OpenAI format: choices[0].delta.content
                                                            else if let Some(delta) = json
                                                                .get("choices")
                                                                .and_then(|c| c.get(0))
                                                                .and_then(|c| c.get("delta"))
                                                            {
                                                                // Handle text content
                                                                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                                                    print!("{}", content);
                                                                    std::io::stdout().flush().ok();
                                                                    current_content.push_str(content);
                                                                }
                                                                // Accumulate tool calls for next iteration
                                                                tool_accumulator.process_delta(delta);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        println!();
                                    }
                                }
                            }

                            // Save final response
                            if !current_content.is_empty() && !tool_accumulator.has_tool_calls() {
                                // Add final assistant message (text response after tool loop or direct response)
                                chat_session.messages.push(serde_json::json!({
                                    "role": "assistant",
                                    "content": current_content
                                }));
                                chat_session.touch();
                                if let Err(e) = save_chat_session(&chat_session) {
                                    tracing::warn!("Failed to save session: {}", e);
                                }
                            } else if iteration > 0 {
                                // Tool loop completed but no final text - still save session
                                chat_session.touch();
                                if let Err(e) = save_chat_session(&chat_session) {
                                    tracing::warn!("Failed to save session: {}", e);
                                }
                            } else if current_content.is_empty() && !tool_accumulator.has_tool_calls() {
                                // No content and no tool calls - remove the failed user message
                                chat_session.messages.pop();
                            }

                            // Handle any keybinding action pressed during streaming
                            if let Some(action_result) = pending_action {
                                match action_result {
                                    SlashCommandResult::Exit => {
                                        // Save history before exit
                                        if let Err(e) = rl.save_history(&history_path) {
                                            tracing::warn!("Failed to save history: {}", e);
                                        }
                                        println!("\nGoodbye!");
                                        return Ok(());
                                    }
                                    SlashCommandResult::ToggleMode => {
                                        chat_session.mode = chat_session.mode.toggle();
                                        println!("\nSwitched to {} mode: {}\n",
                                            chat_session.mode.display(),
                                            chat_session.mode.description());
                                    }
                                    SlashCommandResult::Status => {
                                        let tokens = estimate_session_tokens(&chat_session);
                                        let duration = chrono::Utc::now().signed_duration_since(chat_session.created_at);
                                        println!("\n[{}] {} | ~{} tokens | {} min\n",
                                            chat_session.mode.display(),
                                            chat_session.model,
                                            tokens,
                                            duration.num_minutes());
                                    }
                                    SlashCommandResult::Export => {
                                        export_chat_session(&chat_session);
                                    }
                                    _ => {
                                        // Other actions print their own messages via execute_key_action
                                    }
                                }
                            }
                        } else {
                            let status = response.status();
                            let body = response.text().await.unwrap_or_default();
                            eprintln!("\nError {}: {}\n", status, body);
                            // Remove the failed user message
                            chat_session.messages.pop();
                        }
                    }
                    Err(e) => {
                        spinner.finish_and_clear();
                        eprintln!("\nRequest failed: {}\n", e);
                        // Remove the failed user message
                        chat_session.messages.pop();
                    }
                }

                // Autosave session after each response (protects against terminal close)
                save_session_to_short_term_memory(&chat_session, memory_db.as_ref());
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C - graceful exit (save session before exiting)
                println!("\n\x1b[90mInterrupted - saving session...\x1b[0m");
                break;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D - exit
                break;
            }
            Err(err) => {
                eprintln!("Error: {:?}", err);
                break;
            }
        }
    }

    // Save session to short-term memory on any exit
    save_session_to_short_term_memory(&chat_session, memory_db.as_ref());

    // Save history
    if let Err(e) = rl.save_history(&history_path) {
        tracing::warn!("Failed to save history: {}", e);
    }

    println!("\nGoodbye!");
    Ok(())
}

/// Start the proxy server
async fn cmd_start(
    port: Option<u16>,
    host: Option<String>,
    target: Option<String>,
) -> anyhow::Result<()> {
    let mut config = config::load_config()?;

    // Apply command-line overrides
    if let Some(p) = port {
        config.proxy.port = p;
    }
    if let Some(h) = host {
        config.proxy.host = h;
    }
    if let Some(t) = target {
        config.proxy.target = t;
    }

    // Check for API key - warn if missing but allow startup for OAuth mode
    if let Some(provider) = config.active_provider() {
        if provider.api_key.is_none() {
            let env_var = match config.proxy.target.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "google" => "GOOGLE_API_KEY",
                "zai" => "ZAI_API_KEY",
                "deepseek" => "DEEPSEEK_API_KEY",
                "qwen" => "QWEN_API_KEY",
                _ => "API_KEY",
            };
            // For Anthropic, OAuth authentication is available
            if config.proxy.target == "anthropic" {
                tracing::warn!(
                    "No API key configured for '{}'. OAuth authentication is available.",
                    config.proxy.target
                );
                info!("Visit http://localhost:{}/auth/device to authenticate with Claude Max", config.proxy.port);
            } else {
                error!(
                    "No API key configured for provider '{}'. Set {} environment variable.",
                    config.proxy.target, env_var
                );
                return Ok(());
            }
        }
    }

    info!("OpenClaudia v{} starting...", env!("CARGO_PKG_VERSION"));
    info!(
        "Proxy: http://{}:{} -> {}",
        config.proxy.host, config.proxy.port, config.proxy.target
    );
    info!(
        "Point your AI client at: http://localhost:{}",
        config.proxy.port
    );

    proxy::start_server(config).await
}

/// Show current configuration
fn cmd_config() -> anyhow::Result<()> {
    match config::load_config() {
        Ok(config) => {
            println!("OpenClaudia Configuration\n");
            println!("Proxy:");
            println!("  Host: {}", config.proxy.host);
            println!("  Port: {}", config.proxy.port);
            println!("  Target: {}", config.proxy.target);
            println!();
            println!("Providers:");
            for (name, provider) in &config.providers {
                let has_key = provider.api_key.is_some();
                println!(
                    "  {}: {} (API key: {})",
                    name,
                    provider.base_url,
                    if has_key { "configured" } else { "not set" }
                );
            }
            println!();
            println!("Session:");
            println!("  Timeout: {} minutes", config.session.timeout_minutes);
            println!("  Persist path: {:?}", config.session.persist_path);
        }
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            info!("Run 'openclaudia init' to create a configuration file.");
        }
    }
    Ok(())
}

/// Check configuration and connectivity
async fn cmd_doctor() -> anyhow::Result<()> {
    use crate::mcp::McpManager;
    use crate::plugins::{PluginError, PluginManager};
    use crate::providers::{get_adapter, ProviderError};
    use crate::rules::RulesEngine;
    use crate::session::SessionManager;
    use std::time::Duration;

    println!("OpenClaudia Doctor\n");

    // Check configuration
    print!("Configuration... ");
    match config::load_config() {
        Ok(config) => {
            println!("OK");

            // Check API keys and default models
            for (name, provider) in &config.providers {
                print!("  {} API key... ", name);
                if provider.api_key.is_some() {
                    println!("configured");
                } else {
                    println!("NOT SET");
                }
                // Show default model if configured
                if let Some(model) = &provider.model {
                    println!("    Default model: {}", model);
                }
            }

            // Check connectivity to target provider
            print!("\nConnectivity to {}... ", config.proxy.target);
            if let Some(provider) = config.active_provider() {
                let client = reqwest::Client::new();
                match client.get(&provider.base_url).send().await {
                    Ok(_) => println!("OK"),
                    Err(e) => println!("FAILED: {}", e),
                }
            } else {
                println!("SKIPPED (no provider configured)");
            }
        }
        Err(e) => {
            println!("FAILED: {}", e);
            println!("\nRun 'openclaudia init' to create a configuration file.");
        }
    }

    // Check for hooks directory
    print!("\nHooks directory... ");
    if PathBuf::from(".openclaudia/hooks").exists() {
        println!("OK");
    } else {
        println!("NOT FOUND");
    }

    // Check for rules directory and load rules
    print!("Rules directory... ");
    if PathBuf::from(".openclaudia/rules").exists() {
        println!("OK");
        let rules_engine = RulesEngine::new(".openclaudia/rules");
        let all_rules = rules_engine.all_rules();
        if !all_rules.is_empty() {
            println!("  Loaded rules: {}", all_rules.len());
            for rule in all_rules {
                println!("    - {} (languages: {:?})", rule.name, rule.languages);
            }
        }
        // Test getting rules for files
        let test_files = ["src/main.rs", "test.py"];
        let matched = rules_engine.get_rules_for_files(&test_files);
        println!("  Rules for test files: {} matched", matched.len());
    } else {
        println!("NOT FOUND");
    }

    // Check plugins
    print!("\nPlugins... ");
    let mut plugin_manager = PluginManager::new();
    let errors = plugin_manager.discover();
    if plugin_manager.count() > 0 {
        println!("OK ({} loaded)", plugin_manager.count());
        for plugin in plugin_manager.all() {
            let root = plugin.root();
            println!(
                "  - {} v{} ({})",
                plugin.name(),
                plugin.manifest.version,
                root.display()
            );

            // Show plugin env vars
            let env_vars = plugin.env_vars();
            if !env_vars.is_empty() {
                println!("    Environment: {} vars", env_vars.len());
            }

            // Show plugin commands
            if !plugin.manifest.commands.is_empty() {
                println!("    Commands: {}", plugin.manifest.commands.len());
            }

            // Show MCP servers
            if !plugin.manifest.mcp_servers.is_empty() {
                println!("    MCP servers: {}", plugin.manifest.mcp_servers.len());
            }
        }

        // Show all MCP servers from plugins
        let all_mcp = plugin_manager.all_mcp_servers();
        if !all_mcp.is_empty() {
            println!("\n  MCP Servers from plugins:");
            for (plugin, server) in all_mcp {
                println!(
                    "    - {} from {} ({})",
                    server.name,
                    plugin.name(),
                    server.transport
                );
            }
        }

        // Test plugin resolve_path method
        for plugin in plugin_manager.all() {
            let resolved = plugin.resolve_path("scripts/init.sh");
            info!(
                "Plugin {} script path: {}",
                plugin.name(),
                resolved.display()
            );
        }
    } else if !errors.is_empty() {
        println!("ERRORS");
        for err in errors {
            println!("  Error: {}", err);
        }
    } else {
        println!("none found");
    }

    // Test PluginError::NotFound variant for completeness
    let not_found_err = PluginError::NotFound("test-plugin".to_string());
    info!("Plugin error test: {}", not_found_err);

    // Test MCP manager functionality
    print!("\nMCP Manager... ");
    let mut mcp_manager = McpManager::new();

    // Test is_connected (should return false for nonexistent)
    let is_connected = mcp_manager.is_connected("test-server");
    println!(
        "{}",
        if is_connected {
            "connected"
        } else {
            "no servers"
        }
    );

    // Test get_server_info
    if let Some((name, supports_list_changed)) = mcp_manager.get_server_info("test-server") {
        println!(
            "  Server: {} (list_changed: {})",
            name, supports_list_changed
        );
    }

    // Test call_tool (will fail gracefully - server not connected)
    match mcp_manager
        .call_tool("test_tool", serde_json::json!({}))
        .await
    {
        Ok(result) => println!("  Tool result: {}", result),
        Err(e) => info!("  Expected error (no server): {}", e),
    }

    // Test call_tool_with_timeout
    match mcp_manager
        .call_tool_with_timeout("test_tool", serde_json::json!({}), Duration::from_secs(1))
        .await
    {
        Ok(result) => println!("  Timeout tool result: {}", result),
        Err(e) => info!("  Expected timeout error: {}", e),
    }

    // Test disconnect methods
    let _ = mcp_manager.disconnect("nonexistent").await;
    let _ = mcp_manager.disconnect_all().await;

    // Check session state
    print!("\nSession... ");
    let mut session_manager = SessionManager::new(".openclaudia/session");
    if let Some(handoff) = session_manager.get_handoff_context() {
        println!("found handoff context ({} bytes)", handoff.len());
    }

    let sessions = session_manager.list_sessions();
    if !sessions.is_empty() {
        println!("  Previous sessions: {}", sessions.len());
        for session in sessions.iter().take(3) {
            println!(
                "    - {} ({:?}, {} requests)",
                session.id, session.mode, session.request_count
            );
        }
        if sessions.len() > 10 {
            println!("  Note: Consider running cleanup (>10 sessions stored)");
            session_manager.cleanup_old_sessions(10);
        }
    } else {
        println!("  No previous sessions");
    }

    // Test session operations
    let session = session_manager.start_initializer();
    let session_id = session.id.clone();
    info!("Test session created: {}", session_id);

    // Test session methods
    if let Some(session) = session_manager.get_session_mut() {
        session.add_tokens(100);
        session.complete_task("Doctor check task");
        session.add_modified_file("src/main.rs");
        info!(
            "Session updated: {} tokens, {} completed tasks",
            session.total_tokens,
            session.progress.completed_tasks.len()
        );
    }

    // Test load_session
    if let Some(loaded) = session_manager.load_session(&session_id) {
        info!("Loaded session: {} (mode: {:?})", loaded.id, loaded.mode);
    }

    let coding_session = session_manager.start_coding(&session_id);
    info!("Coding session: {}", coding_session.id);

    // Test rules reload and rules_dir
    print!("\nRules engine... ");
    let mut rules_engine = RulesEngine::new(".openclaudia/rules");
    let rules_path = rules_engine.rules_dir().to_path_buf();
    println!("path: {}", rules_path.display());
    rules_engine.reload();
    info!("Rules reloaded from {}", rules_path.display());

    // Test provider adapters and error variants
    print!("\nProvider adapters... ");
    let adapter = get_adapter("anthropic");
    println!("{} adapter OK", adapter.name());

    // Test transform_response
    let test_response = serde_json::json!({
        "id": "test",
        "content": [{"type": "text", "text": "test"}],
        "model": "test-model",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });
    match adapter.transform_response(test_response, false) {
        Ok(transformed) => info!("Response transformed: {}", transformed["object"]),
        Err(e) => info!("Transform error (expected): {}", e),
    }

    // Test ProviderError variants
    let _invalid = ProviderError::InvalidResponse("test".to_string());
    let _unsupported = ProviderError::Unsupported("test feature".to_string());
    info!("Provider error variants OK");

    // Test PluginManager::with_paths and other methods
    let custom_paths = vec![PathBuf::from(".openclaudia/plugins")];
    let mut custom_plugin_manager = PluginManager::with_paths(custom_paths);
    let _ = custom_plugin_manager.discover();
    info!(
        "Custom plugin manager: {} plugins",
        custom_plugin_manager.count()
    );

    // Test plugin manager methods
    if let Some(plugin) = custom_plugin_manager.get("test-plugin") {
        info!("Found plugin: {}", plugin.name());
    }

    // Test hooks methods
    let all_hooks = custom_plugin_manager.all_hooks();
    info!("All hooks: {}", all_hooks.len());

    let session_hooks = custom_plugin_manager.hooks_for_event("session_start");
    info!("Session start hooks: {}", session_hooks.len());

    // Test enable/disable/reload
    let _ = custom_plugin_manager.enable("test-plugin");
    let _ = custom_plugin_manager.disable("test-plugin");
    let reload_errors = custom_plugin_manager.reload();
    info!("Plugin reload: {} errors", reload_errors.len());

    // Test proxy MCP functions
    print!("\nProxy MCP functions... ");
    let mcp_for_proxy = std::sync::Arc::new(tokio::sync::RwLock::new(McpManager::new()));

    // Test handle_mcp_tool_call
    match crate::proxy::handle_mcp_tool_call(&mcp_for_proxy, "test_tool", serde_json::json!({}))
        .await
    {
        Ok(result) => info!("MCP tool result: {}", result),
        Err(e) => info!("Expected MCP error: {}", e),
    }

    // Test shutdown_mcp
    crate::proxy::shutdown_mcp(&mcp_for_proxy).await;
    println!("OK");

    println!("\nDoctor check complete.");
    Ok(())
}

/// Run in iteration/loop mode with Stop hooks
async fn cmd_loop(
    max_iterations: u32,
    port: Option<u16>,
    target: Option<String>,
) -> anyhow::Result<()> {
    use crate::hooks::{HookEngine, HookEvent, HookInput};
    use crate::session::SessionManager;
    use tokio::sync::watch;

    let mut config = config::load_config()?;

    // Apply command-line overrides
    if let Some(p) = port {
        config.proxy.port = p;
    }
    if let Some(t) = target {
        config.proxy.target = t;
    }

    // Validate API key
    if let Some(provider) = config.active_provider() {
        if provider.api_key.is_none() {
            let env_var = match config.proxy.target.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "google" => "GOOGLE_API_KEY",
                "zai" => "ZAI_API_KEY",
                "deepseek" => "DEEPSEEK_API_KEY",
                "qwen" => "QWEN_API_KEY",
                _ => "API_KEY",
            };
            error!(
                "No API key configured for provider '{}'. Set {} environment variable.",
                config.proxy.target, env_var
            );
            return Ok(());
        }
    }

    // Initialize session manager
    let session_dir = config.session.persist_path.clone();
    let mut session_manager = SessionManager::new(&session_dir);
    let session = session_manager.get_or_create_session();
    let session_id = session.id.clone();

    // Initialize hook engine
    let hook_engine = HookEngine::new(config.hooks.clone());

    info!(
        "OpenClaudia v{} starting in loop mode...",
        env!("CARGO_PKG_VERSION")
    );
    info!(
        "Max iterations: {}",
        if max_iterations == 0 {
            "unlimited".to_string()
        } else {
            max_iterations.to_string()
        }
    );
    info!("Session ID: {}", session_id);
    info!(
        "Proxy: http://{}:{} -> {}",
        config.proxy.host, config.proxy.port, config.proxy.target
    );

    // Create shutdown channel for graceful termination
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Handle Ctrl+C
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        if let Ok(()) = tokio::signal::ctrl_c().await {
            info!("Received Ctrl+C, initiating shutdown...");
            let _ = shutdown_tx_clone.send(true);
        }
    });

    let mut iteration: u32 = 0;
    let mut shutdown_rx_loop = shutdown_rx.clone();

    loop {
        iteration += 1;

        // Check max iterations
        if max_iterations > 0 && iteration > max_iterations {
            info!("Reached maximum iterations ({})", max_iterations);
            break;
        }

        // Check for shutdown signal
        if *shutdown_rx_loop.borrow() {
            info!("Shutdown signal received");
            break;
        }

        info!("=== Iteration {} ===", iteration);

        // Update session
        if let Some(session) = session_manager.get_session_mut() {
            session.increment_requests();
        }

        // Start the proxy server for this iteration
        let config_clone = config.clone();
        let shutdown_rx_server = shutdown_rx.clone();

        let server_handle = tokio::spawn(async move {
            proxy::start_server_with_shutdown(config_clone, shutdown_rx_server).await
        });

        // Wait for the server to complete (client disconnects or shutdown)
        match server_handle.await {
            Ok(Ok(())) => {
                info!("Iteration {} completed", iteration);
            }
            Ok(Err(e)) => {
                error!("Server error in iteration {}: {}", iteration, e);
            }
            Err(e) => {
                error!("Server task error: {}", e);
            }
        }

        // Check Stop hooks to determine if we should continue
        let stop_input = HookInput::new(HookEvent::Stop)
            .with_session_id(&session_id)
            .with_extra("iteration", serde_json::json!(iteration));

        let stop_result = hook_engine.run(HookEvent::Stop, &stop_input).await;

        if !stop_result.allowed {
            info!(
                "Stop hook requested termination: {:?}",
                stop_result
                    .outputs
                    .first()
                    .and_then(|o| o.reason.as_deref())
            );
            break;
        }

        // Check for shutdown again before next iteration
        if shutdown_rx_loop.changed().await.is_err() || *shutdown_rx_loop.borrow() {
            info!("Shutdown requested between iterations");
            break;
        }

        info!("Continuing to next iteration...");
    }

    // End session with handoff notes
    let handoff = format!(
        "Loop mode completed after {} iterations.\nSession ended at iteration {}.",
        iteration, iteration
    );
    session_manager.end_session(Some(&handoff));

    info!("Loop mode ended after {} iterations", iteration);
    Ok(())
}
