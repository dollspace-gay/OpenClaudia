use super::input::open_external_editor;
use super::models::get_available_models;
use super::review::{configure_provider_api_key, review_git_changes};
use super::{get_data_dir, get_history_path, get_sessions_dir, list_chat_sessions};
use crate::cli::commands::init::init_project_rules;
use crate::cli::display::theme::handle_theme_command;
use openclaudia::memory;
use openclaudia::plugins;
use openclaudia::skills;
use openclaudia::tools::file_index::FileIndex;
use openclaudia::tools::safe_truncate;
use std::fs;

/// Plugin slash command actions
pub enum PluginAction {
    /// Show main plugin menu / list installed plugins
    Menu,
    /// Show plugin help
    Help,
    /// Install a plugin (optionally from marketplace)
    Install {
        plugin: Option<String>,
        marketplace: Option<String>,
    },
    /// Manage installed plugins
    Manage,
    /// Uninstall a plugin
    Uninstall { plugin: String },
    /// Enable a plugin
    Enable { plugin: String },
    /// Disable a plugin
    Disable { plugin: String },
    /// Validate a plugin manifest
    Validate { path: Option<String> },
    /// Marketplace subcommand
    Marketplace {
        action: Option<String>,
        target: Option<String>,
    },
    /// Reload all plugins
    Reload,
    /// Execute a specific plugin command (/plugin-name:command)
    RunCommand {
        plugin_name: String,
        command_name: String,
    },
}

/// Slash command result
pub enum SlashCommandResult {
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
    /// Plugin management command
    Plugin(PluginAction),
    /// Theme was changed to the given name
    ThemeChanged(String),
    /// Toggle vim mode (visual indicator in prompt)
    ToggleVim,
    /// Invoke a skill (inject its prompt as the next user message)
    Skill(String),
    /// Set effort level for the session (low/medium/high)
    SetEffort(String),
    /// Cycle effort level: low → medium → high → low
    CycleEffort,
    /// Show help message (already printed)
    Handled,
}

/// Handle slash commands, returns Some if command was handled
pub fn handle_slash_command(
    input: &str,
    messages: &mut Vec<serde_json::Value>,
    provider: &str,
    current_model: &str,
) -> Option<SlashCommandResult> {
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
            println!("  /model           - Show current model and provider");
            println!("  /model list      - List available models for current provider");
            println!("  /model <name>    - Switch to a different model");
            println!("  /copy            - Copy last assistant response to clipboard");
            println!("  /init            - Initialize project config with auto-detection");
            println!("  /review          - Review uncommitted git changes");
            println!("  /commit          - Stage changes and commit with auto-generated message");
            println!("  /commit-push-pr  - Commit, push, and create a pull request");
            println!("  /review <branch> - Compare current branch against <branch>");
            println!("  /status          - Show session status (model, tokens, etc.)");
            println!("  /connect         - Configure API keys for providers");
            println!("  /theme           - List available color themes");
            println!("  /theme <name>    - Switch to a color theme");
            println!("  /mode            - Toggle between Build and Plan modes");
            println!("  /vim             - Toggle vim mode (show mode indicator in prompt)");
            println!("  /effort [level]  - Set effort level (low/medium/high)");
            println!("  /keybindings     - Show configured keyboard shortcuts");
            println!("  /rename <title>  - Rename the current session");
            println!("  /version         - Show version and system information");
            println!("  /debug           - Show debug info (paths, env vars, config)");
            println!("  /find <query>    - Fuzzy-find files in the project");
            println!("  /doctor          - Run inline diagnostics");
            println!("  /config          - Show current configuration");
            println!("  /config path     - Show config file locations");
            println!("  /cost            - Show session cost estimate");
            println!("  /context         - Show context window usage breakdown");
            println!("  /login           - Check authentication status");
            println!("  /logout          - Show how to clear credentials");
            println!();
            println!("Memory Commands (auto-learning):");
            println!("  /memory          - Show auto-learning stats");
            println!("  /memory patterns - Show learned coding patterns");
            println!("  /memory errors   - Show known error patterns");
            println!("  /memory prefs    - Show learned preferences");
            println!("  /memory files    - Show file co-edit relationships");
            println!("  /memory reset    - Reset all learned data (with confirmation)");
            println!();
            println!("Activity Commands:");
            println!("  /activity        - Show current session activities");
            println!("  /activity sessions - Show recent session summaries");
            println!("  /activity files  - Show files modified this session");
            println!("  /activity issues - Show issues worked this session");
            println!();
            println!("Plugin Commands:");
            println!("  /plugin          - List installed plugins");
            println!("  /plugin install  - Install a plugin");
            println!("  /plugin manage   - Manage installed plugins");
            println!("  /plugin help     - Show all plugin commands");
            println!("  /<plugin>:<cmd>  - Run a plugin command");
            println!();
            println!("Model Control:");
            println!("  /model           - Show current model");
            println!("  /model list      - List available models for current provider");
            println!("  /effort <level>  - Set effort level (low/medium/high)");
            println!();
            println!("Skill Commands:");
            println!("  /skill           - List available skills");
            println!("  /skill <name>    - Invoke a skill (inject prompt as next message)");
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
                println!("\nSaved Sessions ({}):\n", sessions.len());
                for (i, session) in sessions.iter().take(10).enumerate() {
                    let date = session.updated_at.format("%Y-%m-%d %H:%M");
                    let msg_count = session.messages.len();
                    let id_prefix = &session.id[..8.min(session.id.len())];
                    println!(
                        "  {}. \x1b[36m{}\x1b[0m  \x1b[90m{} · {} · {} msgs\x1b[0m",
                        i + 1,
                        session.title,
                        date,
                        session.model,
                        msg_count,
                    );
                    println!("     \x1b[90mid: {id_prefix}\x1b[0m");
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
                let sessions = list_chat_sessions();
                if let Some(session) = sessions.first() {
                    println!("\nContinuing: {}\n", session.title);
                    return Some(SlashCommandResult::LoadSession(session.id.clone()));
                }
                println!("\nNo sessions to continue.\n");
            } else if let Ok(num) = args.parse::<usize>() {
                let sessions = list_chat_sessions();
                if num > 0 && num <= sessions.len() {
                    let session = &sessions[num - 1];
                    println!("\nContinuing: {}\n", session.title);
                    return Some(SlashCommandResult::LoadSession(session.id.clone()));
                }
                println!("\nInvalid session number. Use /sessions to see available sessions.\n");
            } else {
                println!("\nUsage: /continue <number>\n");
            }
            Some(SlashCommandResult::Handled)
        }
        "exit" | "quit" | "q" => Some(SlashCommandResult::Exit),
        "history" => {
            if messages.is_empty() {
                println!("\nNo messages in conversation.\n");
            } else {
                println!("\nConversation History ({} messages):", messages.len());
                for (i, msg) in messages.iter().enumerate() {
                    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let preview = if content.len() > 60 {
                        format!("{}...", safe_truncate(content, 57))
                    } else {
                        content.to_string()
                    };
                    println!("  {}. [{}] {}", i + 1, role, preview);
                }
                println!();
            }
            Some(SlashCommandResult::Handled)
        }
        "model" | "models" => {
            if args.is_empty() && cmd == "model" {
                // Show current model
                println!("\nCurrent model: \x1b[36m{current_model}\x1b[0m");
                println!("Provider: {provider}");
                println!("Use /model list to see available models, /model <name> to switch.\n");
                Some(SlashCommandResult::Handled)
            } else if args.is_empty() && cmd == "models" || args == "list" {
                // List available models (static list)
                let models = get_available_models(provider);
                println!("\nAvailable models for \x1b[36m{provider}\x1b[0m:\n");
                for m in &models {
                    let marker = if *m == current_model {
                        " \x1b[32m← current\x1b[0m"
                    } else {
                        ""
                    };
                    println!("  \x1b[36m{m}\x1b[0m{marker}");
                }

                // Try dynamic model listing for OpenAI-compatible providers
                if let Ok(config) = openclaudia::config::load_config() {
                    if let Some(provider_config) = config.get_provider(provider) {
                        let adapter = openclaudia::providers::get_adapter(provider);
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            if let Some(dynamic) =
                                handle.block_on(super::models::fetch_dynamic_models(
                                    provider_config,
                                    adapter.as_ref(),
                                ))
                            {
                                println!("\n  Dynamic models (from API):");
                                for m in &dynamic {
                                    let marker = if m == current_model {
                                        " \x1b[32m← current\x1b[0m"
                                    } else {
                                        ""
                                    };
                                    println!("    \x1b[36m{m}\x1b[0m{marker}");
                                }
                            }
                        }
                    }
                }

                println!("\nUse /model <name> to switch.\n");
                Some(SlashCommandResult::Handled)
            } else {
                // Switch model
                let new_model = args.trim().to_string();
                let available = get_available_models(provider);
                if available.contains(&new_model.as_str()) || !available.is_empty() {
                    println!("\nSwitching to model: \x1b[36m{new_model}\x1b[0m\n");
                    Some(SlashCommandResult::SwitchModel(new_model))
                } else {
                    Some(SlashCommandResult::Handled)
                }
            }
        }
        "export" => Some(SlashCommandResult::Export),
        "compact" | "summarize" => Some(SlashCommandResult::Compact),
        "editor" | "edit" | "e" => {
            if let Some(content) = open_external_editor() {
                Some(SlashCommandResult::EditorInput(content))
            } else {
                Some(SlashCommandResult::Handled)
            }
        }
        "undo" => Some(SlashCommandResult::Undo),
        "redo" => Some(SlashCommandResult::Redo),
        "copy" | "yank" | "y" => {
            if let Some(last_assistant) = messages
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
            {
                if let Some(content) = last_assistant.get("content").and_then(|c| c.as_str()) {
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => match clipboard.set_text(content) {
                            Ok(()) => println!("\nCopied {} chars to clipboard.\n", content.len()),
                            Err(e) => eprintln!("\nFailed to copy to clipboard: {e}\n"),
                        },
                        Err(e) => eprintln!("\nClipboard not available: {e}\n"),
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
            use std::path::Path;
            let config_exists = Path::new(".openclaudia/config.yaml").exists();
            if config_exists {
                println!("\n\u{26a0} Configuration already exists at .openclaudia/config.yaml");
                println!("Use /config to view, or delete the file to reinitialize.\n");
            } else {
                // Create directories
                let _ = std::fs::create_dir_all(".openclaudia/skills");

                // Detect project type
                let mut project_types = Vec::new();
                if Path::new("Cargo.toml").exists() {
                    project_types.push("Rust");
                }
                if Path::new("package.json").exists() {
                    project_types.push("Node.js");
                }
                if Path::new("pyproject.toml").exists() || Path::new("setup.py").exists() {
                    project_types.push("Python");
                }
                if Path::new("go.mod").exists() {
                    project_types.push("Go");
                }
                if Path::new("pom.xml").exists() {
                    project_types.push("Java");
                }
                if Path::new("Gemfile").exists() {
                    project_types.push("Ruby");
                }

                if !project_types.is_empty() {
                    println!("\nDetected: {}", project_types.join(", "));
                }

                // Create default config
                let default_config = "\
# OpenClaudia Configuration
proxy:
  port: 8080
  host: \"127.0.0.1\"
  target: anthropic

providers:
  anthropic:
    base_url: https://api.anthropic.com

session:
  timeout_minutes: 30
  persist_path: .openclaudia/session
";

                let _ = std::fs::create_dir_all(".openclaudia");
                match std::fs::write(".openclaudia/config.yaml", default_config) {
                    Ok(()) => {
                        println!("\n\u{2713} Created .openclaudia/config.yaml");
                        println!("\u{2713} Created .openclaudia/skills/");
                        println!("\nEdit .openclaudia/config.yaml to configure providers and API keys.\n");
                    }
                    Err(e) => println!("\n\u{2717} Failed to create config: {e}\n"),
                }
            }
            // Also run existing project rules initialization
            init_project_rules();
            Some(SlashCommandResult::Handled)
        }
        "review" => {
            review_git_changes(args);
            Some(SlashCommandResult::Handled)
        }
        "status" | "info" => Some(SlashCommandResult::Status),
        "connect" | "auth" => {
            configure_provider_api_key();
            Some(SlashCommandResult::Handled)
        }
        "theme" | "themes" => {
            if let Some(new_name) = handle_theme_command(args) {
                Some(SlashCommandResult::ThemeChanged(new_name))
            } else {
                Some(SlashCommandResult::Handled)
            }
        }
        "mode" => Some(SlashCommandResult::ToggleMode),
        "vim" => Some(SlashCommandResult::ToggleVim),
        "effort" => {
            let level = args.trim().to_lowercase();
            match level.as_str() {
                "low" | "l" => {
                    println!(
                        "\n\u{2713} Effort set to \x1b[33mlow\x1b[0m (faster, less thorough)\n"
                    );
                    Some(SlashCommandResult::SetEffort("low".to_string()))
                }
                "medium" | "med" | "m" => {
                    println!("\n\u{2713} Effort set to \x1b[36mmedium\x1b[0m (balanced)\n");
                    Some(SlashCommandResult::SetEffort("medium".to_string()))
                }
                "high" | "h" => {
                    println!("\n\u{2713} Effort set to \x1b[32mhigh\x1b[0m (thorough, slower)\n");
                    Some(SlashCommandResult::SetEffort("high".to_string()))
                }
                "" => {
                    // No args: cycle low → medium → high → low
                    Some(SlashCommandResult::CycleEffort)
                }
                _ => {
                    println!("\nUsage: /effort [low|medium|high]");
                    println!("  low    - Quick answers, minimal thinking");
                    println!("  medium - Balanced (default)");
                    println!("  high   - Thorough, more thinking time");
                    println!("  (no argument cycles through levels)\n");
                    Some(SlashCommandResult::Handled)
                }
            }
        }
        "keybindings" | "keys" | "bindings" => Some(SlashCommandResult::Keybindings),
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
            println!(
                "Platform:   {} / {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            println!();
            Some(SlashCommandResult::Handled)
        }
        "doctor" => {
            println!("\nRunning diagnostics...\n");

            // Check git
            print!("  Git... ");
            match std::process::Command::new("git")
                .args(["--version"])
                .output()
            {
                Ok(o) if o.status.success() => {
                    println!("\u{2713} {}", String::from_utf8_lossy(&o.stdout).trim());
                }
                _ => println!("\u{2717} not found"),
            }

            // Check Claude Code credentials
            print!("  Claude Code credentials... ");
            if openclaudia::claude_credentials::has_claude_code_credentials() {
                println!("\u{2713} found");
            } else {
                println!("\u{2717} not found (~/.claude/.credentials.json)");
            }

            // Check config
            print!("  Config... ");
            match openclaudia::config::load_config() {
                Ok(_) => println!("\u{2713} loaded"),
                Err(e) => println!("\u{2717} {e}"),
            }

            // Check MCP servers
            print!("  MCP config... ");
            let mcp_path = std::path::PathBuf::from(".mcp.json");
            if mcp_path.exists() {
                match std::fs::read_to_string(&mcp_path) {
                    Ok(content) => {
                        let count = serde_json::from_str::<serde_json::Value>(&content)
                            .ok()
                            .and_then(|v| {
                                v.get("mcpServers")
                                    .and_then(|s| s.as_object())
                                    .map(serde_json::Map::len)
                            })
                            .unwrap_or(0);
                        println!("\u{2713} {count} server(s)");
                    }
                    Err(e) => println!("\u{2717} {e}"),
                }
            } else {
                println!("\u{00b7} not configured");
            }

            // Check skills
            print!("  Skills... ");
            let loaded_skills = skills::load_skills();
            if loaded_skills.is_empty() {
                println!("\u{00b7} none loaded");
            } else {
                println!("\u{2713} {} skill(s)", loaded_skills.len());
            }

            // Check gh CLI
            print!("  GitHub CLI (gh)... ");
            match std::process::Command::new("gh")
                .args(["--version"])
                .output()
            {
                Ok(o) if o.status.success() => println!(
                    "\u{2713} {}",
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .next()
                        .unwrap_or("installed")
                ),
                _ => println!("\u{00b7} not found (optional, for /commit-push-pr)"),
            }

            println!();
            Some(SlashCommandResult::Handled)
        }
        "config" => {
            let config_parts: Vec<&str> = args.splitn(3, ' ').collect();
            match config_parts.first().copied().unwrap_or("show") {
                "" | "show" => match openclaudia::config::load_config() {
                    Ok(cfg) => {
                        println!("\nConfiguration:\n");
                        println!("  Provider: {}", cfg.proxy.target);
                        println!("  Host: {}:{}", cfg.proxy.host, cfg.proxy.port);
                        for (name, p) in &cfg.providers {
                            let has_key = p.api_key.is_some();
                            println!(
                                "  {} \u{2192} {} (key: {})",
                                name,
                                p.base_url,
                                if has_key { "\u{2713}" } else { "\u{2717}" }
                            );
                        }
                        println!(
                            "  VDD: {} ({})",
                            if cfg.vdd.enabled { "on" } else { "off" },
                            cfg.vdd.mode
                        );
                        println!("  Session timeout: {} min", cfg.session.timeout_minutes);
                        println!();
                    }
                    Err(e) => println!("\nFailed to load config: {e}\n"),
                },
                "path" => {
                    println!("\nConfig locations:");
                    println!("  Project: .openclaudia/config.yaml");
                    if let Some(home) = dirs::home_dir() {
                        println!(
                            "  User: {}",
                            home.join(".openclaudia/config.yaml").display()
                        );
                    }
                    println!("  Credentials: ~/.claude/.credentials.json");
                    println!("  MCP: .mcp.json");
                    println!("  Skills: .openclaudia/skills/");
                    println!();
                }
                _ => println!("\nUsage: /config [show|path]\n"),
            }
            Some(SlashCommandResult::Handled)
        }
        "debug" => {
            println!("\n=== Debug Information ===\n");
            println!("Provider:     {provider}");
            println!("Model:        {current_model}");
            println!("Messages:     {}", messages.len());
            println!();
            println!("Configuration Paths:");
            println!("  Project:    .openclaudia/config.yaml");
            if let Some(home) = dirs::home_dir() {
                println!(
                    "  User:       {}",
                    home.join(".openclaudia/config.yaml").display()
                );
            }
            if let Some(config_dir) = dirs::config_dir() {
                println!(
                    "  System:     {}",
                    config_dir.join("openclaudia/config.yaml").display()
                );
            }
            println!();
            println!("Data Directories:");
            println!("  Sessions:   {}", get_sessions_dir().display());
            println!("  History:    {}", get_history_path().display());
            println!("  Data:       {}", get_data_dir().display());
            println!();
            println!("Environment Variables:");
            for var in &[
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "GOOGLE_API_KEY",
                "DEEPSEEK_API_KEY",
                "QWEN_API_KEY",
                "ZAI_API_KEY",
                "EDITOR",
            ] {
                let status = if std::env::var(var).is_ok() {
                    "set"
                } else {
                    "not set"
                };
                println!("  {var}: {status}");
            }
            println!();
            Some(SlashCommandResult::Handled)
        }
        "find" | "f" => {
            if args.is_empty() {
                println!("\nUsage: /find <query>\n");
            } else {
                let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
                let index = FileIndex::build(&root);
                let results = index.search(args, 20);
                if results.is_empty() {
                    println!(
                        "\nNo files matching '{}' ({} files indexed)\n",
                        args,
                        index.len()
                    );
                } else {
                    println!(
                        "\nTop {} matches for '{}' ({} files indexed):\n",
                        results.len(),
                        args,
                        index.len()
                    );
                    for (i, r) in results.iter().enumerate() {
                        println!(
                            "  {:>2}. \x1b[36m{}\x1b[0m \x1b[90m(score: {})\x1b[0m",
                            i + 1,
                            r.path,
                            r.score
                        );
                    }
                    println!();
                }
            }
            Some(SlashCommandResult::Handled)
        }
        "memory" | "mem" => Some(SlashCommandResult::Memory(args.to_string())),
        "activity" | "act" => Some(SlashCommandResult::Activity(args.to_string())),
        "plugin" | "plugins" => {
            let sub_parts: Vec<&str> = args.splitn(2, ' ').collect();
            let subcmd = sub_parts.first().copied().unwrap_or("").to_lowercase();
            let sub_args = sub_parts.get(1).copied().unwrap_or("").trim();

            let action = match subcmd.as_str() {
                "" => PluginAction::Menu,
                "help" | "?" => PluginAction::Help,
                "install" | "i" => {
                    if sub_args.is_empty() {
                        PluginAction::Install {
                            plugin: None,
                            marketplace: None,
                        }
                    } else if sub_args.contains('@') {
                        let parts: Vec<&str> = sub_args.splitn(2, '@').collect();
                        PluginAction::Install {
                            plugin: Some(parts[0].to_string()),
                            marketplace: Some(parts[1].to_string()),
                        }
                    } else {
                        PluginAction::Install {
                            plugin: Some(sub_args.to_string()),
                            marketplace: None,
                        }
                    }
                }
                "manage" => PluginAction::Manage,
                "uninstall" | "remove" | "rm" => {
                    if sub_args.is_empty() {
                        println!("\nUsage: /plugin uninstall <plugin-name>\n");
                        return Some(SlashCommandResult::Handled);
                    }
                    PluginAction::Uninstall {
                        plugin: sub_args.to_string(),
                    }
                }
                "enable" => {
                    if sub_args.is_empty() {
                        println!("\nUsage: /plugin enable <plugin-name>\n");
                        return Some(SlashCommandResult::Handled);
                    }
                    PluginAction::Enable {
                        plugin: sub_args.to_string(),
                    }
                }
                "disable" => {
                    if sub_args.is_empty() {
                        println!("\nUsage: /plugin disable <plugin-name>\n");
                        return Some(SlashCommandResult::Handled);
                    }
                    PluginAction::Disable {
                        plugin: sub_args.to_string(),
                    }
                }
                "validate" => PluginAction::Validate {
                    path: if sub_args.is_empty() {
                        None
                    } else {
                        Some(sub_args.to_string())
                    },
                },
                "marketplace" | "market" => {
                    let market_parts: Vec<&str> = sub_args.splitn(2, ' ').collect();
                    let market_cmd = market_parts.first().copied().unwrap_or("");
                    let market_target = market_parts.get(1).copied().unwrap_or("").trim();
                    PluginAction::Marketplace {
                        action: if market_cmd.is_empty() {
                            None
                        } else {
                            Some(market_cmd.to_string())
                        },
                        target: if market_target.is_empty() {
                            None
                        } else {
                            Some(market_target.to_string())
                        },
                    }
                }
                "reload" => PluginAction::Reload,
                _ => {
                    println!("\nUnknown plugin subcommand: {subcmd}. Use /plugin help.\n");
                    return Some(SlashCommandResult::Handled);
                }
            };
            Some(SlashCommandResult::Plugin(action))
        }
        "skill" | "skills" => {
            if args.is_empty() {
                // List available skills
                let all_skills = skills::load_skills();
                if all_skills.is_empty() {
                    println!("\nNo skills found.");
                    println!("Add skill files to .openclaudia/skills/ or ~/.openclaudia/skills/");
                    println!("\nSkill file format (YAML frontmatter + markdown body):");
                    println!("  ---");
                    println!("  name: my-skill");
                    println!("  description: Does something useful");
                    println!("  ---");
                    println!("  ");
                    println!("  You are a specialized agent that...\n");
                } else {
                    println!("\n=== Available Skills ({}) ===\n", all_skills.len());
                    for skill in &all_skills {
                        println!("  \x1b[36m{}\x1b[0m - {}", skill.name, skill.description);
                        println!("    \x1b[90m{}\x1b[0m", skill.path.display());
                    }
                    println!("\nUse /skill <name> to invoke a skill.\n");
                }
                Some(SlashCommandResult::Handled)
            } else {
                let skill_name = args.trim();
                if let Some(skill) = skills::get_skill(skill_name) {
                    println!("\n\x1b[36mInvoking skill: {}\x1b[0m\n", skill.name);
                    Some(SlashCommandResult::Skill(skill.prompt))
                } else {
                    eprintln!(
                        "\nSkill '{skill_name}' not found. Use /skill to list available skills.\n"
                    );
                    Some(SlashCommandResult::Handled)
                }
            }
        }
        "commit" => {
            use std::process::Command;
            // Check if in a git repo
            if !Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                println!("\nNot inside a git repository.\n");
                return Some(SlashCommandResult::Handled);
            }

            let staged = Command::new("git")
                .args(["diff", "--cached", "--stat"])
                .output();
            let unstaged = Command::new("git").args(["diff", "--stat"]).output();
            let has_staged = staged
                .as_ref()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            let has_unstaged = unstaged
                .as_ref()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);

            if !has_staged && !has_unstaged {
                println!("\nNo changes to commit.\n");
                return Some(SlashCommandResult::Handled);
            }

            if !has_staged {
                println!("\nUnstaged changes:");
                if let Ok(ref o) = unstaged {
                    println!("{}", String::from_utf8_lossy(&o.stdout));
                }
                print!("Stage all changes? [y/n] ");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                if input.trim().to_lowercase().starts_with('y') {
                    let _ = Command::new("git").args(["add", "-A"]).output();
                    println!("All changes staged.");
                } else {
                    println!("Commit cancelled.");
                    return Some(SlashCommandResult::Handled);
                }
            }

            let files = Command::new("git")
                .args(["diff", "--cached", "--name-only"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            let file_list: Vec<&str> = files.trim().lines().collect();
            let msg = if file_list.len() == 1 {
                format!("Update {}", file_list[0])
            } else {
                format!("Update {} files", file_list.len())
            };

            println!("\nFiles: {}", files.trim());
            print!("\nCommit message: \x1b[36m{msg}\x1b[0m\n[y/e(dit)/n] ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            match input.trim().to_lowercase().as_str() {
                "y" | "yes" | "" => {
                    match Command::new("git").args(["commit", "-m", &msg]).output() {
                        Ok(o) if o.status.success() => {
                            println!("\n✓ {}", String::from_utf8_lossy(&o.stdout).trim());
                        }
                        Ok(o) => println!("\n✗ {}", String::from_utf8_lossy(&o.stderr).trim()),
                        Err(e) => println!("\n✗ {e}"),
                    }
                }
                "e" | "edit" => {
                    print!("Enter commit message: ");
                    std::io::stdout().flush().ok();
                    let mut custom = String::new();
                    std::io::stdin().read_line(&mut custom).ok();
                    if !custom.trim().is_empty() {
                        match Command::new("git")
                            .args(["commit", "-m", custom.trim()])
                            .output()
                        {
                            Ok(o) if o.status.success() => {
                                println!("\n✓ {}", String::from_utf8_lossy(&o.stdout).trim());
                            }
                            Ok(o) => println!("\n✗ {}", String::from_utf8_lossy(&o.stderr).trim()),
                            Err(e) => println!("\n✗ {e}"),
                        }
                    }
                }
                _ => println!("Commit cancelled."),
            }
            Some(SlashCommandResult::Handled)
        }
        "commit-push-pr" => {
            use std::process::Command;
            if !Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                println!("\nNot inside a git repository.\n");
                return Some(SlashCommandResult::Handled);
            }

            // Commit first (reuse commit logic inline)
            let staged = Command::new("git")
                .args(["diff", "--cached", "--stat"])
                .output();
            let unstaged = Command::new("git").args(["diff", "--stat"]).output();
            let has_staged = staged
                .as_ref()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            let has_unstaged = unstaged
                .as_ref()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);

            if has_staged || has_unstaged {
                if !has_staged {
                    let _ = Command::new("git").args(["add", "-A"]).output();
                }
                let files = Command::new("git")
                    .args(["diff", "--cached", "--name-only"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                let file_list: Vec<&str> = files.trim().lines().collect();
                let msg = if file_list.len() == 1 {
                    format!("Update {}", file_list[0])
                } else {
                    format!("Update {} files", file_list.len())
                };
                match Command::new("git").args(["commit", "-m", &msg]).output() {
                    Ok(o) if o.status.success() => println!("✓ Committed: {msg}"),
                    Ok(o) => {
                        println!(
                            "✗ Commit failed: {}",
                            String::from_utf8_lossy(&o.stderr).trim()
                        );
                        return Some(SlashCommandResult::Handled);
                    }
                    Err(e) => {
                        println!("✗ {e}");
                        return Some(SlashCommandResult::Handled);
                    }
                }
            }

            // Push
            let branch = Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if branch == "main" || branch == "master" {
                println!("\n⚠ You're on '{branch}'. Push anyway? [y/n] ");
                use std::io::Write;
                std::io::stdout().flush().ok();
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).ok();
                if !input.trim().to_lowercase().starts_with('y') {
                    println!("Push cancelled.");
                    return Some(SlashCommandResult::Handled);
                }
            }
            match Command::new("git")
                .args(["push", "-u", "origin", &branch])
                .output()
            {
                Ok(o) if o.status.success() => println!("✓ Pushed to origin/{branch}"),
                Ok(o) => {
                    println!(
                        "✗ Push failed: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                    return Some(SlashCommandResult::Handled);
                }
                Err(e) => {
                    println!("✗ {e}");
                    return Some(SlashCommandResult::Handled);
                }
            }

            // Create PR if gh is available
            if Command::new("which")
                .arg("gh")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                let last_msg = Command::new("git")
                    .args(["log", "-1", "--format=%s"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or(branch);
                match Command::new("gh")
                    .args(["pr", "create", "--title", &last_msg, "--body", ""])
                    .output()
                {
                    Ok(o) if o.status.success() => println!(
                        "✓ PR created: {}",
                        String::from_utf8_lossy(&o.stdout).trim()
                    ),
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr);
                        if err.contains("already exists") {
                            println!("PR already exists for this branch.");
                        } else {
                            println!("✗ PR creation failed: {}", err.trim());
                        }
                    }
                    Err(e) => println!("✗ {e}"),
                }
            } else {
                println!("(gh CLI not found — install it to auto-create PRs)");
            }
            Some(SlashCommandResult::Handled)
        }
        "cost" => {
            let msg_text: String = messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            let tokens = openclaudia::compaction::estimate_tokens(&msg_text);
            // Rough pricing: Sonnet ~$3/M input, $15/M output; assume 50/50 split
            let est_cost = tokens as f64 * 0.000_009;
            println!("\nSession cost estimate:");
            println!("  Tokens used: ~{tokens}");
            println!("  Estimated cost: ${est_cost:.4}");
            println!("  (Approximate — actual cost depends on model and input/output ratio)\n");
            Some(SlashCommandResult::Handled)
        }
        "context" => {
            let msg_count = messages.len();
            let ctx_text: String = messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            let tokens = openclaudia::compaction::estimate_tokens(&ctx_text);
            let user_msgs = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .count();
            let asst_msgs = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
                .count();
            let tool_msgs = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("tool"))
                .count();
            let sys_msgs = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                .count();
            let max_tokens = openclaudia::compaction::get_context_window(current_model);
            let pct = if max_tokens > 0 {
                tokens as f32 / max_tokens as f32 * 100.0
            } else {
                0.0
            };

            println!("\nContext window:");
            println!("  Messages: {msg_count} total (user: {user_msgs}, assistant: {asst_msgs}, tool: {tool_msgs}, system: {sys_msgs})");
            println!("  Tokens: ~{tokens} / {max_tokens} ({pct:.1}%)");
            if pct >= 85.0 {
                println!("  \x1b[33m⚠ Nearing limit — use /compact\x1b[0m");
            }
            println!();
            Some(SlashCommandResult::Handled)
        }
        "login" => {
            if openclaudia::claude_credentials::has_claude_code_credentials() {
                println!("\n✓ Authenticated via Claude Code credentials.");
                println!("  File: ~/.claude/.credentials.json");
                if let Ok(creds) = tokio::runtime::Handle::current()
                    .block_on(openclaudia::claude_credentials::load_credentials())
                {
                    println!(
                        "  Type: {}",
                        creds.subscription_type.as_deref().unwrap_or("unknown")
                    );
                    println!(
                        "  Tier: {}",
                        creds.rate_limit_tier.as_deref().unwrap_or("default")
                    );
                }
            } else {
                println!("\n✗ Not authenticated via Claude Code.");
                println!("  To log in:");
                println!("  1. Install Claude Code: npm install -g @anthropic-ai/claude-code");
                println!("  2. Run: claude");
                println!("  3. Complete the login flow");
                println!("  4. Restart OpenClaudia");
            }
            println!();
            Some(SlashCommandResult::Handled)
        }
        "logout" => {
            println!("\nTo clear Claude Code credentials:");
            println!("  rm ~/.claude/.credentials.json");
            println!("\nTo use an API key instead:");
            println!("  export ANTHROPIC_API_KEY=sk-...");
            println!();
            Some(SlashCommandResult::Handled)
        }
        _ => {
            if cmd.contains(':') {
                let colon_parts: Vec<&str> = cmd.splitn(2, ':').collect();
                if colon_parts.len() == 2 {
                    return Some(SlashCommandResult::Plugin(PluginAction::RunCommand {
                        plugin_name: colon_parts[0].to_string(),
                        command_name: colon_parts[1].to_string(),
                    }));
                }
            }
            eprintln!("Unknown command: /{cmd}. Type /help for available commands.\n");
            Some(SlashCommandResult::Handled)
        }
    }
}

/// Handle /memory command for viewing auto-learned knowledge
pub fn handle_memory_command(args: &str, memory_db: Option<&memory::MemoryDb>) {
    let db = if let Some(db) = memory_db {
        db
    } else {
        println!("\n\x1b[33mMemory database not available.\x1b[0m\n");
        return;
    };

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let subargs = parts.get(1).copied().unwrap_or("");

    match subcmd.as_str() {
        "" | "stats" => match db.auto_learn_stats() {
            Ok(stats) => {
                println!("\n=== Auto-Learning Statistics ===");
                println!("  Coding patterns:      {}", stats.coding_patterns);
                println!("  File relationships:   {}", stats.file_relationships);
                println!("  Error patterns:       {}", stats.error_patterns);
                println!("  Errors resolved:      {}", stats.errors_resolved);
                println!("  Learned preferences:  {}", stats.learned_preferences);
                println!("  Database path:        {}", db.path().display());
                println!();
            }
            Err(e) => eprintln!("\nFailed to get auto-learn stats: {e}\n"),
        },
        "patterns" => {
            match db.get_patterns_for_file(if subargs.is_empty() { "*" } else { subargs }) {
                Ok(patterns) => {
                    if patterns.is_empty() {
                        println!("\nNo coding patterns learned yet.\n");
                    } else {
                        println!("\n=== Coding Patterns ({}) ===\n", patterns.len());
                        for p in patterns.iter().take(20) {
                            println!(
                                "  \x1b[36m[{}]\x1b[0m {} \x1b[90m({}x, {})\x1b[0m",
                                p.pattern_type, p.description, p.confidence, p.file_glob
                            );
                        }
                        println!();
                    }
                }
                Err(e) => eprintln!("\nFailed to get patterns: {e}\n"),
            }
        }
        "errors" => {
            if subargs.is_empty() {
                println!("\nUsage: /memory errors <file_path>");
                println!("Example: /memory errors src/main.rs\n");
            } else {
                match db.get_error_patterns_for_file(subargs) {
                    Ok(errors) => {
                        if errors.is_empty() {
                            println!("\nNo error patterns for '{subargs}'.\n");
                        } else {
                            println!(
                                "\n=== Error Patterns for '{}' ({}) ===\n",
                                subargs,
                                errors.len()
                            );
                            for e in &errors {
                                print!(
                                    "  \x1b[31m{}\x1b[0m ({}x)",
                                    e.error_signature, e.occurrences
                                );
                                if let Some(ref res) = e.resolution {
                                    print!(" \x1b[32m-> {res}\x1b[0m");
                                }
                                println!();
                            }
                            println!();
                        }
                    }
                    Err(e) => eprintln!("\nFailed to get error patterns: {e}\n"),
                }
            }
        }
        "prefs" | "preferences" => match db.get_all_preferences() {
            Ok(prefs) => {
                if prefs.is_empty() {
                    println!("\nNo preferences learned yet.\n");
                } else {
                    println!("\n=== Learned Preferences ({}) ===\n", prefs.len());
                    for p in &prefs {
                        println!(
                            "  \x1b[35m[{}]\x1b[0m {} \x1b[90m(confidence: {})\x1b[0m",
                            p.category, p.preference, p.confidence
                        );
                    }
                    println!();
                }
            }
            Err(e) => eprintln!("\nFailed to get preferences: {e}\n"),
        },
        "files" | "relationships" => {
            if subargs.is_empty() {
                println!("\nUsage: /memory files <file_path>");
                println!("Example: /memory files src/main.rs\n");
            } else {
                match db.get_related_files(subargs) {
                    Ok(related) => {
                        if related.is_empty() {
                            println!("\nNo file relationships for '{subargs}'.\n");
                        } else {
                            println!("\n=== Files Co-Edited with '{subargs}' ===\n");
                            for (file, count) in &related {
                                println!("  {file} ({count}x)");
                            }
                            println!();
                        }
                    }
                    Err(e) => eprintln!("\nFailed to get file relationships: {e}\n"),
                }
            }
        }
        "reset" => {
            if subargs == "confirm" || subargs == "yes" {
                match db.reset_all() {
                    Ok(()) => {
                        println!("\n\x1b[32mAll learned data reset.\x1b[0m\n");
                    }
                    Err(e) => eprintln!("\nFailed to reset memory: {e}\n"),
                }
            } else {
                println!("\n\x1b[31mWarning: This will delete ALL learned data!\x1b[0m");
                println!("This includes coding patterns, error patterns, preferences, and file relationships.");
                println!("\nTo confirm, run: /memory reset confirm\n");
            }
        }
        _ => {
            println!("\nUnknown memory subcommand: {subcmd}");
            println!("Available: patterns, errors, prefs, files, reset\n");
        }
    }
}

/// Handle /activity command for viewing recent session activities
pub fn handle_activity_command(
    args: &str,
    current_session_id: &str,
    memory_db: Option<&memory::MemoryDb>,
) {
    let db = if let Some(db) = memory_db {
        db
    } else {
        println!(
            "\n\x1b[33mActivity tracking not available (memory database failed to open).\x1b[0m\n"
        );
        return;
    };

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    let subcmd = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let subargs = parts.get(1).copied().unwrap_or("");

    match subcmd.as_str() {
        "" | "current" => match db.get_session_activities(current_session_id) {
            Ok(activities) => {
                if activities.is_empty() {
                    println!("\nNo activities recorded in this session yet.\n");
                } else {
                    println!(
                        "\n=== Current Session Activities ({}) ===",
                        activities.len()
                    );
                    println!("Session: {current_session_id}\n");
                    for activity in activities.iter().take(20) {
                        let icon = match activity.activity_type.as_str() {
                            "file_read" => "R",
                            "file_write" => "W",
                            "file_edit" => "E",
                            "bash_command" => "$",
                            "issue_created" => "+",
                            "issue_closed" => "x",
                            "issue_comment" => "#",
                            _ => ".",
                        };
                        let details = activity.details.as_deref().unwrap_or("");
                        let details_str = if details.is_empty() {
                            String::new()
                        } else {
                            format!(" ({details})")
                        };
                        println!(
                            "  \x1b[90m[{}]\x1b[0m {} \x1b[36m{}\x1b[0m {}{}",
                            activity.created_at,
                            icon,
                            activity.activity_type,
                            activity.target,
                            details_str
                        );
                        println!(
                            "       \x1b[90mID: {} | Session: {}\x1b[0m",
                            activity.id,
                            safe_truncate(&activity.session_id, 8)
                        );
                    }
                    if activities.len() > 20 {
                        println!("\n  ... and {} more activities", activities.len() - 20);
                    }
                    println!();
                }
            }
            Err(e) => eprintln!("\nFailed to get activities: {e}\n"),
        },
        "sessions" | "recent" => {
            let limit = subargs.parse().unwrap_or(5);
            match db.get_recent_sessions(limit) {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        println!("\nNo recent sessions recorded.\n");
                    } else {
                        println!("\n=== Recent Sessions ({}) ===\n", sessions.len());
                        for (i, session) in sessions.iter().enumerate() {
                            println!(
                                "  \x1b[36m{}.\x1b[0m [ID:{}] Session {} (ended {})",
                                i + 1,
                                session.id,
                                safe_truncate(&session.session_id, 8),
                                session.ended_at
                            );
                            println!("     Started: {}", session.started_at);

                            let summary_preview = if session.summary.len() > 100 {
                                format!("{}...", safe_truncate(&session.summary, 97))
                            } else {
                                session.summary.clone()
                            };
                            println!("     Summary: {summary_preview}");

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
                Err(e) => eprintln!("\nFailed to get recent sessions: {e}\n"),
            }
        }
        "files" => match db.get_session_files_modified(current_session_id) {
            Ok(files) => {
                if files.is_empty() {
                    println!("\nNo files modified in this session yet.\n");
                } else {
                    println!("\n=== Files Modified This Session ({}) ===\n", files.len());
                    for file in &files {
                        println!("  {file}");
                    }
                    println!();
                }
            }
            Err(e) => eprintln!("\nFailed to get modified files: {e}\n"),
        },
        "issues" => match db.get_session_issues(current_session_id) {
            Ok(issues) => {
                if issues.is_empty() {
                    println!("\nNo issues worked on in this session yet.\n");
                } else {
                    println!("\n=== Issues Worked This Session ({}) ===\n", issues.len());
                    for issue in &issues {
                        println!("  {issue}");
                    }
                    println!();
                }
            }
            Err(e) => eprintln!("\nFailed to get issues: {e}\n"),
        },
        "help" => {
            println!("\nActivity Commands:");
            println!("  /activity          - Show current session activities");
            println!("  /activity sessions - Show recent session summaries");
            println!("  /activity files    - Show files modified this session");
            println!("  /activity issues   - Show issues worked this session");
            println!();
        }
        _ => {
            println!("\nUnknown activity subcommand: {subcmd}");
            println!("Available: current, sessions, files, issues, help\n");
        }
    }
}

/// Handle /plugin slash command actions
pub fn handle_plugin_action(action: PluginAction, plugin_manager: &mut plugins::PluginManager) {
    match action {
        PluginAction::Menu => {
            let all: Vec<_> = plugin_manager.all().collect();
            if all.is_empty() {
                println!("\nNo plugins installed.");
                println!("Use /plugin install to browse and install plugins.");
                println!("Use /plugin help for all commands.\n");
            } else {
                println!("\n=== Installed Plugins ({}) ===\n", all.len());
                for plugin in &all {
                    let status = if plugin.enabled {
                        "\x1b[32menabled\x1b[0m"
                    } else {
                        "\x1b[31mdisabled\x1b[0m"
                    };
                    let version = plugin.manifest.version.as_deref().unwrap_or("0.0.0");
                    println!("  {} v{} [{}]", plugin.name(), version, status);
                    if let Some(desc) = &plugin.manifest.description {
                        println!("    {desc}");
                    }
                    let cmd_count = plugin.command_paths.len() + plugin.command_metadata.len();
                    let hook_count = plugin.hook_definitions.len();
                    let mcp_count = plugin.mcp_configs.len();
                    let mut components = Vec::new();
                    if cmd_count > 0 {
                        components.push(format!("{cmd_count} command(s)"));
                    }
                    if hook_count > 0 {
                        components.push(format!("{hook_count} hook def(s)"));
                    }
                    if mcp_count > 0 {
                        components.push(format!("{mcp_count} MCP server(s)"));
                    }
                    if !components.is_empty() {
                        println!("    Components: {}", components.join(", "));
                    }
                    let commands = plugin.resolved_commands();
                    for cmd in &commands {
                        let desc = cmd.description.as_deref().unwrap_or("No description");
                        println!("    /{}:{} - {}", plugin.name(), cmd.name, desc);
                    }
                }
                println!("\nUse /plugin help for management commands.\n");
            }
        }
        PluginAction::Help => {
            println!("\nPlugin Commands:");
            println!();
            println!("  Installation:");
            println!("    /plugin install              - Browse and install plugins");
            println!("    /plugin install <plugin>      - Install specific plugin");
            println!("    /plugin install <p>@<market>  - Install from marketplace");
            println!();
            println!("  Management:");
            println!("    /plugin                      - List installed plugins");
            println!("    /plugin manage               - Manage installed plugins");
            println!("    /plugin enable <plugin>      - Enable a plugin");
            println!("    /plugin disable <plugin>     - Disable a plugin");
            println!("    /plugin uninstall <plugin>   - Uninstall a plugin");
            println!("    /plugin reload               - Reload all plugins");
            println!();
            println!("  Marketplaces:");
            println!("    /plugin marketplace          - Marketplace management");
            println!("    /plugin marketplace add <p>  - Add a marketplace");
            println!("    /plugin marketplace remove <n> - Remove a marketplace");
            println!("    /plugin marketplace update   - Update marketplaces");
            println!("    /plugin marketplace list     - List all marketplaces");
            println!();
            println!("  Validation:");
            println!("    /plugin validate <path>      - Validate a manifest");
            println!();
            println!("  Plugin Commands:");
            println!("    /<plugin-name>:<command>     - Run a plugin command");
            println!();
        }
        PluginAction::Install {
            plugin,
            marketplace,
        } => match (&plugin, &marketplace) {
            (Some(p), Some(m)) => {
                println!("\nInstalling plugin '{p}' from marketplace '{m}'...");
                match plugin_manager.install_from_marketplace(p, m) {
                    Ok(id) => println!("Installed '{id}'. Restart to apply changes.\n"),
                    Err(e) => eprintln!("Failed to install: {e}\n"),
                }
            }
            (Some(p), None) => {
                println!("\nInstalling plugin '{p}'...");
                let path = std::path::Path::new(p.as_str());
                if path.exists() && path.is_dir() {
                    match plugins::Plugin::load(path) {
                        Ok(loaded) => {
                            let name = loaded.name().to_string();
                            let plugins_dir = std::path::PathBuf::from(".openclaudia/plugins");
                            let dest = plugins_dir.join(&name);
                            if let Err(e) = plugins::copy_dir_recursive(path, &dest) {
                                eprintln!("Failed to install plugin: {e}\n");
                                return;
                            }
                            let mut installed = plugins::InstalledPlugins::load();
                            installed.upsert(
                                &name,
                                plugins::PluginInstallEntry {
                                    scope: plugins::InstallScope::Project,
                                    project_path: Some(
                                        std::env::current_dir()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .to_string(),
                                    ),
                                    install_path: dest.to_string_lossy().to_string(),
                                    version: loaded.manifest.version,
                                    installed_at: Some(chrono::Utc::now().to_rfc3339()),
                                    last_updated: None,
                                    git_commit_sha: None,
                                },
                            );
                            if let Err(e) = installed.save() {
                                tracing::warn!("Failed to save install tracking: {}", e);
                            }
                            let _ = plugin_manager.reload();
                            println!("Installed plugin '{name}'. Restart to apply changes.\n");
                        }
                        Err(e) => {
                            eprintln!("Failed to load plugin from path: {e}\n");
                        }
                    }
                } else if p.contains('/')
                    || std::path::Path::new(p)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("git"))
                    || p.starts_with("http")
                {
                    println!("\nCloning plugin from '{p}'...");
                    match plugin_manager.install_from_git(p, None) {
                        Ok(name) => {
                            println!("Installed plugin '{name}'. Restart to apply changes.\n");
                        }
                        Err(e) => eprintln!("Failed to install: {e}\n"),
                    }
                } else {
                    eprintln!("\nPlugin '{p}' not found as a local path.");
                    println!("Try: /plugin install <git-url> or /plugin install <plugin>@<marketplace>\n");
                }
            }
            (None, _) => {
                let available = plugin_manager.list_available_plugins();
                if available.is_empty() {
                    println!("\nNo marketplaces configured.");
                    println!("Add a marketplace: /plugin marketplace add <path-or-url>");
                    println!("Install from local directory: /plugin install /path/to/plugin");
                    println!("Install from git: /plugin install <git-url>\n");
                } else {
                    println!("\n=== Available Plugins ===\n");
                    for (marketplace, plugin) in &available {
                        let desc = plugin.description.as_deref().unwrap_or("No description");
                        println!("  {}@{} - {}", plugin.name, marketplace, desc);
                    }
                    println!("\nInstall: /plugin install <plugin-name>@<marketplace>\n");
                }
            }
        },
        PluginAction::Manage => {
            let all: Vec<_> = plugin_manager.all().collect();
            if all.is_empty() {
                println!("\nNo plugins installed.\n");
            } else {
                println!("\n=== Plugin Management ===\n");
                for plugin in &all {
                    let status = if plugin.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    };
                    println!("  {} [{}]", plugin.name(), status);
                    println!("    Path: {}", plugin.root().display());
                }
                println!();
                println!("  /plugin enable <name>    - Enable a plugin");
                println!("  /plugin disable <name>   - Disable a plugin");
                println!("  /plugin uninstall <name> - Remove a plugin\n");
            }
        }
        PluginAction::Uninstall { plugin } => {
            let mut installed = plugins::InstalledPlugins::load();
            if installed.remove(&plugin) {
                if let Err(e) = installed.save() {
                    tracing::warn!("Failed to save install tracking: {}", e);
                }
                let plugins_dir = std::path::PathBuf::from(".openclaudia/plugins");
                let plugin_dir = plugins_dir.join(&plugin);
                if plugin_dir.exists() {
                    if let Err(e) = fs::remove_dir_all(&plugin_dir) {
                        eprintln!("Warning: Could not remove plugin directory: {e}");
                    }
                }
                let _ = plugin_manager.reload();
                println!("\nUninstalled plugin '{plugin}'. Restart to apply changes.\n");
            } else {
                eprintln!("\nPlugin '{plugin}' not found in install tracking.\n");
            }
        }
        PluginAction::Enable { plugin } => match plugin_manager.enable(&plugin) {
            Ok(()) => println!("\nEnabled plugin '{plugin}'. Restart to apply changes.\n"),
            Err(e) => eprintln!("\nFailed to enable plugin: {e}\n"),
        },
        PluginAction::Disable { plugin } => match plugin_manager.disable(&plugin) {
            Ok(()) => println!("\nDisabled plugin '{plugin}'. Restart to apply changes.\n"),
            Err(e) => eprintln!("\nFailed to disable plugin: {e}\n"),
        },
        PluginAction::Validate { path } => {
            let target = path.unwrap_or_else(|| ".".to_string());
            let target_path = std::path::Path::new(&target);

            if target_path.is_dir() {
                match plugins::Plugin::load(target_path) {
                    Ok(plugin) => {
                        println!("\n=== Plugin Validation: PASSED ===\n");
                        println!("  Name:        {}", plugin.name());
                        println!(
                            "  Version:     {}",
                            plugin.manifest.version.as_deref().unwrap_or("not set")
                        );
                        if let Some(desc) = &plugin.manifest.description {
                            println!("  Description: {desc}");
                        }
                        let cmds = plugin.resolved_commands();
                        let hooks = plugin.resolved_hooks();
                        let mcps = plugin.resolved_mcp_servers();
                        println!("  Commands:    {}", cmds.len());
                        println!("  Hooks:       {}", hooks.len());
                        println!("  MCP Servers: {}", mcps.len());
                        println!();
                    }
                    Err(e) => {
                        println!("\n=== Plugin Validation: FAILED ===\n");
                        println!("  Error: {e}\n");
                    }
                }
            } else if target_path.is_file() {
                match fs::read_to_string(target_path) {
                    Ok(content) => {
                        if let Ok(manifest) =
                            serde_json::from_str::<plugins::PluginManifest>(&content)
                        {
                            println!("\n=== Manifest Validation: PASSED ===\n");
                            println!("  Name:    {}", manifest.name);
                            println!(
                                "  Version: {}",
                                manifest.version.as_deref().unwrap_or("not set")
                            );
                            println!();
                        } else if let Ok(marketplace) =
                            serde_json::from_str::<plugins::MarketplaceManifest>(&content)
                        {
                            println!("\n=== Marketplace Manifest: PASSED ===\n");
                            println!("  Name:    {}", marketplace.name);
                            println!("  Plugins: {}", marketplace.plugins.len());
                            println!();
                        } else {
                            println!("\n=== Manifest Validation: FAILED ===\n");
                            println!("  Could not parse as plugin or marketplace manifest.\n");
                        }
                    }
                    Err(e) => {
                        println!("\n=== Manifest Validation: FAILED ===\n");
                        println!("  Could not read file: {e}\n");
                    }
                }
            } else {
                println!("\nPath not found: {target}\n");
                println!("Usage: /plugin validate <path>");
                println!(
                    "  /plugin validate .                          - Validate current directory"
                );
                println!("  /plugin validate .claude-plugin/plugin.json - Validate manifest file");
                println!("  /plugin validate /path/to/plugin-directory\n");
            }
        }
        PluginAction::Marketplace { action, target } => match action.as_deref() {
            Some("add") => {
                if let Some(t) = &target {
                    let path = std::path::Path::new(t.as_str());
                    if path.exists() && path.is_dir() {
                        match plugin_manager.add_marketplace_from_directory(path) {
                            Ok(manifest) => println!(
                                "\nAdded marketplace '{}' ({} plugins).\n",
                                manifest.name,
                                manifest.plugins.len()
                            ),
                            Err(e) => eprintln!("\nFailed to add marketplace: {e}\n"),
                        }
                    } else if t.contains('/')
                        || std::path::Path::new(t)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("git"))
                        || t.starts_with("http")
                    {
                        println!("\nCloning marketplace from '{t}'...");
                        match plugin_manager.add_marketplace_from_git(t, None) {
                            Ok(manifest) => println!(
                                "Added marketplace '{}' ({} plugins).\n",
                                manifest.name,
                                manifest.plugins.len()
                            ),
                            Err(e) => eprintln!("Failed to add marketplace: {e}\n"),
                        }
                    } else {
                        eprintln!("\nCould not resolve '{t}' as a path or URL.\n");
                    }
                } else {
                    println!("\nUsage: /plugin marketplace add <path-or-url>\n");
                }
            }
            Some("remove" | "rm") => {
                if let Some(t) = &target {
                    match plugin_manager.remove_marketplace(t) {
                        Ok(()) => println!("\nRemoved marketplace '{t}'.\n"),
                        Err(e) => eprintln!("\nFailed to remove marketplace: {e}\n"),
                    }
                } else {
                    println!("\nUsage: /plugin marketplace remove <name>\n");
                }
            }
            Some("update") => {
                let marketplaces = plugin_manager.list_marketplaces();
                if marketplaces.is_empty() {
                    println!("\nNo marketplaces installed.\n");
                } else if let Some(t) = &target {
                    match plugin_manager.update_marketplace(t) {
                        Ok(manifest) => println!(
                            "\nUpdated marketplace '{}' ({} plugins).\n",
                            manifest.name,
                            manifest.plugins.len()
                        ),
                        Err(e) => eprintln!("\nFailed to update '{t}': {e}\n"),
                    }
                } else {
                    println!("\nUpdating {} marketplace(s)...", marketplaces.len());
                    for (name, _) in &marketplaces {
                        match plugin_manager.update_marketplace(name) {
                            Ok(m) => {
                                println!("  {} - updated ({} plugins)", name, m.plugins.len());
                            }
                            Err(e) => eprintln!("  {name} - failed: {e}"),
                        }
                    }
                    println!();
                }
            }
            Some("list") => {
                let marketplaces = plugin_manager.list_marketplaces();
                if marketplaces.is_empty() {
                    println!("\nNo marketplaces installed.");
                    println!("Use /plugin marketplace add <path-or-url> to add one.\n");
                } else {
                    println!(
                        "\n=== Installed Marketplaces ({}) ===\n",
                        marketplaces.len()
                    );
                    for (name, manifest) in &marketplaces {
                        println!("  {} ({} plugins)", name, manifest.plugins.len());
                        for plugin in &manifest.plugins {
                            let desc = plugin.description.as_deref().unwrap_or("No description");
                            println!("    - {} - {}", plugin.name, desc);
                        }
                    }
                    println!("\nInstall: /plugin install <plugin>@<marketplace>\n");
                }
            }
            _ => {
                println!("\nMarketplace Commands:");
                println!("  /plugin marketplace add <path/url> - Add a marketplace");
                println!("  /plugin marketplace remove <name>  - Remove a marketplace");
                println!("  /plugin marketplace update         - Update all marketplaces");
                println!("  /plugin marketplace list           - List marketplaces\n");
            }
        },
        PluginAction::Reload => {
            let errors = plugin_manager.reload();
            println!("\nReloaded plugins: {} loaded", plugin_manager.count());
            for err in &errors {
                eprintln!("  Error: {err}");
            }
            println!();
        }
        PluginAction::RunCommand {
            plugin_name,
            command_name,
        } => {
            if let Some(plugin) = plugin_manager.get(&plugin_name) {
                let commands = plugin.resolved_commands();
                if let Some(cmd) = commands.iter().find(|c| c.name == command_name) {
                    println!("\n--- /{plugin_name}: {command_name} ---\n");
                    println!("{}", cmd.content);
                    println!();
                } else {
                    let available: Vec<_> = commands.iter().map(|c| c.name.clone()).collect();
                    eprintln!("\nCommand '{command_name}' not found in plugin '{plugin_name}'.");
                    if available.is_empty() {
                        eprintln!("This plugin has no commands.\n");
                    } else {
                        eprintln!("Available: {}\n", available.join(", "));
                    }
                }
            } else {
                eprintln!(
                    "\nPlugin '{plugin_name}' not found. Use /plugin to see installed plugins.\n"
                );
            }
        }
    }
}
