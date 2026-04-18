//! `OpenClaudia` - Open-source universal agent harness
//!
//! Provides Claude Code-like capabilities for any AI agent.

// Per project policy (CLAUDE.md "no_allow_dead_code" rule + crosslink
// #461), blanket pedantic-lint suppressions are not allowed here. Each
// individual offense surfaced by `cargo clippy -W clippy::pedantic` is
// tracked in the clippy-strict issue batch (#384 uninlined_format_args,
// #385 doc_markdown, #387 unreadable_literal, #394 needless_raw_string_hashes,
// #402 must_use_candidate, #424 too_many_lines / god-functions, etc.).
// Default `cargo build` and non-pedantic `cargo clippy` are unaffected.

mod cli;

use openclaudia::{
    config, guardrails, memory, permissions::PermissionManager, plugins, prompt, proxy,
    proxy::normalize_base_url,
    session, tool_intercept,
    tools::{self, safe_truncate},
    tui, vdd,
};

use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Re-import extracted types and functions used heavily in cmd_chat
use cli::display::tips::get_random_tip;
use cli::repl::input::expand_file_references;
use cli::repl::keybindings::{display_keybindings, execute_key_action, key_event_to_string};
use cli::repl::permissions::execute_shell_command_with_permission;
use cli::repl::plan_mode::{check_plan_mode_restriction, process_tool_result_marker};
use cli::repl::session_io::{
    compact_chat_session, estimate_session_tokens, export_chat_session,
    save_session_to_short_term_memory,
};
use cli::repl::slash::{
    handle_activity_command, handle_memory_command, handle_plugin_action, handle_slash_command,
    SlashCommandResult,
};
use cli::repl::vim::{self, VimState};
use cli::repl::{
    get_history_path, list_chat_sessions, load_chat_session, save_chat_session, ChatSession,
};

#[derive(Parser)]
#[command(name = "openclaudia")]
#[command(author, version, about = "Open-source universal agent harness")]
#[allow(clippy::struct_excessive_bools)] // CLI flags are naturally boolean
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Model to use for chat
    #[arg(short, long, global = true)]
    model: Option<String>,

    /// Resume the most recent chat session
    #[arg(long, alias = "continue")]
    resume: bool,

    /// Resume a specific session by ID (prefix match)
    #[arg(long)]
    session_id: Option<String>,

    /// Run in coordinator mode (multi-agent orchestration)
    #[arg(long)]
    coordinator: bool,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Skip all interactive permission prompts (auto-allow everything).
    /// WARNING: Only use in CI/automation. Disables safety prompts for write/destructive tools.
    #[arg(long)]
    dangerously_skip_permissions: bool,

    /// Launch full-screen interactive TUI (experimental)
    #[arg(long)]
    tui_mode: bool,

    /// Behavioral mode preset (create, extend, safe, refactor, explore, debug, methodical, director)
    #[arg(long, value_name = "PRESET")]
    mode: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize `OpenClaudia` configuration in the current directory
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

    /// Start the `OpenClaudia` proxy server
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

    /// Start ACP server on stdin/stdout for agent interoperability (acpx)
    Acp {
        /// Target provider (overrides config)
        #[arg(short, long)]
        target: Option<String>,

        /// Model to use
        #[arg(short, long)]
        model: Option<String>,
    },

    /// Run in iteration/loop mode with Stop hooks
    Loop {
        /// Maximum number of iterations (0 = unlimited)
        #[arg(short = 'n', long, default_value = "0")]
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
        None if cli.tui_mode => {
            // Legacy rustyline REPL (--tui-mode is now the escape hatch name, kept for compat)
            cmd_chat(
                cli.model,
                cli.resume,
                cli.session_id,
                cli.coordinator,
                cli.dangerously_skip_permissions,
                cli.mode,
            )
            .await
        }
        None => {
            // Default: full-screen TUI
            cmd_tui(cli.model).await
        }
        Some(Commands::Init { force }) => cli::commands::init::cmd_init(force),
        Some(Commands::Auth { status, logout }) => {
            cli::commands::auth::cmd_auth(status, logout).await
        }
        Some(Commands::Acp {
            target,
            model: acp_model,
        }) => cli::commands::acp::cmd_acp(target, acp_model.or(cli.model)).await,
        Some(Commands::Start { port, host, target }) => {
            cli::commands::start::cmd_start(port, host, target).await
        }
        Some(Commands::Config) => {
            cli::commands::config_cmd::cmd_config();
            Ok(())
        }
        Some(Commands::Doctor) => cli::commands::doctor::cmd_doctor().await,
        Some(Commands::Loop {
            max_iterations,
            port,
            target,
        }) => cli::commands::loop_cmd::cmd_loop(max_iterations, port, target).await,
    }
}

/// Full-screen TUI mode (default when no subcommand).
///
/// Loads config, resolves the provider/model/API key, builds the system prompt,
/// then launches the ratatui interactive TUI with the API pipeline wired up.
async fn cmd_tui(model_override: Option<String>) -> anyhow::Result<()> {
    use openclaudia::hooks::{load_claude_code_hooks, merge_hooks_config, HookEngine};
    use openclaudia::rules::RulesEngine;
    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            if config::config_file_exists() {
                eprintln!("Failed to parse configuration: {e}");
            } else {
                eprintln!("No configuration found. Run 'openclaudia init' first.");
            }
            return Ok(());
        }
    };

    // Auto-detect provider from model name
    let mut config = config;
    if let Some(ref model) = model_override {
        let detected = openclaudia::proxy::determine_provider(model, &config);
        if detected != config.proxy.target {
            config.proxy.target = detected;
        }
    }

    let provider = if let Some(p) = config.active_provider() {
        p
    } else {
        eprintln!(
            "No provider configured for target '{}'",
            config.proxy.target
        );
        return Ok(());
    };

    // Resolve API key (same logic as cmd_chat)
    let mut claude_code_token: Option<String> = None;

    // `api_key` is `Option<ApiKey>` rather than a raw String to keep the
    // log-safe newtype semantics (crosslink #256). In the OAuth/Claude-Code
    // path it's `None` — `resolve_headers` ignores it when `claude_code_token`
    // is provided. Otherwise we require a real key from provider config.
    let api_key: Option<openclaudia::providers::ApiKey> =
        if config.proxy.target == "anthropic" && provider.api_key.is_none() {
            if openclaudia::claude_credentials::has_claude_code_credentials() {
                match openclaudia::claude_credentials::load_credentials().await {
                    Ok(creds) => {
                        claude_code_token = Some(creds.access_token);
                        None
                    }
                    Err(e) => {
                        eprintln!("Error: Claude Code credentials unusable: {e}");
                        eprintln!(
                            "Install Claude Code and run `claude` to log in, or set ANTHROPIC_API_KEY."
                        );
                        return Ok(());
                    }
                }
            } else {
                eprintln!("No API key configured for Anthropic.");
                eprintln!("Install Claude Code and run `claude` to log in, or set ANTHROPIC_API_KEY.");
                return Ok(());
            }
        } else if let Some(k) = &provider.api_key {
            Some(k.clone())
        } else {
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

    let model = model_override
        .or_else(|| provider.model.clone())
        .unwrap_or_else(|| match config.proxy.target.as_str() {
            "anthropic" => "claude-sonnet-4-6".to_string(),
            "openai" => "gpt-5.2".to_string(),
            "google" => "gemini-2.5-flash".to_string(),
            "zai" => "glm-5".to_string(),
            "deepseek" => "deepseek-chat".to_string(),
            "qwen" => "qwen3.5-plus".to_string(),
            _ => "gpt-5.2".to_string(),
        });

    // Resolve endpoint
    let endpoint = openclaudia::pipeline::resolve_endpoint(
        &config.proxy.target,
        &model,
        &provider.base_url,
        claude_code_token.as_deref(),
    );

    // Resolve headers
    let headers = openclaudia::pipeline::resolve_headers(
        &config.proxy.target,
        api_key.as_ref(),
        claude_code_token.as_deref(),
        &provider
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>(),
    );

    // Initialize guardrails
    guardrails::configure(&config.guardrails);

    // Initialize memory database
    let cwd_path = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let memory_db: Option<memory::MemoryDb> = memory::MemoryDb::open_for_project(&cwd_path).ok();

    // Build system prompt blocks (stable prefix + dynamic suffix for cache efficiency)
    let cwd = cwd_path.to_string_lossy().to_string();
    let tui_prompt_blocks = prompt::build_system_prompt_blocks(
        &openclaudia::modes::BehaviorMode::default(),
        None, // Hook instructions injected per-turn, not at init
        None,
        memory_db.as_ref(),
        Some(&cwd),
    );
    let system_prompt = tui_prompt_blocks.to_combined();

    // Initialize hook engine
    let claude_hooks = load_claude_code_hooks();
    let merged_hooks = merge_hooks_config(config.hooks.clone(), claude_hooks);
    let hook_engine = std::sync::Arc::new(HookEngine::new(merged_hooks));

    // Initialize rules engine and load rules
    let rules_engine = RulesEngine::new(".openclaudia/rules");
    let rules_content = {
        let extensions: Vec<&str> = vec!["rs", "py", "ts", "js", "go", "java", "rb", "md"];
        let content = rules_engine.get_combined_rules(&extensions);
        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    };

    // Build and launch the TUI
    let mut app = tui::app::App::new(&model, &config.proxy.target);
    app.set_api_config(
        endpoint,
        headers,
        system_prompt,
        Some(tui_prompt_blocks),
        claude_code_token,
    );
    app.hook_engine = Some(hook_engine);
    app.memory_db = memory_db.map(std::sync::Arc::new);
    app.rules_content = rules_content;
    app.run().map_err(|e| anyhow::anyhow!("TUI error: {e}"))
}

/// Result of an interactive permission prompt for a tool call.
enum ToolPermissionResult {
    /// User allowed execution (or tool doesn't need permission).
    Allowed,
    /// User denied execution.
    Denied(String),
}

/// Check whether a tool call requires interactive permission and prompt the user if so.
///
/// Read-only / informational tools execute without prompting. Write/destructive tools
/// (bash, `write_file`, `edit_file`, etc.) prompt the user unless:
/// - `skip_permissions` is true (--dangerously-skip-permissions flag)
/// - The tool has been marked "always allow" for this session
///
/// Returns `Allowed` to proceed, or `Denied(message)` to send back to the model.
fn check_tool_permission_interactive(
    tool_name: &str,
    tool_args: &serde_json::Value,
    skip_permissions: bool,
    always_allowed: &mut std::collections::HashSet<String>,
) -> ToolPermissionResult {
    // Tools that never need permission (read-only / informational)
    let needs_permission = !matches!(
        tool_name,
        "read_file"
            | "list_files"
            | "grep"
            | "glob"
            | "web_fetch"
            | "web_search"
            | "ask_user_question"
            | "task_create"
            | "task_update"
            | "task_get"
            | "task_list"
            | "enter_plan_mode"
            | "exit_plan_mode"
            | "lsp"
            | "memory_search"
            | "core_memory_get"
    );

    if !needs_permission || skip_permissions {
        return ToolPermissionResult::Allowed;
    }

    // Check session-level "always allow" cache
    if always_allowed.contains(tool_name) {
        return ToolPermissionResult::Allowed;
    }

    // Build a human-readable description of what the tool wants to do
    let description = match tool_name {
        "bash" => {
            let cmd = tool_args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            format!("Run command: {cmd}")
        }
        "write_file" => {
            let path = tool_args
                .get("file_path")
                .or_else(|| tool_args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            format!("Write file: {path}")
        }
        "edit_file" => {
            let path = tool_args
                .get("file_path")
                .or_else(|| tool_args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            format!("Edit file: {path}")
        }
        _ => format!("Execute: {tool_name}"),
    };

    eprint!("\x1b[33m⚠ {description}\x1b[0m [y/n/a(lways)] ");
    use std::io::Write as _;
    std::io::stderr().flush().ok();

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        // Non-interactive / broken pipe -> deny
        return ToolPermissionResult::Denied(format!(
            "Permission denied (non-interactive) for tool '{tool_name}'"
        ));
    }
    let response = input.trim().to_lowercase();

    match response.as_str() {
        "y" | "yes" | "" => ToolPermissionResult::Allowed,
        "a" | "always" => {
            always_allowed.insert(tool_name.to_string());
            eprintln!(
                "\x1b[32m✓ Will auto-allow '{tool_name}' for the rest of this session.\x1b[0m"
            );
            ToolPermissionResult::Allowed
        }
        _ => ToolPermissionResult::Denied(format!(
            "Permission denied by user for tool '{tool_name}'"
        )),
    }
}

/// Interactive chat mode (default command)
/// Chat-session cleanup: finalize auto-learner, autosave session,
/// persist readline history, restore terminal scroll region.
///
/// Each step is best-effort — failures in any one are logged at
/// `warn!` but do not propagate, because the CLI is already about to
/// exit. Extracted from `cmd_chat` per crosslink #262.
fn finalize_chat(
    auto_learner: &mut Option<openclaudia::auto_learn::AutoLearner>,
    chat_session: &ChatSession,
    memory_db: Option<&memory::MemoryDb>,
    rl: &mut rustyline::DefaultEditor,
    history_path: &std::path::Path,
) {
    // Finalize auto-learning (compute file relationships, etc.).
    if let Some(learner) = auto_learner.as_mut() {
        learner.on_session_end();
    }

    // Autosave to short-term memory so a future resume can pick up.
    save_session_to_short_term_memory(chat_session, memory_db);

    // Persist readline history — missing file is a warning, not an error.
    if let Err(e) = rl.save_history(history_path) {
        tracing::warn!("Failed to save history: {}", e);
    }

    // Restore the terminal scroll region before returning control.
    let _ = tui::teardown_pinned_bar();
}

/// Discover plugins and log a one-line status banner.
///
/// Wraps `PluginManager::new()` + `.discover()` + the "N plugin(s)
/// loaded" print + per-error `tracing::warn!`. Returns the manager
/// for the caller to use. Extracted from `cmd_chat` per crosslink #262.
fn init_plugin_manager() -> plugins::PluginManager {
    let mut plugin_manager = plugins::PluginManager::new();
    let plugin_errors = plugin_manager.discover();
    if plugin_manager.count() > 0 {
        println!("\x1b[90m{} plugin(s) loaded\x1b[0m", plugin_manager.count());
    }
    for err in &plugin_errors {
        tracing::warn!("Plugin error: {}", err);
    }
    plugin_manager
}

/// Initialize the rustyline editor with history file loaded.
///
/// Creates the history directory on a best-effort basis, logging a
/// warning (but never failing) if creation or load fails. Extracted
/// from `cmd_chat` per crosslink #262.
///
/// # Errors
/// Propagates errors from `DefaultEditor::new()` — these are
/// terminal-initialization failures that mean the CLI cannot run.
fn init_rustyline_with_history() -> anyhow::Result<(rustyline::DefaultEditor, std::path::PathBuf)> {
    let mut rl = rustyline::DefaultEditor::new()?;
    let history_path = get_history_path();

    if let Some(parent) = history_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, path = ?parent, "Failed to create history directory");
        }
    }

    // Missing history file on first run is expected; ignore load errors.
    let _ = rl.load_history(&history_path);

    Ok((rl, history_path))
}

/// Auto-detect the project's git root and `chdir` into it.
///
/// Silent on failure — non-git directories or missing `git` binary are
/// both valid reasons to just use the caller's current working
/// directory. Extracted from `cmd_chat` per crosslink #262
/// (god-function decomposition).
fn chdir_to_git_root() {
    let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !root.is_empty() {
        let _ = std::env::set_current_dir(&root);
    }
}

/// Resolve the model name to use for a chat session.
///
/// Priority: explicit `-m` flag > provider's configured model > a
/// per-target hard-coded default. Pure function — no I/O, no mutation.
/// Extracted from `cmd_chat` per crosslink #262.
fn resolve_model_name(
    model_override: Option<String>,
    provider_model: Option<String>,
    target: &str,
) -> String {
    model_override
        .or(provider_model)
        .unwrap_or_else(|| match target {
            "anthropic" => "claude-sonnet-4-6".to_string(),
            "openai" => "gpt-5.2".to_string(),
            "google" => "gemini-2.5-flash".to_string(),
            "zai" => "glm-5".to_string(),
            "deepseek" => "deepseek-chat".to_string(),
            "qwen" => "qwen3.5-plus".to_string(),
            _ => "gpt-5.2".to_string(),
        })
}

/// Parse a behavioral-mode string (`--mode`) into a `BehaviorMode`.
/// `None` yields the default preset.
///
/// Extracted from `cmd_chat` per crosslink #262.
///
/// # Errors
/// Returns the `String` error produced by `Preset::FromStr` when the
/// user supplied an unknown preset name. The CLI layer prints it and
/// exits `Ok(())` — this helper surfaces the error rather than
/// coupling to stderr.
fn parse_initial_behavior_mode(
    mode_override: Option<&str>,
) -> Result<openclaudia::modes::BehaviorMode, String> {
    let Some(s) = mode_override else {
        return Ok(openclaudia::modes::BehaviorMode::default());
    };
    let preset: openclaudia::modes::Preset = s.parse()?;
    Ok(openclaudia::modes::BehaviorMode::from_preset(preset))
}

/// Outcome of resolving authentication for a chat session.
///
/// Exactly one of `api_key` or `claude_code_token` is set (or both
/// `None` when `cmd_chat` has already printed an error and is about to
/// return). See [`resolve_chat_auth`].
struct ChatAuth {
    /// Provider API key (newtype — Debug/Display redact).
    api_key: Option<openclaudia::providers::ApiKey>,
    /// Claude Code OAuth Bearer token, when auth came from the
    /// `~/.claude/.credentials.json` store.
    claude_code_token: Option<String>,
}

/// Resolve which authentication mechanism the chat session should use.
///
/// Priority for Anthropic:
///  1. Claude Code credentials (`~/.claude/.credentials.json`) — zero
///     config, uses the active subscription.
///  2. API key from provider config / env.
///
/// Returns `Ok(None)` when authentication is impossible AND
/// `cmd_chat` should exit cleanly — each such branch prints a
/// user-facing `eprintln!` before returning. Returns `Ok(Some(_))`
/// with the chosen auth material. Returns `Err(_)` only for
/// unexpected I/O errors. Extracted from `cmd_chat` per crosslink #262.
async fn resolve_chat_auth(
    target: &str,
    provider: &openclaudia::config::ProviderConfig,
) -> anyhow::Result<Option<ChatAuth>> {
    // Anthropic / no API-key branch: try Claude Code first.
    if target == "anthropic" && provider.api_key.is_none() {
        if !openclaudia::claude_credentials::has_claude_code_credentials() {
            eprintln!("No API key configured for Anthropic.");
            eprintln!(
                "Install Claude Code and run `claude` to log in, or set ANTHROPIC_API_KEY."
            );
            return Ok(None);
        }
        match openclaudia::claude_credentials::load_credentials().await {
            Ok(creds) => {
                eprintln!(
                    "✓ Authenticated via Claude Code ({}, {})",
                    creds.subscription_type.as_deref().unwrap_or("unknown"),
                    creds.rate_limit_tier.as_deref().unwrap_or("default"),
                );
                return Ok(Some(ChatAuth {
                    api_key: None,
                    claude_code_token: Some(creds.access_token),
                }));
            }
            Err(e) => {
                eprintln!("Error: Claude Code credentials unusable: {e}");
                eprintln!(
                    "Install Claude Code and run `claude` to log in, or set ANTHROPIC_API_KEY."
                );
                return Ok(None);
            }
        }
    }

    if let Some(k) = &provider.api_key {
        return Ok(Some(ChatAuth {
            api_key: Some(k.clone()),
            claude_code_token: None,
        }));
    }

    let env_var = match target {
        "openai" => "OPENAI_API_KEY",
        "google" => "GOOGLE_API_KEY",
        "zai" => "ZAI_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "qwen" => "QWEN_API_KEY",
        _ => "API_KEY",
    };
    eprintln!("No API key configured for '{target}'. Set {env_var} or add to config.");
    Ok(None)
}

async fn cmd_chat(
    model_override: Option<String>,
    resume: bool,
    session_id: Option<String>,
    coordinator: bool,
    dangerously_skip_permissions: bool,
    mode_override: Option<String>,
) -> anyhow::Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    use openclaudia::hooks::{
        load_claude_code_hooks, merge_hooks_config, HookEngine, HookEvent, HookInput,
    };
    use openclaudia::providers::{
        convert_messages_to_anthropic, convert_tools_to_anthropic, get_adapter,
    };
    use openclaudia::rules::RulesEngine;
    use rustyline::error::ReadlineError;
    use rustyline::{Config, DefaultEditor, EditMode, Editor};

    chdir_to_git_root();

    // Compile regex once for file extension extraction
    let ext_regex = regex::Regex::new(r"[\w/\\.-]+\.([a-zA-Z0-9]{1,10})\b").unwrap();

    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            if config::config_file_exists() {
                eprintln!("Failed to parse configuration: {e}");
                eprintln!("Check your .openclaudia/config.yaml for syntax errors.");
            } else {
                eprintln!("No configuration found. Run 'openclaudia init' first.");
            }
            return Ok(());
        }
    };

    // If -m flag specifies a model, auto-detect the provider from model name
    let mut config = config;
    if let Some(ref model) = model_override {
        let detected = openclaudia::proxy::determine_provider(model, &config);
        if detected != config.proxy.target {
            eprintln!(
                "[debug] Model '{}' detected as provider '{}' (overriding target '{}')",
                model, detected, config.proxy.target
            );
            config.proxy.target = detected;
        }
    }

    let initial_behavior_mode = match parse_initial_behavior_mode(mode_override.as_deref()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return Ok(());
        }
    };

    // Initialize guardrails engine from config
    guardrails::configure(&config.guardrails);

    let provider = if let Some(p) = config.active_provider() {
        p
    } else {
        eprintln!(
            "No provider configured for target '{}'",
            config.proxy.target
        );
        return Ok(());
    };

    let Some(ChatAuth {
        api_key,
        claude_code_token,
    }) = resolve_chat_auth(&config.proxy.target, provider).await?
    else {
        // `resolve_chat_auth` already printed a user-facing error.
        return Ok(());
    };

    let mut model = resolve_model_name(
        model_override,
        provider.model.clone(),
        &config.proxy.target,
    );

    let adapter = get_adapter(&config.proxy.target);
    let client = reqwest::Client::new();

    // Initialize hook engine with merged hooks (config + Claude Code hooks)
    let claude_hooks = load_claude_code_hooks();
    let merged_hooks = merge_hooks_config(config.hooks.clone(), claude_hooks);
    let hook_engine = HookEngine::new(merged_hooks);

    // Initialize rules engine
    let rules_engine = RulesEngine::new(".openclaudia/rules");

    let mut plugin_manager = init_plugin_manager();

    let (mut rl, history_path) = init_rustyline_with_history()?;

    // Clear screen and render TUI welcome screen
    let _ = tui::clear_screen();
    let welcome = tui::WelcomeScreen::new(env!("CARGO_PKG_VERSION"), &config.proxy.target, &model);
    if let Err(e) = welcome.render() {
        // Fallback to simple output if TUI fails
        eprintln!("TUI render failed: {e}, using simple output");
        println!("OpenClaudia v{}", env!("CARGO_PKG_VERSION"));
        println!("Provider: {} | Model: {}", config.proxy.target, model);
        println!("Type /help for commands, /sessions to list saved chats");
        println!("Tip: {}\n", get_random_tip());
    }

    // Set up pinned bottom bar using ANSI scroll region
    let _ = tui::setup_pinned_bar();

    // Initialize chat session with behavioral mode
    let mut chat_session = ChatSession::new(&model, &config.proxy.target, initial_behavior_mode);

    // Resume a previous session if --resume or --session-id was specified
    if resume || session_id.is_some() {
        let sessions = list_chat_sessions();
        let target = if let Some(ref id) = session_id {
            sessions.iter().find(|s| s.id.starts_with(id)).cloned()
        } else {
            sessions.into_iter().next()
        };
        if let Some(loaded) = target {
            eprintln!("Resuming session: {} ({})", loaded.title, &loaded.id[..8]);
            chat_session = loaded;
        } else {
            eprintln!("No session found to resume. Starting new session.");
        }
    }

    // Load saved theme (or default)
    let mut active_theme = tui::Theme::load();

    // Vim mode state (toggled via /vim)
    let mut vim_enabled = false;
    let mut vim_state = VimState::new();

    // Effort level (toggled via /effort)
    let mut effort_level = "medium".to_string();

    // Initialize audit logger for this session
    let mut audit_logger = openclaudia::session::AuditLogger::new(&chat_session.id);

    // Initialize memory database (always-on for auto-learning)
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let memory_db: Option<memory::MemoryDb> = match memory::MemoryDb::open_for_project(&cwd) {
        Ok(db) => {
            // Show short-term memory status
            let recent_count = db.get_recent_sessions(10).map(|s| s.len()).unwrap_or(0);
            if recent_count > 0 {
                println!("\x1b[90m📝 {recent_count} recent session(s) loaded from memory\x1b[0m");
            }

            // Show auto-learning stats
            if let Ok(stats) = db.auto_learn_stats() {
                let total = stats.coding_patterns
                    + stats.error_patterns
                    + stats.learned_preferences
                    + stats.file_relationships;
                if total > 0 {
                    println!(
                        "\x1b[90m🧠 Auto-learned: {} patterns, {} error fixes, {} preferences, {} file relationships\x1b[0m",
                        stats.coding_patterns, stats.errors_resolved, stats.learned_preferences, stats.file_relationships
                    );
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

    // Initialize auto-learner (captures knowledge from tool signals)
    let mut auto_learner: Option<openclaudia::auto_learn::AutoLearner> = memory_db
        .as_ref()
        .map(openclaudia::auto_learn::AutoLearner::new);

    // Initialize permissions cache for sensitive operations
    let mut permissions: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Session-level set of tools the user has chosen to "always allow" during this session.
    // Populated when the user responds with 'a'/'always' at a permission prompt.
    let mut always_allowed_tools: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Library-layer PermissionManager — shares `default_allow` with the
    // interactive gate. The manager is plumbed into every dispatch call
    // (`execute_tool_with_memory` + `execute_tool`) so no tool can bypass
    // the check, matching the gated-dispatch invariant introduced by
    // crosslink #460 and closing follow-up #505. Kept in parallel with
    // the interactive `always_allowed_tools` set for now — unifying them
    // is a larger refactor tracked separately.
    let permission_mgr = PermissionManager::new(
        std::path::PathBuf::from(".openclaudia/permissions.json"),
        true,
        config.permissions.default_allow.clone(),
    );

    // Initialize VDD engine if enabled
    let vdd_engine: Option<vdd::VddEngine> = if config.vdd.enabled {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        println!(
            "\x1b[33m🔍 VDD enabled ({} mode) - adversary: {}\x1b[0m",
            config.vdd.mode, config.vdd.adversary.provider
        );
        Some(vdd::VddEngine::new(&config.vdd, &config, http_client))
    } else {
        None
    };

    loop {
        // Render separator, status bar, then prompt appears on next line
        let behavior_name = chat_session.behavior_mode.display_name();
        let mode_str = format!(
            "{} ({})",
            chat_session.mode.display().to_lowercase(),
            behavior_name,
        );
        let _ = tui::render_input_prompt(&mode_str);
        let _ = tui::render_bottom_bar(&effort_level, &mode_str);

        let prompt = if vim_enabled {
            // Show pending command in prompt (e.g., "d…" while waiting for motion)
            let pending = vim_state.pending_display();
            let status = vim::status_description(&vim_state);
            // Reference fields to keep them alive for future use
            let _ = vim_state.yank_buffer.len();
            let _ = vim_state.last_find.is_some();
            let _ = vim::describe_action(&vim::VimAction::None);
            if vim_state.is_pending() {
                format!("{status} {pending} \u{203A} ")
            } else {
                format!("{status} \u{203A} ")
            }
        } else {
            "\u{203A} ".to_string()
        };
        let readline = rl.readline(&prompt);

        match readline {
            Ok(line) => {
                let mut input = line.trim().to_string();
                let mut editor_message_added = false;

                // When vim enabled, sync state machine to Insert mode
                // (rustyline returns to insert after Enter)
                if vim_enabled {
                    let _ = vim_state.process_key("Escape"); // ensure Normal tracking
                    let _ = vim_state.process_key("i"); // back to Insert for next prompt
                }

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
                let mut input = input.clone();

                // Handle slash commands
                if let Some(result) = handle_slash_command(
                    &input,
                    &mut chat_session.messages,
                    &config.proxy.target,
                    &model,
                ) {
                    match result {
                        SlashCommandResult::Exit => {
                            // Save session to short-term memory before exiting
                            save_session_to_short_term_memory(&chat_session, memory_db.as_ref());
                            break;
                        }
                        SlashCommandResult::Clear => {
                            // Save current session before starting new one
                            save_session_to_short_term_memory(&chat_session, memory_db.as_ref());
                            let prev_mode = chat_session.behavior_mode.clone();
                            chat_session =
                                ChatSession::new(&model, &config.proxy.target, prev_mode);
                            continue;
                        }
                        SlashCommandResult::LoadSession(session_id) => {
                            // Load the requested session
                            if let Some(loaded) = load_chat_session(&session_id) {
                                chat_session = loaded;
                                println!(
                                    "Loaded {} messages from previous session.\n",
                                    chat_session.messages.len()
                                );
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
                                println!("\nCompacted: ~{before} tokens -> ~{after} tokens\n");
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
                                println!(
                                    "\nUndone last exchange. {} messages remaining.\n",
                                    chat_session.messages.len()
                                );
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
                                println!(
                                    "\nRedone last exchange. {} messages now.\n",
                                    chat_session.messages.len()
                                );
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
                            let duration =
                                chrono::Utc::now().signed_duration_since(chat_session.created_at);
                            let mins = duration.num_minutes();

                            println!("\n=== Session Status ===");
                            println!("  Session ID: {}...", safe_truncate(&chat_session.id, 8));
                            println!("  Title:      {}", chat_session.title);
                            println!("  Provider:   {}", chat_session.provider);
                            println!("  Model:      {}", chat_session.model);
                            println!("  Behavior:   {}", chat_session.behavior_mode.description());
                            println!(
                                "  Mode:       {} ({})",
                                chat_session.mode.display(),
                                chat_session.mode.description()
                            );
                            println!("  Messages:   {msg_count}");
                            println!("  Est tokens: ~{tokens}");

                            // Show estimated cost if pricing is available
                            if let Some(pricing) = session::get_pricing(&chat_session.model) {
                                let est_input = tokens as u64;
                                let usage = openclaudia::session::TokenUsage {
                                    input_tokens: est_input,
                                    output_tokens: est_input / 4, // rough estimate
                                    cache_read_tokens: 0,
                                    cache_write_tokens: 0,
                                };
                                if let Some(cost) =
                                    session::calculate_cost(&chat_session.model, &usage)
                                {
                                    println!("  Est cost:   ${cost:.4}");
                                }
                                println!(
                                    "  Pricing:    ${}/M in, ${}/M out",
                                    pricing.input_per_million, pricing.output_per_million
                                );
                            }

                            println!("  Duration:   {mins} min");
                            println!(
                                "  Created:    {}",
                                chat_session.created_at.format("%Y-%m-%d %H:%M UTC")
                            );
                            println!("  Theme:      {}", active_theme.name);
                            println!();
                            continue;
                        }
                        SlashCommandResult::ToggleMode => {
                            chat_session.mode = chat_session.mode.toggle();
                            println!(
                                "\nSwitched to {} mode: {}\n",
                                chat_session.mode.display(),
                                chat_session.mode.description()
                            );
                            continue;
                        }
                        SlashCommandResult::Keybindings => {
                            display_keybindings(&config.keybindings);
                            continue;
                        }
                        SlashCommandResult::Rename(new_title) => {
                            chat_session.title.clone_from(&new_title);
                            chat_session.touch();
                            if let Err(e) = save_chat_session(&chat_session) {
                                tracing::warn!("Failed to save session: {}", e);
                            }
                            println!("\nSession renamed to: {new_title}\n");
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
                        SlashCommandResult::Plugin(action) => {
                            handle_plugin_action(action, &mut plugin_manager);
                            continue;
                        }
                        SlashCommandResult::ThemeChanged(name) => {
                            if let Some(theme) = tui::Theme::from_name(&name) {
                                active_theme = theme;
                            }
                            continue;
                        }
                        SlashCommandResult::ToggleVim => {
                            vim_enabled = !vim_enabled;
                            if vim_enabled {
                                // Switch rustyline to Vi edit mode
                                rl = Editor::with_config(
                                    Config::builder().edit_mode(EditMode::Vi).build(),
                                )
                                .unwrap_or_else(|_| {
                                    DefaultEditor::new()
                                        .expect("Failed to initialize terminal editor")
                                });
                                let _ = rl.load_history(&history_path);
                                vim_state = VimState::new();
                                eprintln!("Vim mode enabled (rustyline Vi mode)");
                            } else {
                                // Switch back to Emacs edit mode
                                rl = Editor::with_config(
                                    Config::builder().edit_mode(EditMode::Emacs).build(),
                                )
                                .unwrap_or_else(|_| {
                                    DefaultEditor::new()
                                        .expect("Failed to initialize terminal editor")
                                });
                                let _ = rl.load_history(&history_path);
                                eprintln!("Vim mode disabled (Emacs mode)");
                            }
                            continue;
                        }
                        SlashCommandResult::SetEffort(level) => {
                            effort_level = level;
                            continue;
                        }
                        SlashCommandResult::CycleEffort => {
                            effort_level = match effort_level.as_str() {
                                "low" => "medium".to_string(),
                                "medium" => "high".to_string(),
                                _ => "low".to_string(),
                            };
                            let label = match effort_level.as_str() {
                                "low" => "\x1b[33mlow\x1b[0m (faster, less thorough)",
                                "high" => "\x1b[32mhigh\x1b[0m (thorough, slower)",
                                _ => "\x1b[36mmedium\x1b[0m (balanced)",
                            };
                            println!("\n\u{2713} Effort set to {label}\n");
                            continue;
                        }
                        SlashCommandResult::SetBehaviorMode(new_mode) => {
                            chat_session.behavior_mode = new_mode;
                            continue;
                        }
                        SlashCommandResult::Handled => {
                            continue;
                        }
                        SlashCommandResult::Skill(prompt) => {
                            // Inject skill prompt as the user message for this turn
                            eprintln!("\x1b[36m⚡ Running skill...\x1b[0m");
                            input = prompt;
                            // Fall through to normal message processing
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
                        expand_file_references(&input)
                    } else {
                        input.clone()
                    };

                    chat_session.messages.push(serde_json::json!({
                        "role": "user",
                        "content": expanded_input.clone()
                    }));
                    chat_session.update_title();
                    chat_session.touch();
                    // Clear undo stack since we're adding new messages
                    chat_session.clear_undo_stack();

                    // Auto-learn from user message (correction/preference detection)
                    if let Some(ref mut learner) = auto_learner {
                        // Get the last assistant message for context
                        let prev_assistant = chat_session
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
                            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                            .map(std::string::ToString::to_string);
                        learner.on_user_message(&expanded_input, prev_assistant.as_deref());
                    }

                    // Run UserPromptSubmit hooks
                    let hook_input =
                        HookInput::new(HookEvent::UserPromptSubmit).with_prompt(&expanded_input);
                    let hook_result = hook_engine
                        .run(HookEvent::UserPromptSubmit, &hook_input)
                        .await;

                    if !hook_result.allowed {
                        let reason = hook_result
                            .outputs
                            .first()
                            .and_then(|o| o.reason.clone())
                            .unwrap_or_else(|| "Request blocked by hook".to_string());
                        eprintln!("\nBlocked: {reason}\n");
                        // Save before removing the blocked message (prevent data loss)
                        let _ = save_chat_session(&chat_session);
                        chat_session.messages.pop();
                        continue;
                    }

                    // Inject hook context into messages if any
                    for output in &hook_result.outputs {
                        // JSON hooks: systemMessage field
                        if let Some(sys_msg) = &output.system_message {
                            chat_session.messages.insert(
                                0,
                                serde_json::json!({
                                    "role": "system",
                                    "content": sys_msg
                                }),
                            );
                        }
                        // Plain text hooks: additionalContext (wrapped in system-reminder like Claude Code)
                        if let Some(ctx) = &output.additional_context {
                            chat_session.messages.push(serde_json::json!({
                                "role": "system",
                                "content": format!("<system-reminder>\n{}\n</system-reminder>", ctx)
                            }));
                        }
                    }
                }

                // Extract file extensions from messages and inject rules
                let extensions: Vec<String> = chat_session
                    .messages
                    .iter()
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .flat_map(|text| {
                        ext_regex
                            .captures_iter(text)
                            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_lowercase()))
                            .collect::<Vec<_>>()
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();

                // Inject rules if we found file extensions
                if !extensions.is_empty() {
                    let rules_content = rules_engine.get_combined_rules(
                        &extensions
                            .iter()
                            .map(std::string::String::as_str)
                            .collect::<Vec<_>>(),
                    );
                    if !rules_content.is_empty()
                        && !chat_session.messages.iter().any(|m| {
                            m.get("content")
                                .and_then(|c| c.as_str())
                                .is_some_and(|s| s.contains("## Rules"))
                        })
                    {
                        // Add rules as system message at the start
                        chat_session.messages.insert(
                            0,
                            serde_json::json!({
                                "role": "system",
                                "content": rules_content
                            }),
                        );
                    }
                }

                // Build and inject Claudia's core system prompt
                // Collect any hook instructions that were injected as system messages
                let hook_instructions: Option<String> = chat_session
                    .messages
                    .iter()
                    .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .filter(|c| !c.contains("Persona: Claudia")) // Don't include our own prompt
                    .map(std::string::ToString::to_string)
                    .reduce(|acc, s| format!("{acc}\n\n{s}"));

                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Build prompt as split blocks for cache efficiency.
                // Stable prefix (identity, axes, tools, principles, comms) stays cached
                // across turns; dynamic suffix (env, memory, hooks) is reprocessed.
                let mut prompt_blocks = prompt::build_system_prompt_blocks(
                    &chat_session.behavior_mode,
                    hook_instructions.as_deref(),
                    None, // Custom instructions could come from config in future
                    memory_db.as_ref(),
                    Some(&cwd),
                );

                // Inject coordinator prompt into stable prefix (it's session-stable)
                if coordinator {
                    prompt_blocks.stable_prefix = format!(
                        "{}\n\n{}",
                        openclaudia::subagent::AgentType::Coordinator.system_prompt(),
                        prompt_blocks.stable_prefix
                    );
                }

                // Inject file-specific knowledge into dynamic suffix
                if let Some(ref db) = memory_db {
                    let mut injected_files: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for msg in chat_session.messages.iter().rev().take(10) {
                        if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                            if role == "tool" || role == "assistant" {
                                if let Some(tool_calls) =
                                    msg.get("tool_calls").and_then(|t| t.as_array())
                                {
                                    for tc in tool_calls {
                                        let name = tc
                                            .get("function")
                                            .and_then(|f| f.get("name"))
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("");
                                        if matches!(name, "read_file" | "edit_file" | "write_file")
                                        {
                                            if let Some(args_str) = tc
                                                .get("function")
                                                .and_then(|f| f.get("arguments"))
                                                .and_then(|a| a.as_str())
                                            {
                                                if let Ok(args) =
                                                    serde_json::from_str::<serde_json::Value>(
                                                        args_str,
                                                    )
                                                {
                                                    if let Some(path) = args
                                                        .get("path")
                                                        .or_else(|| args.get("file_path"))
                                                        .and_then(|p| p.as_str())
                                                    {
                                                        injected_files.insert(path.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let mut file_knowledge_parts = Vec::new();
                    for file_path in injected_files.iter().take(3) {
                        if let Ok(knowledge) = db.format_file_knowledge(file_path) {
                            if !knowledge.is_empty() {
                                file_knowledge_parts.push(knowledge);
                            }
                        }
                    }
                    if !file_knowledge_parts.is_empty() {
                        if !prompt_blocks.dynamic_suffix.is_empty() {
                            prompt_blocks.dynamic_suffix.push_str("\n\n");
                        }
                        prompt_blocks.dynamic_suffix.push_str("## File Knowledge\n");
                        prompt_blocks
                            .dynamic_suffix
                            .push_str(&file_knowledge_parts.join("\n"));
                    }
                }

                // Update system prompt in messages for all providers.
                // Non-Anthropic providers use this directly; Anthropic ignores it
                // (convert_messages_to_anthropic filters system messages) and
                // uses the multi-block prompt_blocks instead.
                //
                // We always replace rather than insert-once because mode
                // switches via /mode must be reflected on the next turn.
                let system_prompt_combined = prompt_blocks.to_combined();
                if let Some(pos) = chat_session.messages.iter().position(|m| {
                    m.get("content")
                        .and_then(|c| c.as_str())
                        .is_some_and(|s| s.contains("Persona: Claudia"))
                }) {
                    chat_session.messages[pos] = serde_json::json!({
                        "role": "system",
                        "content": system_prompt_combined
                    });
                } else {
                    chat_session.messages.insert(
                        0,
                        serde_json::json!({
                            "role": "system",
                            "content": system_prompt_combined
                        }),
                    );
                }

                // Build request body based on provider target.
                let mut request_body = if config.proxy.target == "anthropic" {
                    // Anthropic direct API mode - need proper Anthropic format
                    // Convert messages to Anthropic format (handles tool_calls and tool results)
                    let anthropic_messages = convert_messages_to_anthropic(&chat_session.messages);

                    // Get tools in OpenAI format and convert to Anthropic format
                    let openai_tools = tools::get_all_tool_definitions(true);
                    let anthropic_tools =
                        convert_tools_to_anthropic(openai_tools.as_array().unwrap_or(&vec![]));

                    let mut req = serde_json::json!({
                        "model": model,
                        "messages": anthropic_messages,
                        "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
                        "stream": true,
                        "tools": anthropic_tools
                    });

                    // Multi-block system prompt for cache efficiency:
                    // Block 0 (stable prefix): identity + axes + tools + principles + comms
                    //   → cache_control: { type: "ephemeral" } — cached across turns
                    // Block 1 (dynamic suffix): env + memory + hooks + file knowledge
                    //   → no cache_control — reprocessed each turn
                    req["system"] = openclaudia::providers::build_system_blocks(&prompt_blocks);

                    req
                } else if config.proxy.target == "google" {
                    // Google Gemini - build native Gemini format
                    // Convert OpenAI-style messages to Gemini contents
                    let openai_tools = tools::get_all_tool_definitions(true);
                    let tools_vec = openai_tools.as_array().cloned().unwrap_or_default();
                    let functions: Vec<serde_json::Value> = tools_vec.iter().filter_map(|tool| {
                        let func = tool.get("function")?;
                        Some(serde_json::json!({
                            "name": func.get("name")?,
                            "description": func.get("description").unwrap_or(&serde_json::json!("")),
                            "parameters": func.get("parameters").unwrap_or(&serde_json::json!({}))
                        }))
                    }).collect();

                    // Convert messages to Gemini contents format
                    let mut contents = Vec::new();
                    let mut system_text: Option<String> = None;
                    for msg in &chat_session.messages {
                        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        if role == "system" {
                            system_text = Some(text.to_string());
                            continue;
                        }
                        let gemini_role = if role == "assistant" { "model" } else { "user" };
                        contents.push(serde_json::json!({
                            "role": gemini_role,
                            "parts": [{"text": text}]
                        }));
                    }

                    let mut req = serde_json::json!({
                        "contents": contents,
                        "generationConfig": {"maxOutputTokens": 4096},
                        "tools": [{"functionDeclarations": functions}]
                    });
                    if let Some(sys) = system_text {
                        req["systemInstruction"] = serde_json::json!({"parts": [{"text": sys}]});
                    }
                    req
                } else {
                    // OpenAI-compatible format for other providers
                    serde_json::json!({
                        "model": model,
                        "messages": chat_session.messages,
                        "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
                        "stream": true,
                        "tools": tools::get_all_tool_definitions(true)
                    })
                };

                // Inject Claude Code system prompt for OAuth model access
                if claude_code_token.is_some() {
                    openclaudia::claude_credentials::inject_system_prompt(&mut request_body);
                }

                // Apply effort level to thinking params (Anthropic only)
                if config.proxy.target == "anthropic" {
                    match effort_level.as_str() {
                        "high" => {
                            request_body["thinking"] =
                                serde_json::json!({"type": "enabled", "budget_tokens": 10000});
                            request_body["max_tokens"] = serde_json::json!(16000);
                        }
                        "low" => {
                            request_body["max_tokens"] = serde_json::json!(2048);
                        }
                        _ => {} // medium = default behavior
                    }
                }

                // Build headers based on auth mode
                // Get endpoint - Claude Code OAuth goes direct to Anthropic API
                let endpoint = if claude_code_token.is_some() {
                    openclaudia::claude_credentials::get_oauth_endpoint(&model)
                } else {
                    format!(
                        "{}{}",
                        normalize_base_url(&provider.base_url),
                        adapter.chat_endpoint(&model)
                    )
                };
                let headers: Vec<(String, String)> = if let Some(ref token) = claude_code_token {
                    // Claude Code OAuth: Bearer token directly to Anthropic API
                    openclaudia::claude_credentials::get_oauth_headers(token)
                } else if let Some(ref key) = api_key {
                    adapter.get_headers(key)
                } else {
                    // Neither OAuth token nor api_key — earlier validation
                    // guarantees this is unreachable, but we handle it
                    // defensively rather than panicking.
                    Vec::new()
                };

                // Merge in any custom headers from provider config
                let headers: Vec<(String, String)> = {
                    let mut h = headers;
                    h.extend(provider.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
                    h
                };

                // Show spinner while connecting
                let spinner = ProgressBar::new_spinner();
                spinner.set_style(
                    ProgressStyle::default_spinner()
                        .template("{spinner:.cyan} {msg}") // ProgressStyle template, not format!
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
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
                            if config.proxy.target == "google" {
                                // ── Google Gemini: non-streaming JSON response ──
                                use std::io::Write;
                                println!();

                                let body = response.text().await.unwrap_or_default();
                                let mut full_content = String::new();

                                match serde_json::from_str::<serde_json::Value>(&body) {
                                    Ok(gemini_json) => {
                                        // Extract text from candidates[0].content.parts[].text
                                        let text: String = gemini_json
                                            .get("candidates")
                                            .and_then(|c| c.get(0))
                                            .and_then(|c| c.get("content"))
                                            .and_then(|c| c.get("parts"))
                                            .and_then(|p| p.as_array())
                                            .map(|parts| {
                                                parts
                                                    .iter()
                                                    .filter_map(|p| {
                                                        p.get("text").and_then(|t| t.as_str())
                                                    })
                                                    .collect::<Vec<_>>()
                                                    .join("")
                                            })
                                            .unwrap_or_default();

                                        if !text.is_empty() {
                                            print!("{text}");
                                            std::io::stdout().flush().ok();
                                            full_content.push_str(&text);
                                        }

                                        // Extract function calls from candidates[0].content.parts[].functionCall
                                        let mut gemini_tool_calls: Vec<tools::ToolCall> =
                                            gemini_json
                                                .get("candidates")
                                                .and_then(|c| c.get(0))
                                                .and_then(|c| c.get("content"))
                                                .and_then(|c| c.get("parts"))
                                                .and_then(|p| p.as_array())
                                                .map(|parts| {
                                                    parts
                                                        .iter()
                                                        .filter_map(|p| {
                                                            let fc = p.get("functionCall")?;
                                                            let name = fc
                                                                .get("name")?
                                                                .as_str()?
                                                                .to_string();
                                                            let args = fc
                                                                .get("args")
                                                                .map(|a| {
                                                                    serde_json::to_string(a)
                                                                        .unwrap_or_default()
                                                                })
                                                                .unwrap_or_else(|| {
                                                                    "{}".to_string()
                                                                });
                                                            Some(tools::ToolCall {
                                                                id: format!(
                                                                    "call_{}",
                                                                    uuid::Uuid::new_v4()
                                                                ),
                                                                call_type: "function".to_string(),
                                                                function: tools::FunctionCall {
                                                                    name,
                                                                    arguments: args,
                                                                },
                                                            })
                                                        })
                                                        .collect()
                                                })
                                                .unwrap_or_default();

                                        // Extract usage
                                        let input_tokens = gemini_json
                                            .get("usageMetadata")
                                            .and_then(|u| u.get("promptTokenCount"))
                                            .and_then(serde_json::Value::as_u64)
                                            .unwrap_or(0);
                                        let output_tokens = gemini_json
                                            .get("usageMetadata")
                                            .and_then(|u| u.get("candidatesTokenCount"))
                                            .and_then(serde_json::Value::as_u64)
                                            .unwrap_or(0);

                                        // Audit: log model response
                                        audit_logger.log(
                                            "model_response",
                                            &serde_json::json!({
                                                "model": &model,
                                                "content_length": full_content.len(),
                                                "tool_calls": gemini_tool_calls.len(),
                                                "cancelled": false,
                                            }),
                                        );

                                        // ── Gemini tool execution loop ──
                                        let max_iterations = config.session.max_turns;
                                        let mut iteration: u32 = 0;
                                        // Track conversation in Gemini's native format
                                        let mut gemini_contents: Vec<serde_json::Value> =
                                            serde_json::from_value(
                                                request_body
                                                    .get("contents")
                                                    .cloned()
                                                    .unwrap_or(serde_json::json!([])),
                                            )
                                            .unwrap_or_default();

                                        while !gemini_tool_calls.is_empty()
                                            && (max_iterations == 0 || iteration < max_iterations)
                                        {
                                            iteration += 1;
                                            guardrails::reset_turn();

                                            // Store model response with functionCall parts
                                            let model_parts: Vec<serde_json::Value> = {
                                                let mut parts = Vec::new();
                                                if !full_content.is_empty() {
                                                    parts.push(
                                                        serde_json::json!({"text": full_content}),
                                                    );
                                                }
                                                for tc in &gemini_tool_calls {
                                                    let args: serde_json::Value =
                                                        serde_json::from_str(
                                                            &tc.function.arguments,
                                                        )
                                                        .unwrap_or(serde_json::json!({}));
                                                    parts.push(serde_json::json!({
                                                        "functionCall": {
                                                            "name": tc.function.name,
                                                            "args": args
                                                        }
                                                    }));
                                                }
                                                parts
                                            };
                                            gemini_contents.push(serde_json::json!({
                                                "role": "model",
                                                "parts": model_parts
                                            }));

                                            // Also store in chat_session for history
                                            let tool_calls_json: Vec<serde_json::Value> =
                                                gemini_tool_calls
                                                    .iter()
                                                    .map(|tc| {
                                                        serde_json::json!({
                                                            "id": tc.id,
                                                            "type": "function",
                                                            "function": {
                                                                "name": tc.function.name,
                                                                "arguments": tc.function.arguments
                                                            }
                                                        })
                                                    })
                                                    .collect();
                                            chat_session.messages.push(serde_json::json!({
                                            "role": "assistant",
                                            "content": serde_json::Value::String(full_content.clone()),
                                            "tool_calls": tool_calls_json
                                        }));

                                            // Execute tools and collect functionResponse parts
                                            let mut function_responses: Vec<serde_json::Value> =
                                                Vec::new();
                                            for tool_call in &gemini_tool_calls {
                                                // Plan mode check
                                                if let Some(block_msg) = check_plan_mode_restriction(
                                                    &chat_session,
                                                    &tool_call.function.name,
                                                    &tool_call.function.arguments,
                                                ) {
                                                    println!("\n\x1b[33m⚠ Blocked in plan mode: {}\x1b[0m", tool_call.function.name);
                                                    function_responses.push(serde_json::json!({
                                                        "functionResponse": {
                                                            "name": tool_call.function.name,
                                                            "response": {"error": block_msg}
                                                        }
                                                    }));
                                                    chat_session.messages.push(serde_json::json!({
                                                        "role": "tool",
                                                        "tool_call_id": tool_call.id,
                                                        "content": format!("[ERROR] {}", block_msg),
                                                        "is_error": true
                                                    }));
                                                    continue;
                                                }

                                                // Permission check before execution
                                                let tool_args_val: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
                                                    .unwrap_or_else(|e| { tracing::warn!("Malformed tool arguments for '{}': {}", tool_call.function.name, e); serde_json::Value::Object(Default::default()) });
                                                match check_tool_permission_interactive(
                                                    &tool_call.function.name,
                                                    &tool_args_val,
                                                    dangerously_skip_permissions,
                                                    &mut always_allowed_tools,
                                                ) {
                                                    ToolPermissionResult::Denied(msg) => {
                                                        let _denied_content = serde_json::json!([{
                                                            "type": "tool_result",
                                                            "tool_use_id": &tool_call.id,
                                                            "is_error": true,
                                                            "content": msg
                                                        }]);
                                                        // Skip to next tool
                                                        continue;
                                                    }
                                                    ToolPermissionResult::Allowed => {}
                                                }

                                                println!(
                                                    "\n\x1b[36m⚡ Running {}...\x1b[0m",
                                                    tool_call.function.name
                                                );

                                                audit_logger.log(
                                                    "tool_call",
                                                    &serde_json::json!({
                                                        "name": &tool_call.function.name,
                                                        "arguments": &tool_call.function.arguments,
                                                        "id": &tool_call.id,
                                                    }),
                                                );

                                                // Permission manager: the interactive gate above
                                                // has already authorized this tool call at the
                                                // UX layer; the library-level gate is threaded
                                                // Library-layer gate runs in addition to the
                                                // interactive UX gate above — this enforces
                                                // the config-driven `default_allow` patterns
                                                // even when the interactive gate allowed the
                                                // tool, and closes the fail-open surface
                                                // tracked by crosslink #505 (follow-up to
                                                // #460 mandated point 2).
                                                let result = if let Some(ref db) = memory_db {
                                                    tools::execute_tool_with_memory(
                                                        tool_call,
                                                        Some(db),
                                                        Some(&permission_mgr),
                                                    )
                                                } else {
                                                    tools::execute_tool_with_memory(
                                                        tool_call,
                                                        None,
                                                        Some(&permission_mgr),
                                                    )
                                                };

                                                // Auto-learn from tool result
                                                if let Some(ref mut learner) = auto_learner {
                                                    let args: serde_json::Value =
                                                        serde_json::from_str(
                                                            &tool_call.function.arguments,
                                                        )
                                                        .unwrap_or_default();
                                                    if result.is_error {
                                                        learner.on_tool_failure(
                                                            &tool_call.function.name,
                                                            &args,
                                                            &result.content,
                                                        );
                                                    } else {
                                                        learner.on_tool_success(
                                                            &tool_call.function.name,
                                                            &args,
                                                            &result.content,
                                                        );
                                                    }
                                                }

                                                let (final_content, _was_marker) =
                                                    process_tool_result_marker(
                                                        &mut chat_session,
                                                        &tool_call.function.name,
                                                        &result.content,
                                                    );
                                                let final_is_error = if _was_marker {
                                                    false
                                                } else {
                                                    result.is_error
                                                };

                                                // Show result preview
                                                cli::display::tool_result::display_tool_result(
                                                    &tool_call.function.name,
                                                    &final_content,
                                                    final_is_error,
                                                );

                                                // Build Gemini functionResponse
                                                let response_content = if final_is_error {
                                                    serde_json::json!({"error": final_content})
                                                } else {
                                                    serde_json::json!({"result": final_content})
                                                };
                                                function_responses.push(serde_json::json!({
                                                    "functionResponse": {
                                                        "name": tool_call.function.name,
                                                        "response": response_content
                                                    }
                                                }));

                                                // Store in session
                                                let result_content = if final_is_error {
                                                    format!("[ERROR] {final_content}")
                                                } else {
                                                    final_content
                                                };
                                                chat_session.messages.push(serde_json::json!({
                                                    "role": "tool",
                                                    "tool_call_id": result.tool_call_id,
                                                    "content": result_content,
                                                    "is_error": final_is_error
                                                }));
                                            }

                                            // Add user turn with functionResponse parts
                                            gemini_contents.push(serde_json::json!({
                                                "role": "user",
                                                "parts": function_responses
                                            }));

                                            // Send follow-up to Gemini
                                            println!("\n\x1b[90m(Sending {} tool result{} to Gemini...)\x1b[0m",
                                            gemini_tool_calls.len(),
                                            if gemini_tool_calls.len() == 1 { "" } else { "s" }
                                        );

                                            let openai_tools =
                                                tools::get_all_tool_definitions(true);
                                            let tools_vec = openai_tools
                                                .as_array()
                                                .cloned()
                                                .unwrap_or_default();
                                            let functions: Vec<serde_json::Value> = tools_vec.iter().filter_map(|tool| {
                                            let func = tool.get("function")?;
                                            Some(serde_json::json!({
                                                "name": func.get("name")?,
                                                "description": func.get("description").unwrap_or(&serde_json::json!("")),
                                                "parameters": func.get("parameters").unwrap_or(&serde_json::json!({}))
                                            }))
                                        }).collect();

                                            let mut followup_req = serde_json::json!({
                                                "contents": gemini_contents,
                                                "generationConfig": {"maxOutputTokens": 4096},
                                                "tools": [{"functionDeclarations": functions}]
                                            });
                                            if let Some(sys) = request_body.get("systemInstruction")
                                            {
                                                followup_req["systemInstruction"] = sys.clone();
                                            }

                                            let mut req =
                                                client.post(&endpoint).json(&followup_req);
                                            for (key, value) in &headers {
                                                req = req.header(key, value);
                                            }

                                            match req.send().await {
                                                Ok(resp) if resp.status().is_success() => {
                                                    let resp_body =
                                                        resp.text().await.unwrap_or_default();
                                                    full_content = String::new();
                                                    gemini_tool_calls = Vec::new();

                                                    if let Ok(resp_json) =
                                                        serde_json::from_str::<serde_json::Value>(
                                                            &resp_body,
                                                        )
                                                    {
                                                        // Extract text
                                                        let resp_text: String = resp_json
                                                            .get("candidates")
                                                            .and_then(|c| c.get(0))
                                                            .and_then(|c| c.get("content"))
                                                            .and_then(|c| c.get("parts"))
                                                            .and_then(|p| p.as_array())
                                                            .map(|parts| {
                                                                parts
                                                                    .iter()
                                                                    .filter_map(|p| {
                                                                        p.get("text").and_then(
                                                                            |t| t.as_str(),
                                                                        )
                                                                    })
                                                                    .collect::<Vec<_>>()
                                                                    .join("")
                                                            })
                                                            .unwrap_or_default();

                                                        if !resp_text.is_empty() {
                                                            println!();
                                                            print!("{resp_text}");
                                                            std::io::stdout().flush().ok();
                                                            full_content = resp_text;
                                                        }

                                                        // Extract new tool calls
                                                        gemini_tool_calls = resp_json
                                                        .get("candidates").and_then(|c| c.get(0))
                                                        .and_then(|c| c.get("content"))
                                                        .and_then(|c| c.get("parts"))
                                                        .and_then(|p| p.as_array())
                                                        .map(|parts| {
                                                            parts.iter().filter_map(|p| {
                                                                let fc = p.get("functionCall")?;
                                                                let name = fc.get("name")?.as_str()?.to_string();
                                                                let args = fc.get("args").map_or_else(|| "{}".to_string(), |a| serde_json::to_string(a).unwrap_or_default());
                                                                Some(tools::ToolCall {
                                                                    id: format!("call_{}", uuid::Uuid::new_v4()),
                                                                    call_type: "function".to_string(),
                                                                    function: tools::FunctionCall { name, arguments: args },
                                                                })
                                                            }).collect()
                                                        }).unwrap_or_default();
                                                        // Loop continues — will check gemini_tool_calls at top
                                                    } else {
                                                        eprintln!("\nFailed to parse Gemini follow-up response");
                                                        break;
                                                    }
                                                }
                                                Ok(resp) => {
                                                    let status = resp.status();
                                                    let err_body =
                                                        resp.text().await.unwrap_or_default();
                                                    eprintln!(
                                                        "\nGemini follow-up failed: {status} {err_body}"
                                                    );
                                                    break;
                                                }
                                                Err(e) => {
                                                    eprintln!("\nGemini follow-up error: {e}");
                                                    break;
                                                }
                                            }
                                        } // end Gemini tool loop

                                        // Save final assistant message
                                        if !full_content.trim().is_empty() {
                                            chat_session.messages.push(serde_json::json!({
                                                "role": "assistant",
                                                "content": full_content.trim()
                                            }));
                                            chat_session.touch();
                                            if let Err(e) = save_chat_session(&chat_session) {
                                                tracing::warn!("Failed to save session: {}", e);
                                            }
                                        }

                                        // VDD: Run adversarial review if enabled
                                        if let Some(ref engine) = vdd_engine {
                                            let user_task = chat_session
                                                .messages
                                                .iter()
                                                .rev()
                                                .find(|m| {
                                                    m.get("role").and_then(|r| r.as_str())
                                                        == Some("user")
                                                })
                                                .and_then(|m| {
                                                    m.get("content").and_then(|c| c.as_str())
                                                })
                                                .unwrap_or("");

                                            match engine
                                                .review_text(
                                                    &full_content,
                                                    user_task,
                                                    &config.proxy.target,
                                                    api_key.as_ref(),
                                                )
                                                .await
                                            {
                                                Ok(result) => {
                                                    if result.findings.is_empty() {
                                                        println!("\n\x1b[32m✓ VDD Review: No issues found\x1b[0m");
                                                    } else {
                                                        let genuine_count = result
                                                            .findings
                                                            .iter()
                                                            .filter(|f| {
                                                                f.status
                                                                    == vdd::FindingStatus::Genuine
                                                            })
                                                            .count();
                                                        println!("\n\x1b[33m🔍 VDD Review: {} finding(s) ({} genuine)\x1b[0m",
                                                        result.findings.len(), genuine_count);
                                                        for finding in &result.findings {
                                                            let status_icon = match finding.status {
                                                            vdd::FindingStatus::Genuine => "⚠",
                                                            vdd::FindingStatus::FalsePositive => "✗",
                                                            vdd::FindingStatus::Disputed => "?",
                                                        };
                                                            println!(
                                                                "  {} [{}] {}",
                                                                status_icon,
                                                                finding.severity,
                                                                finding.description
                                                            );
                                                        }
                                                        if !result.context_injection.is_empty() {
                                                            chat_session.messages.push(serde_json::json!({
                                                            "role": "system",
                                                            "content": format!("<vdd-review>\n{}\n</vdd-review>", result.context_injection)
                                                        }));
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::warn!("VDD review failed: {}", e);
                                                    println!(
                                                        "\n\x1b[31m⚠ VDD review failed: {e}\x1b[0m"
                                                    );
                                                }
                                            }
                                        }

                                        // Update status bar
                                        let tokens = estimate_session_tokens(&chat_session)
                                            + full_content.len() / 4;
                                        let cost = session::calculate_cost(
                                            &model,
                                            &openclaudia::session::TokenUsage {
                                                input_tokens: input_tokens.max(tokens as u64),
                                                output_tokens: output_tokens
                                                    .max(full_content.len() as u64 / 4),
                                                cache_read_tokens: 0,
                                                cache_write_tokens: 0,
                                            },
                                        );
                                        let duration = chrono::Utc::now()
                                            .signed_duration_since(chat_session.created_at);
                                        let dur_str = format!("{}m", duration.num_minutes());
                                        tui::draw_status_bar(
                                            &model,
                                            tokens,
                                            cost,
                                            chat_session.mode.display(),
                                            &dur_str,
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!("\nFailed to parse Gemini response: {e}");
                                        eprintln!("Raw body: {}", &body[..body.len().min(500)]);
                                        let _ = save_chat_session(&chat_session);
                                        chat_session.messages.pop(); // Remove failed user message
                                    }
                                }

                                println!();
                            } else {
                                // Stream the response (Anthropic SSE / OpenAI SSE)
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
                                let mut anthropic_accumulator =
                                    tools::AnthropicToolAccumulator::new();

                                // Thinking display state
                                let mut in_thinking_block = false;
                                let mut thinking_start_time: Option<std::time::Instant> = None;

                                // Streaming markdown renderer
                                let mut md_renderer = tui::StreamingMarkdownRenderer::new();

                                // SSE usage accumulator
                                let mut stream_usage = openclaudia::session::TokenUsage::default();

                                // Stream timeout tracking
                                let mut last_data_time = std::time::Instant::now();
                                let stream_timeout =
                                    std::time::Duration::from_secs(proxy::SSE_STREAM_TIMEOUT_SECS);

                                // Audit: log model request
                                audit_logger.log(
                                    "model_request",
                                    &serde_json::json!({
                                        "model": &model,
                                        "provider": &config.proxy.target,
                                    }),
                                );

                                while let Some(chunk_result) = stream.next().await {
                                    // Check stream timeout
                                    if last_data_time.elapsed() > stream_timeout {
                                        eprintln!(
                                            "\nStream timeout: no data received for {}s",
                                            proxy::SSE_STREAM_TIMEOUT_SECS
                                        );
                                        // Preserve any partial content accumulated before timeout
                                        if !full_content.is_empty() {
                                            tracing::warn!(
                                                content_len = full_content.len(),
                                                "Stream timed out with partial content; preserving {} bytes",
                                                full_content.len()
                                            );
                                            full_content.push_str(
                                                "\n\n[Response truncated: stream timeout]",
                                            );
                                        }
                                        break;
                                    }

                                    // Check for configured keybindings during streaming
                                    if event::poll(std::time::Duration::from_millis(1))
                                        .unwrap_or(false)
                                    {
                                        if let Ok(Event::Key(key_event)) = event::read() {
                                            if key_event.kind == KeyEventKind::Press {
                                                // Convert key event to binding string and look up action
                                                if let Some(key_str) =
                                                    key_event_to_string(&key_event, false)
                                                {
                                                    if config.keybindings.is_bound(&key_str) {
                                                        let action = config
                                                            .keybindings
                                                            .get_action_or_default(&key_str);
                                                        // Cancel immediately stops streaming
                                                        if action == config::KeyAction::Cancel {
                                                            cancelled = true;
                                                            print!(" (cancelled)");
                                                            std::io::stdout().flush().ok();
                                                            break;
                                                        }
                                                        // Other actions queued for after streaming completes
                                                        if let Some(result) =
                                                            execute_key_action(&action)
                                                        {
                                                            pending_action = Some(result);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    match chunk_result {
                                        Ok(chunk) => {
                                            last_data_time = std::time::Instant::now();
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
                                                    if let Ok(json) =
                                                        serde_json::from_str::<serde_json::Value>(
                                                            data,
                                                        )
                                                    {
                                                        // Extract SSE usage from streaming events
                                                        if let Some(usage) =
                                                            proxy::extract_usage_from_sse_event(
                                                                &json,
                                                            )
                                                        {
                                                            stream_usage.accumulate(&usage);
                                                        }

                                                        // Thinking block detection (Anthropic)
                                                        if let Some(event_type) = json
                                                            .get("type")
                                                            .and_then(|t| t.as_str())
                                                        {
                                                            if event_type == "content_block_start" {
                                                                if let Some(block_type) = json
                                                                    .get("content_block")
                                                                    .and_then(|b| b.get("type"))
                                                                    .and_then(|t| t.as_str())
                                                                {
                                                                    if block_type == "thinking" {
                                                                        in_thinking_block = true;
                                                                        thinking_start_time = Some(
                                                                            std::time::Instant::now(
                                                                            ),
                                                                        );
                                                                        tui::print_thinking_start();
                                                                        continue;
                                                                    }
                                                                }
                                                            }
                                                            if event_type == "content_block_stop"
                                                                && in_thinking_block
                                                            {
                                                                let elapsed = thinking_start_time
                                                                    .map_or(0.0, |t| {
                                                                        t.elapsed().as_secs_f64()
                                                                    });
                                                                tui::print_thinking_end(elapsed);
                                                                in_thinking_block = false;
                                                                thinking_start_time = None;
                                                                continue;
                                                            }
                                                            if event_type == "content_block_delta"
                                                                && in_thinking_block
                                                            {
                                                                if let Some(text) = json
                                                                    .get("delta")
                                                                    .and_then(|d| d.get("thinking"))
                                                                    .and_then(|t| t.as_str())
                                                                {
                                                                    tui::print_thinking_chunk(text);
                                                                } else if let Some(text) = json
                                                                    .get("delta")
                                                                    .and_then(|d| d.get("text"))
                                                                    .and_then(|t| t.as_str())
                                                                {
                                                                    tui::print_thinking_chunk(text);
                                                                }
                                                                continue;
                                                            }
                                                        }

                                                        // Anthropic format: process all streaming events
                                                        // through the accumulator (handles text_delta,
                                                        // tool_use blocks, and stop_reason).
                                                        if let Some(text) = anthropic_accumulator
                                                            .process_event(&json)
                                                        {
                                                            md_renderer.push(&text);
                                                            full_content.push_str(&text);
                                                        }
                                                        // OpenAI format: choices[0].delta.content
                                                        else if let Some(delta) = json
                                                            .get("choices")
                                                            .and_then(|c| c.get(0))
                                                            .and_then(|c| c.get("delta"))
                                                        {
                                                            // Handle text content
                                                            if let Some(content) = delta
                                                                .get("content")
                                                                .and_then(|c| c.as_str())
                                                            {
                                                                md_renderer.push(content);
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
                                            eprintln!("\nStream error: {e}");
                                            break;
                                        }
                                    }
                                }

                                // Flush any remaining buffered markdown
                                md_renderer.flush();
                                println!();

                                // Audit: log model response
                                audit_logger.log(
                                    "model_response",
                                    &serde_json::json!({
                                        "model": &model,
                                        "content_length": full_content.len(),
                                        "cancelled": cancelled,
                                        "stream_usage": {
                                            "input_tokens": stream_usage.input_tokens,
                                            "output_tokens": stream_usage.output_tokens,
                                        },
                                    }),
                                );

                                // Update status bar after streaming completes
                                {
                                    let tokens = estimate_session_tokens(&chat_session)
                                        + full_content.len() / 4;
                                    let cost = session::calculate_cost(
                                        &model,
                                        &openclaudia::session::TokenUsage {
                                            input_tokens: tokens as u64,
                                            output_tokens: stream_usage
                                                .output_tokens
                                                .max(full_content.len() as u64 / 4),
                                            cache_read_tokens: stream_usage.cache_read_tokens,
                                            cache_write_tokens: stream_usage.cache_write_tokens,
                                        },
                                    );
                                    let duration = chrono::Utc::now()
                                        .signed_duration_since(chat_session.created_at);
                                    let dur_str = format!("{}m", duration.num_minutes());
                                    tui::draw_status_bar(
                                        &model,
                                        tokens,
                                        cost,
                                        chat_session.mode.display(),
                                        &dur_str,
                                    );
                                }

                                // If cancelled, append note to content
                                if cancelled && !full_content.is_empty() {
                                    full_content.push_str("\n\n[Response interrupted by user]");
                                }

                                // TOOL INTERCEPTION
                                // When tools are included in the API request, the model returns
                                // structured tool_use content blocks. If that fails, fall back to
                                // XML-style tool interception from text output.
                                if config.proxy.target == "anthropic" && !cancelled {
                                    let mut handled_structured = false;

                                    // STRUCTURED TOOL_USE PATH
                                    // The model returned tool_use content blocks with
                                    // stop_reason: "tool_use" — execute tools and loop.
                                    if anthropic_accumulator.has_tool_use() {
                                        handled_structured = true;
                                        let max_proxy_iterations = config.session.max_turns;
                                        let mut proxy_iteration: u32 = 0;
                                        let mut executed_tool_sigs: std::collections::HashSet<
                                            String,
                                        > = std::collections::HashSet::new();

                                        loop {
                                            if !anthropic_accumulator.has_tool_use() {
                                                break;
                                            }
                                            if max_proxy_iterations > 0
                                                && proxy_iteration >= max_proxy_iterations
                                            {
                                                eprintln!(
                                                "\n\x1b[33m⚠ Reached max_turns limit ({max_proxy_iterations} turns). Configure session.max_turns in config.yaml (0 = unlimited).\x1b[0m"
                                            );
                                                break;
                                            }
                                            proxy_iteration += 1;

                                            // Reset per-turn blast radius tracking
                                            guardrails::reset_turn();

                                            let text = anthropic_accumulator.get_text();
                                            let tool_calls =
                                                anthropic_accumulator.finalize_tool_calls();
                                            let tool_calls_json =
                                                anthropic_accumulator.to_openai_tool_calls_json();

                                            // Duplicate tool call detection
                                            if proxy_iteration > 0 {
                                                let mut all_dups = true;
                                                for tc in &tool_calls {
                                                    let sig = format!(
                                                        "{}:{}",
                                                        tc.function.name, tc.function.arguments
                                                    );
                                                    if !executed_tool_sigs.contains(&sig) {
                                                        all_dups = false;
                                                    }
                                                }
                                                if all_dups && !tool_calls.is_empty() {
                                                    eprintln!("\n\x1b[33m⚠ Detected duplicate tool calls - breaking agentic loop\x1b[0m");
                                                    break;
                                                }
                                            }
                                            for tc in &tool_calls {
                                                let sig = format!(
                                                    "{}:{}",
                                                    tc.function.name, tc.function.arguments
                                                );
                                                executed_tool_sigs.insert(sig);
                                            }

                                            // Store assistant message with tool_calls in OpenAI format.
                                            // convert_messages_to_anthropic handles back-conversion
                                            // to tool_use blocks for the API.
                                            chat_session.messages.push(serde_json::json!({
                                                "role": "assistant",
                                                "content": serde_json::Value::String(text.clone()),
                                                "tool_calls": tool_calls_json
                                            }));

                                            // Execute each tool locally
                                            for tool_call in &tool_calls {
                                                // Check plan mode restrictions before executing
                                                if let Some(block_msg) = check_plan_mode_restriction(
                                                    &chat_session,
                                                    &tool_call.function.name,
                                                    &tool_call.function.arguments,
                                                ) {
                                                    println!(
                                                    "\n\x1b[33m⚠ Blocked in plan mode: {}\x1b[0m",
                                                    tool_call.function.name
                                                );
                                                    chat_session.messages.push(serde_json::json!({
                                                        "role": "tool",
                                                        "tool_call_id": tool_call.id,
                                                        "content": format!("[ERROR] {}", block_msg),
                                                        "is_error": true
                                                    }));
                                                    continue;
                                                }

                                                // Permission check
                                                let tool_args_val2: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
                                                    .unwrap_or_else(|e| { tracing::warn!("Malformed tool arguments for '{}': {}", tool_call.function.name, e); serde_json::Value::Object(Default::default()) });
                                                match check_tool_permission_interactive(
                                                    &tool_call.function.name,
                                                    &tool_args_val2,
                                                    dangerously_skip_permissions,
                                                    &mut always_allowed_tools,
                                                ) {
                                                    ToolPermissionResult::Denied(msg) => {
                                                        chat_session.messages.push(serde_json::json!({
                                                            "role": "tool",
                                                            "tool_call_id": tool_call.id,
                                                            "content": format!("[ERROR] {}", msg),
                                                            "is_error": true
                                                        }));
                                                        continue;
                                                    }
                                                    ToolPermissionResult::Allowed => {}
                                                }

                                                println!(
                                                    "\n\x1b[36m⚡ Running {}...\x1b[0m",
                                                    tool_call.function.name
                                                );

                                                // Audit: log tool call
                                                audit_logger.log(
                                                    "tool_call",
                                                    &serde_json::json!({
                                                        "name": &tool_call.function.name,
                                                        "arguments": &tool_call.function.arguments,
                                                        "id": &tool_call.id,
                                                    }),
                                                );

                                                // Library-layer gate: see note at the first
                                                // call site above. Closes crosslink #505.
                                                let result = if let Some(ref db) = memory_db {
                                                    tools::execute_tool_with_memory(
                                                        tool_call,
                                                        Some(db),
                                                        Some(&permission_mgr),
                                                    )
                                                } else {
                                                    tools::execute_tool_with_memory(
                                                        tool_call,
                                                        None,
                                                        Some(&permission_mgr),
                                                    )
                                                };

                                                // Auto-learn from tool result
                                                if let Some(ref mut learner) = auto_learner {
                                                    let args: serde_json::Value =
                                                        serde_json::from_str(
                                                            &tool_call.function.arguments,
                                                        )
                                                        .unwrap_or_default();
                                                    if result.is_error {
                                                        learner.on_tool_failure(
                                                            &tool_call.function.name,
                                                            &args,
                                                            &result.content,
                                                        );
                                                    } else {
                                                        learner.on_tool_success(
                                                            &tool_call.function.name,
                                                            &args,
                                                            &result.content,
                                                        );
                                                    }
                                                }

                                                // Check for special markers (user_question, plan mode)
                                                let (final_content, _was_marker) =
                                                    process_tool_result_marker(
                                                        &mut chat_session,
                                                        &tool_call.function.name,
                                                        &result.content,
                                                    );
                                                let final_is_error = if _was_marker {
                                                    false
                                                } else {
                                                    result.is_error
                                                };

                                                // Audit: log tool result
                                                audit_logger.log(
                                                    "tool_result",
                                                    &serde_json::json!({
                                                        "name": &tool_call.function.name,
                                                        "id": &tool_call.id,
                                                        "is_error": final_is_error,
                                                        "content_length": final_content.len(),
                                                    }),
                                                );

                                                // Show result preview
                                                cli::display::tool_result::display_tool_result(
                                                    &tool_call.function.name,
                                                    &final_content,
                                                    final_is_error,
                                                );

                                                // Store tool result with error flag
                                                let result_content = if final_is_error {
                                                    format!("[ERROR] {final_content}")
                                                } else {
                                                    final_content
                                                };
                                                chat_session.messages.push(serde_json::json!({
                                                    "role": "tool",
                                                    "tool_call_id": result.tool_call_id,
                                                    "content": result_content,
                                                    "is_error": final_is_error
                                                }));
                                            }

                                            // Run quality gates after tool execution (if configured for every_turn)
                                            let qg_results = guardrails::run_quality_gates();
                                            for qg in &qg_results {
                                                if qg.passed {
                                                    tracing::debug!(name = %qg.name, "Quality gate passed");
                                                } else {
                                                    let severity = if qg.required {
                                                        "FAILED"
                                                    } else {
                                                        "warning"
                                                    };
                                                    eprintln!(
                                                    "\x1b[33m⚠ Quality gate '{}' {} (exit {})\x1b[0m",
                                                    qg.name, severity, qg.exit_code
                                                );
                                                    if !qg.stderr.is_empty() {
                                                        let preview: String = qg
                                                            .stderr
                                                            .lines()
                                                            .take(3)
                                                            .collect::<Vec<_>>()
                                                            .join("\n");
                                                        eprintln!("  {preview}");
                                                    }
                                                    // Inject findings into context so model can address them
                                                    chat_session.messages.push(serde_json::json!({
                                                    "role": "system",
                                                    "content": format!(
                                                        "[Quality Gate '{}' {}] exit code {}\nstdout: {}\nstderr: {}",
                                                        qg.name, severity,
                                                        qg.exit_code,
                                                        if qg.stdout.len() > 500 { safe_truncate(&qg.stdout, 500) } else { &qg.stdout },
                                                        if qg.stderr.len() > 500 { safe_truncate(&qg.stderr, 500) } else { &qg.stderr }
                                                    )
                                                }));
                                                }
                                            }

                                            // Clear accumulator for next response
                                            anthropic_accumulator.clear();

                                            // Send follow-up request WITH tool definitions
                                            println!(
                                            "\n\x1b[90m(Sending {} tool result{} to Claude...)\x1b[0m",
                                            tool_calls.len(),
                                            if tool_calls.len() == 1 { "" } else { "s" }
                                        );

                                            let anthropic_messages = convert_messages_to_anthropic(
                                                &chat_session.messages,
                                            );

                                            let openai_tools =
                                                tools::get_all_tool_definitions(true);
                                            let anthropic_tools = convert_tools_to_anthropic(
                                                openai_tools.as_array().unwrap_or(&vec![]),
                                            );

                                            let mut followup_req = serde_json::json!({
                                                "model": model,
                                                "messages": anthropic_messages,
                                                "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
                                                "stream": true,
                                                "tools": anthropic_tools
                                            });
                                            // Reuse multi-block system prompt from the initial
                                            // turn — the stable prefix stays cached across the
                                            // entire agentic tool loop.
                                            followup_req["system"] =
                                                openclaudia::providers::build_system_blocks(
                                                    &prompt_blocks,
                                                );
                                            if claude_code_token.is_some() {
                                                openclaudia::claude_credentials::inject_system_prompt(&mut followup_req);
                                            }

                                            let mut req =
                                                client.post(&endpoint).json(&followup_req);
                                            for (key, value) in &headers {
                                                req = req.header(key, value);
                                            }

                                            match req.send().await {
                                                Ok(response) if response.status().is_success() => {
                                                    use futures::StreamExt;
                                                    let mut stream = response.bytes_stream();
                                                    let mut buffer = String::new();
                                                    full_content = String::new();

                                                    println!();

                                                    while let Some(chunk_result) =
                                                        stream.next().await
                                                    {
                                                        match chunk_result {
                                                            Ok(chunk) => {
                                                                buffer.push_str(
                                                                    &String::from_utf8_lossy(
                                                                        &chunk,
                                                                    ),
                                                                );
                                                                while let Some(line_end) =
                                                                    buffer.find('\n')
                                                                {
                                                                    let line = buffer[..line_end]
                                                                        .trim()
                                                                        .to_string();
                                                                    buffer = buffer[line_end + 1..]
                                                                        .to_string();
                                                                    if line.is_empty()
                                                                        || line.starts_with(':')
                                                                    {
                                                                        continue;
                                                                    }
                                                                    if let Some(data) =
                                                                        line.strip_prefix("data: ")
                                                                    {
                                                                        if data == "[DONE]" {
                                                                            break;
                                                                        }
                                                                        if let Ok(json) =
                                                                            serde_json::from_str::<
                                                                                serde_json::Value,
                                                                            >(
                                                                                data
                                                                            )
                                                                        {
                                                                            if let Some(text) =
                                                                            anthropic_accumulator
                                                                                .process_event(
                                                                                    &json,
                                                                                )
                                                                        {
                                                                            print!("{text}");
                                                                            std::io::stdout()
                                                                                .flush()
                                                                                .ok();
                                                                            full_content
                                                                                .push_str(&text);
                                                                        }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                eprintln!("\nStream error: {e}");
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    // Loop continues — will check
                                                    // has_tool_use() at top
                                                }
                                                Ok(response) => {
                                                    eprintln!(
                                                        "\nFollow-up request failed: {}",
                                                        response.status()
                                                    );
                                                    break;
                                                }
                                                Err(e) => {
                                                    eprintln!("\nFollow-up request error: {e}");
                                                    break;
                                                }
                                            }
                                        }

                                        // Add final assistant message
                                        if !full_content.trim().is_empty() {
                                            chat_session.messages.push(serde_json::json!({
                                                "role": "assistant",
                                                "content": full_content.trim()
                                            }));
                                        }
                                    }

                                    // TEXT-BASED XML TOOL INTERCEPTION (fallback)
                                    // If the model returned text with XML tool calls instead of
                                    // structured tool_use blocks, fall back to text interception.
                                    if !handled_structured {
                                        let mut tool_interceptor =
                                            tool_intercept::ToolInterceptor::new();
                                        tool_interceptor.push(&full_content);

                                        // Agentic loop for proxy mode with local tool execution
                                        // 0 = unlimited (matches Claude Code behavior)
                                        let max_proxy_iterations = config.session.max_turns;
                                        let mut proxy_iteration: u32 = 0;

                                        // Track executed tool calls to detect loops
                                        let mut executed_tool_signatures: std::collections::HashSet<
                                        String,
                                    > = std::collections::HashSet::new();

                                        while tool_interceptor.has_complete_block()
                                            && (max_proxy_iterations == 0
                                                || proxy_iteration < max_proxy_iterations)
                                        {
                                            proxy_iteration += 1;

                                            // Extract ALL tool calls at once, stripping hallucinated
                                            // <function_results> blocks the model generated inline.
                                            // Without this, the model generates 8+ tool calls with
                                            // fabricated results in a single response, but only one
                                            // would execute per turn.
                                            let (all_tools, text_parts) =
                                                tool_interceptor.extract_all_tool_calls();

                                            if all_tools.is_empty() {
                                                break;
                                            }

                                            // Check for duplicate tool calls (model stuck in loop)
                                            let mut all_duplicates = true;
                                            for tool in &all_tools {
                                                // Create a signature from tool name and parameters
                                                let params_str: String = tool
                                                    .parameters
                                                    .iter()
                                                    .map(|(k, v)| format!("{k}={v}"))
                                                    .collect::<Vec<_>>()
                                                    .join(",");
                                                let sig = format!("{}:{}", tool.name, params_str);
                                                if executed_tool_signatures.insert(sig) {
                                                    all_duplicates = false;
                                                }
                                            }

                                            if all_duplicates && proxy_iteration > 1 {
                                                eprintln!(
                                                "\n\x1b[33m⚠ Detected duplicate tool calls - breaking loop\x1b[0m"
                                            );
                                                break;
                                            }

                                            // Add assistant message with text content between tools
                                            let combined_text = text_parts.join("\n\n");
                                            if !combined_text.is_empty() {
                                                chat_session.messages.push(serde_json::json!({
                                                    "role": "assistant",
                                                    "content": combined_text
                                                }));
                                            }

                                            // Filter out tools blocked by plan mode
                                            let all_tools: Vec<_> = all_tools
                                                .into_iter()
                                                .filter(|tool| {
                                                    let args_json = serde_json::to_string(&tool.parameters
                                                        .iter()
                                                        .collect::<std::collections::HashMap<_, _>>())
                                                        .unwrap_or_default();
                                                    if let Some(block_msg) = check_plan_mode_restriction(
                                                        &chat_session,
                                                        &tool.name,
                                                        &args_json,
                                                    ) {
                                                        println!(
                                                            "\n\x1b[33m⚠ Blocked in plan mode: {}\x1b[0m",
                                                            tool.name
                                                        );
                                                        // Add error result to messages
                                                        chat_session.messages.push(serde_json::json!({
                                                            "role": "user",
                                                            "content": format!("[ERROR] {}", block_msg)
                                                        }));
                                                        false
                                                    } else {
                                                        true
                                                    }
                                                })
                                                .collect();

                                            // Execute ALL tools locally with the library-layer
                                            // permission gate engaged. Closes crosslink #505 for
                                            // the intercept-mode path.
                                            let results = tool_intercept::execute_intercepted_tools(
                                                &all_tools,
                                                memory_db.as_ref(),
                                                Some(&permission_mgr),
                                            );

                                            // Format ALL results for Claude and add as user message
                                            // Uses the new format with tool names for better completion signaling
                                            let results_xml =
                                                tool_intercept::format_execution_results_xml(
                                                    &results,
                                                );
                                            chat_session.messages.push(serde_json::json!({
                                                "role": "user",
                                                "content": results_xml
                                            }));

                                            // Send follow-up request
                                            println!(
                                        "\n\x1b[90m(Sending {} tool result{} to Claude...)\x1b[0m",
                                        results.len(),
                                        if results.len() == 1 { "" } else { "s" }
                                    );

                                            let anthropic_messages = convert_messages_to_anthropic(
                                                &chat_session.messages,
                                            );

                                            // Proxy mode: omit tools from follow-up requests
                                            let mut followup_req = serde_json::json!({
                                                "model": model,
                                                "messages": anthropic_messages,
                                                "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
                                                "stream": true
                                            });
                                            // Reuse multi-block system prompt — stable prefix
                                            // stays cached across the proxy tool loop.
                                            followup_req["system"] =
                                                openclaudia::providers::build_system_blocks(
                                                    &prompt_blocks,
                                                );
                                            if claude_code_token.is_some() {
                                                openclaudia::claude_credentials::inject_system_prompt(&mut followup_req);
                                            }

                                            let mut req =
                                                client.post(&endpoint).json(&followup_req);
                                            for (key, value) in &headers {
                                                req = req.header(key, value);
                                            }

                                            match req.send().await {
                                                Ok(response) if response.status().is_success() => {
                                                    use futures::StreamExt;

                                                    let mut stream = response.bytes_stream();
                                                    let mut buffer = String::new();
                                                    let mut followup_content = String::new();

                                                    while let Some(chunk_result) =
                                                        stream.next().await
                                                    {
                                                        match chunk_result {
                                                            Ok(chunk) => {
                                                                buffer.push_str(
                                                                    &String::from_utf8_lossy(
                                                                        &chunk,
                                                                    ),
                                                                );
                                                                while let Some(line_end) =
                                                                    buffer.find('\n')
                                                                {
                                                                    let line = buffer[..line_end]
                                                                        .trim()
                                                                        .to_string();
                                                                    buffer = buffer[line_end + 1..]
                                                                        .to_string();
                                                                    if line.is_empty()
                                                                        || line.starts_with(':')
                                                                    {
                                                                        continue;
                                                                    }
                                                                    if let Some(data) =
                                                                        line.strip_prefix("data: ")
                                                                    {
                                                                        if data == "[DONE]" {
                                                                            break;
                                                                        }
                                                                        if let Ok(json) =
                                                                            serde_json::from_str::<
                                                                                serde_json::Value,
                                                                            >(
                                                                                data
                                                                            )
                                                                        {
                                                                            if json
                                                                        .get("type")
                                                                        .and_then(|t| t.as_str())
                                                                        == Some(
                                                                            "content_block_delta",
                                                                        )
                                                                    {
                                                                        if let Some(text) = json
                                                                            .get("delta")
                                                                            .and_then(|d| {
                                                                                d.get("text")
                                                                            })
                                                                            .and_then(|t| {
                                                                                t.as_str()
                                                                            })
                                                                        {
                                                                            print!("{text}");
                                                                            std::io::stdout()
                                                                                .flush()
                                                                                .ok();
                                                                            followup_content
                                                                                .push_str(text);
                                                                        }
                                                                    }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => {
                                                                eprintln!("\nStream error: {e}");
                                                                break;
                                                            }
                                                        }
                                                    }

                                                    // Check if follow-up contains more tool calls
                                                    tool_interceptor.clear();
                                                    tool_interceptor.push(&followup_content);
                                                    full_content = followup_content;
                                                }
                                                Ok(response) => {
                                                    eprintln!(
                                                        "\nFollow-up request failed: {}",
                                                        response.status()
                                                    );
                                                    break;
                                                }
                                                Err(e) => {
                                                    eprintln!("\nFollow-up request error: {e}");
                                                    break;
                                                }
                                            }
                                        }

                                        // Log if we hit the max_turns limit while tools were still pending
                                        if max_proxy_iterations > 0
                                            && proxy_iteration >= max_proxy_iterations
                                            && tool_interceptor.has_complete_block()
                                        {
                                            eprintln!(
                                        "\n\x1b[33m⚠ Reached max_turns limit ({max_proxy_iterations} turns). Configure session.max_turns in config.yaml (0 = unlimited).\x1b[0m"
                                    );
                                        }

                                        // Add final assistant message
                                        if !full_content.trim().is_empty()
                                            && !tool_interceptor.has_pending_tool_calls()
                                        {
                                            chat_session.messages.push(serde_json::json!({
                                                "role": "assistant",
                                                "content": full_content.trim()
                                            }));
                                        }
                                    } // end if !handled_structured (XML fallback)

                                    // VDD: Run adversarial review if enabled
                                    if let Some(ref engine) = vdd_engine {
                                        // Extract the user's original task from the last user message
                                        let user_task = chat_session
                                            .messages
                                            .iter()
                                            .rev()
                                            .find(|m| {
                                                m.get("role").and_then(|r| r.as_str())
                                                    == Some("user")
                                            })
                                            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                                            .unwrap_or("");

                                        match engine
                                            .review_text(
                                                &full_content,
                                                user_task,
                                                &config.proxy.target,
                                                api_key.as_ref(),
                                            )
                                            .await
                                        {
                                            Ok(result) => {
                                                if result.findings.is_empty() {
                                                    println!("\n\x1b[32m✓ VDD Review: No issues found\x1b[0m");
                                                } else {
                                                    let genuine_count = result
                                                        .findings
                                                        .iter()
                                                        .filter(|f| {
                                                            f.status == vdd::FindingStatus::Genuine
                                                        })
                                                        .count();
                                                    println!(
                                                    "\n\x1b[33m🔍 VDD Review: {} finding(s) ({} genuine)\x1b[0m",
                                                    result.findings.len(),
                                                    genuine_count
                                                );
                                                    // Display findings
                                                    for finding in &result.findings {
                                                        let status_icon = match finding.status {
                                                            vdd::FindingStatus::Genuine => "⚠",
                                                            vdd::FindingStatus::FalsePositive => {
                                                                "✗"
                                                            }
                                                            vdd::FindingStatus::Disputed => "?",
                                                        };
                                                        println!(
                                                            "  {} [{}] {}",
                                                            status_icon,
                                                            finding.severity,
                                                            finding.description
                                                        );
                                                    }
                                                    // Inject findings as context for next turn (advisory mode)
                                                    if !result.context_injection.is_empty() {
                                                        chat_session.messages.push(serde_json::json!({
                                                        "role": "system",
                                                        "content": format!(
                                                            "<vdd-review>\n{}\n</vdd-review>",
                                                            result.context_injection
                                                        )
                                                    }));
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("VDD review failed: {}", e);
                                                println!(
                                                    "\n\x1b[31m⚠ VDD review failed: {e}\x1b[0m"
                                                );
                                            }
                                        }
                                    }

                                    println!();
                                    continue; // Skip the regular agentic loop since we handled proxy mode
                                }

                                // Agentic loop - continue while there are tool calls
                                // 0 = unlimited, default: 25
                                let max_iterations = config.session.max_turns;
                                let mut iteration: u32 = 0;
                                let mut current_content = full_content;
                                let mut executed_tool_sigs: std::collections::HashSet<String> =
                                    std::collections::HashSet::new();

                                while tool_accumulator.has_tool_calls()
                                    && !cancelled
                                    && (max_iterations == 0 || iteration < max_iterations)
                                {
                                    iteration += 1;

                                    // Reset per-turn blast radius tracking
                                    guardrails::reset_turn();

                                    // Get tool calls
                                    let tool_calls = tool_accumulator.finalize();

                                    // Duplicate tool call detection
                                    if iteration > 1 {
                                        let mut all_dups = true;
                                        for tc in &tool_calls {
                                            let sig = format!(
                                                "{}:{}",
                                                tc.function.name, tc.function.arguments
                                            );
                                            if !executed_tool_sigs.contains(&sig) {
                                                all_dups = false;
                                            }
                                        }
                                        if all_dups && !tool_calls.is_empty() {
                                            eprintln!("\n\x1b[33m⚠ Detected duplicate tool calls - breaking agentic loop\x1b[0m");
                                            break;
                                        }
                                    }
                                    for tc in &tool_calls {
                                        let sig = format!(
                                            "{}:{}",
                                            tc.function.name, tc.function.arguments
                                        );
                                        executed_tool_sigs.insert(sig);
                                    }

                                    // Add assistant message with tool calls
                                    let tool_calls_json: Vec<serde_json::Value> = tool_calls
                                        .iter()
                                        .map(|tc| {
                                            serde_json::json!({
                                                "id": tc.id,
                                                "type": tc.call_type,
                                                "function": {
                                                    "name": tc.function.name,
                                                    "arguments": tc.function.arguments
                                                }
                                            })
                                        })
                                        .collect();

                                    chat_session.messages.push(serde_json::json!({
                                    "role": "assistant",
                                    "content": serde_json::Value::String(current_content.clone()),
                                    "tool_calls": tool_calls_json
                                }));

                                    // Execute each tool and collect results
                                    for tool_call in &tool_calls {
                                        // Check plan mode restrictions before executing
                                        if let Some(block_msg) = check_plan_mode_restriction(
                                            &chat_session,
                                            &tool_call.function.name,
                                            &tool_call.function.arguments,
                                        ) {
                                            println!(
                                                "\n\x1b[33m⚠ Blocked in plan mode: {}\x1b[0m",
                                                tool_call.function.name
                                            );
                                            chat_session.messages.push(serde_json::json!({
                                                "role": "tool",
                                                "tool_call_id": tool_call.id,
                                                "content": format!("[ERROR] {}", block_msg),
                                                "is_error": true
                                            }));
                                            continue;
                                        }

                                        // Permission check
                                        let tool_args_val3: serde_json::Value =
                                            serde_json::from_str(&tool_call.function.arguments)
                                                .unwrap_or_else(|e| {
                                                    tracing::warn!(
                                                        "Malformed tool arguments for '{}': {}",
                                                        tool_call.function.name,
                                                        e
                                                    );
                                                    serde_json::Value::Object(Default::default())
                                                });
                                        match check_tool_permission_interactive(
                                            &tool_call.function.name,
                                            &tool_args_val3,
                                            dangerously_skip_permissions,
                                            &mut always_allowed_tools,
                                        ) {
                                            ToolPermissionResult::Denied(msg) => {
                                                chat_session.messages.push(serde_json::json!({
                                                    "role": "tool",
                                                    "tool_call_id": tool_call.id,
                                                    "content": format!("[ERROR] {}", msg),
                                                    "is_error": true
                                                }));
                                                continue;
                                            }
                                            ToolPermissionResult::Allowed => {}
                                        }

                                        println!(
                                            "\n\x1b[36m⚡ Running {}...\x1b[0m",
                                            tool_call.function.name
                                        );

                                        // Execute tool with library-layer permission gate
                                        // in addition to the interactive UX gate above.
                                        // Closes crosslink #505.
                                        let result = if let Some(ref db) = memory_db {
                                            tools::execute_tool_with_memory(
                                                tool_call,
                                                Some(db),
                                                Some(&permission_mgr),
                                            )
                                        } else {
                                            tools::execute_tool_with_memory(
                                                tool_call,
                                                None,
                                                Some(&permission_mgr),
                                            )
                                        };

                                        // Auto-learn from tool result
                                        if let Some(ref mut learner) = auto_learner {
                                            let args: serde_json::Value =
                                                serde_json::from_str(&tool_call.function.arguments)
                                                    .unwrap_or_default();
                                            if result.is_error {
                                                learner.on_tool_failure(
                                                    &tool_call.function.name,
                                                    &args,
                                                    &result.content,
                                                );
                                            } else {
                                                learner.on_tool_success(
                                                    &tool_call.function.name,
                                                    &args,
                                                    &result.content,
                                                );
                                            }
                                        }

                                        // Check for special markers (user_question, plan mode)
                                        let (final_content, _was_marker) =
                                            process_tool_result_marker(
                                                &mut chat_session,
                                                &tool_call.function.name,
                                                &result.content,
                                            );
                                        let final_is_error =
                                            if _was_marker { false } else { result.is_error };

                                        // Log activity for short-term memory
                                        if let Some(ref db) = memory_db {
                                            let activity_type =
                                                match tool_call.function.name.as_str() {
                                                    "read_file" => "file_read",
                                                    "write_file" => "file_write",
                                                    "edit_file" => "file_edit",
                                                    "bash" => "bash_command",
                                                    "chainlink" => {
                                                        // Parse chainlink subcommand
                                                        if let Ok(args) = serde_json::from_str::<
                                                            serde_json::Value,
                                                        >(
                                                            &tool_call.function.arguments,
                                                        ) {
                                                            if let Some(cmd) = args
                                                                .get("command")
                                                                .and_then(|v| v.as_str())
                                                            {
                                                                if cmd.starts_with("create") {
                                                                    "issue_created"
                                                                } else if cmd.starts_with("close") {
                                                                    "issue_closed"
                                                                } else if cmd.starts_with("comment")
                                                                {
                                                                    "issue_comment"
                                                                } else {
                                                                    "chainlink"
                                                                }
                                                            } else {
                                                                "chainlink"
                                                            }
                                                        } else {
                                                            "chainlink"
                                                        }
                                                    }
                                                    other => other,
                                                };

                                            // Extract target from args
                                            let target = if let Ok(args) =
                                                serde_json::from_str::<serde_json::Value>(
                                                    &tool_call.function.arguments,
                                                ) {
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
                                                if final_is_error { Some("error") } else { None },
                                            );
                                        }

                                        // Show result preview
                                        cli::display::tool_result::display_tool_result(
                                            &tool_call.function.name,
                                            &final_content,
                                            final_is_error,
                                        );

                                        // Add tool result with error flag
                                        let result_content = if final_is_error {
                                            format!("[ERROR] {final_content}")
                                        } else {
                                            final_content
                                        };
                                        chat_session.messages.push(serde_json::json!({
                                            "role": "tool",
                                            "tool_call_id": result.tool_call_id,
                                            "content": result_content,
                                            "is_error": final_is_error
                                        }));
                                    }

                                    // Run quality gates after tool execution (if configured for every_turn)
                                    let qg_results = guardrails::run_quality_gates();
                                    for qg in &qg_results {
                                        if qg.passed {
                                            tracing::debug!(name = %qg.name, "Quality gate passed");
                                        } else {
                                            let severity =
                                                if qg.required { "FAILED" } else { "warning" };
                                            eprintln!(
                                                "\x1b[33m⚠ Quality gate '{}' {} (exit {})\x1b[0m",
                                                qg.name, severity, qg.exit_code
                                            );
                                            if !qg.stderr.is_empty() {
                                                let preview: String = qg
                                                    .stderr
                                                    .lines()
                                                    .take(3)
                                                    .collect::<Vec<_>>()
                                                    .join("\n");
                                                eprintln!("  {preview}");
                                            }
                                            // Inject findings into context so model can address them
                                            chat_session.messages.push(serde_json::json!({
                                            "role": "system",
                                            "content": format!(
                                                "[Quality Gate '{}' {}] exit code {}\nstdout: {}\nstderr: {}",
                                                qg.name, severity,
                                                qg.exit_code,
                                                if qg.stdout.len() > 500 { safe_truncate(&qg.stdout, 500) } else { &qg.stdout },
                                                if qg.stderr.len() > 500 { safe_truncate(&qg.stderr, 500) } else { &qg.stderr }
                                            )
                                        }));
                                        }
                                    }

                                    // Clear accumulator for next iteration
                                    tool_accumulator.clear();

                                    // Continue the conversation - send tool results back to model
                                    println!("\n\x1b[90mContinuing with tool results...\x1b[0m\n");

                                    // Build new request with tool results
                                    let request_body = if config.proxy.target == "anthropic" {
                                        // Anthropic direct API - convert messages to Anthropic format
                                        let system_msg = chat_session
                                            .messages
                                            .iter()
                                            .find(|m| {
                                                m.get("role").and_then(|r| r.as_str())
                                                    == Some("system")
                                            })
                                            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                                            .map(String::from);

                                        // Convert messages with proper tool_use/tool_result handling
                                        let anthropic_messages =
                                            convert_messages_to_anthropic(&chat_session.messages);

                                        let openai_tools = tools::get_all_tool_definitions(true);
                                        let anthropic_tools = convert_tools_to_anthropic(
                                            openai_tools.as_array().unwrap_or(&vec![]),
                                        );

                                        let mut req = serde_json::json!({
                                            "model": model,
                                            "messages": anthropic_messages,
                                            "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
                                            "stream": true,
                                            "tools": anthropic_tools
                                        });

                                        if let Some(sys) = system_msg {
                                            req["system"] = serde_json::json!([{
                                                "type": "text",
                                                "text": sys,
                                                "cache_control": {"type": "ephemeral"}
                                            }]);
                                        }

                                        req
                                    } else {
                                        // OpenAI-compatible format for other providers
                                        serde_json::json!({
                                            "model": model,
                                            "messages": chat_session.messages,
                                            "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
                                            "stream": true,
                                            "tools": tools::get_all_tool_definitions(true)
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
                                                    buffer
                                                        .push_str(&String::from_utf8_lossy(&chunk));

                                                    while let Some(line_end) = buffer.find('\n') {
                                                        let line =
                                                            buffer[..line_end].trim().to_string();
                                                        buffer = buffer[line_end + 1..].to_string();

                                                        if line.is_empty() || line.starts_with(':')
                                                        {
                                                            continue;
                                                        }

                                                        if let Some(data) =
                                                            line.strip_prefix("data: ")
                                                        {
                                                            if data == "[DONE]" {
                                                                break;
                                                            }

                                                            if let Ok(json) = serde_json::from_str::<
                                                                serde_json::Value,
                                                            >(
                                                                data
                                                            ) {
                                                                // Anthropic format: content_block_delta
                                                                if json
                                                                    .get("type")
                                                                    .and_then(|t| t.as_str())
                                                                    == Some("content_block_delta")
                                                                {
                                                                    if let Some(text) = json
                                                                        .get("delta")
                                                                        .and_then(|d| d.get("text"))
                                                                        .and_then(|t| t.as_str())
                                                                    {
                                                                        print!("{text}");
                                                                        std::io::stdout()
                                                                            .flush()
                                                                            .ok();
                                                                        current_content
                                                                            .push_str(text);
                                                                    }
                                                                }
                                                                // OpenAI format: choices[0].delta.content
                                                                else if let Some(delta) = json
                                                                    .get("choices")
                                                                    .and_then(|c| c.get(0))
                                                                    .and_then(|c| c.get("delta"))
                                                                {
                                                                    // Handle text content
                                                                    if let Some(content) = delta
                                                                        .get("content")
                                                                        .and_then(|c| c.as_str())
                                                                    {
                                                                        print!("{content}");
                                                                        std::io::stdout()
                                                                            .flush()
                                                                            .ok();
                                                                        current_content
                                                                            .push_str(content);
                                                                    }
                                                                    // Accumulate tool calls for next iteration
                                                                    tool_accumulator
                                                                        .process_delta(delta);
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

                                // Log if we hit the max_turns limit while tools were still pending
                                if max_iterations > 0
                                    && iteration >= max_iterations
                                    && tool_accumulator.has_tool_calls()
                                {
                                    eprintln!(
                                    "\n\x1b[33m⚠ Reached max_turns limit ({max_iterations} turns). Configure session.max_turns in config.yaml (0 = unlimited).\x1b[0m"
                                );
                                }

                                // Save final response
                                if !current_content.is_empty() && !tool_accumulator.has_tool_calls()
                                {
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
                                } else if current_content.is_empty()
                                    && !tool_accumulator.has_tool_calls()
                                {
                                    // No content and no tool calls - save then remove
                                    let _ = save_chat_session(&chat_session);
                                    chat_session.messages.pop();
                                }

                                // VDD: Run adversarial review if enabled
                                if !cancelled {
                                    if let Some(ref engine) = vdd_engine {
                                        let vdd_content = &current_content;
                                        if !vdd_content.trim().is_empty() {
                                            let user_task = chat_session
                                                .messages
                                                .iter()
                                                .rev()
                                                .find(|m| {
                                                    m.get("role").and_then(|r| r.as_str())
                                                        == Some("user")
                                                })
                                                .and_then(|m| {
                                                    m.get("content").and_then(|c| c.as_str())
                                                })
                                                .unwrap_or("");

                                            match engine
                                                .review_text(
                                                    vdd_content,
                                                    user_task,
                                                    &config.proxy.target,
                                                    api_key.as_ref(),
                                                )
                                                .await
                                            {
                                                Ok(result) => {
                                                    if result.findings.is_empty() {
                                                        println!("\n\x1b[32m✓ VDD Review: No issues found\x1b[0m");
                                                    } else {
                                                        let genuine_count = result
                                                            .findings
                                                            .iter()
                                                            .filter(|f| {
                                                                f.status
                                                                    == vdd::FindingStatus::Genuine
                                                            })
                                                            .count();
                                                        println!("\n\x1b[33m🔍 VDD Review: {} finding(s) ({} genuine)\x1b[0m",
                                                        result.findings.len(), genuine_count);
                                                        for finding in &result.findings {
                                                            let status_icon = match finding.status {
                                                            vdd::FindingStatus::Genuine => "⚠",
                                                            vdd::FindingStatus::FalsePositive => "✗",
                                                            vdd::FindingStatus::Disputed => "?",
                                                        };
                                                            println!(
                                                                "  {} [{}] {}",
                                                                status_icon,
                                                                finding.severity,
                                                                finding.description
                                                            );
                                                        }
                                                        if !result.context_injection.is_empty() {
                                                            chat_session.messages.push(serde_json::json!({
                                                            "role": "system",
                                                            "content": format!("<vdd-review>\n{}\n</vdd-review>", result.context_injection)
                                                        }));
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::warn!("VDD review failed: {}", e);
                                                    println!(
                                                        "\n\x1b[31m⚠ VDD review failed: {e}\x1b[0m"
                                                    );
                                                }
                                            }
                                        }
                                    }
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
                                            println!(
                                                "\nSwitched to {} mode: {}\n",
                                                chat_session.mode.display(),
                                                chat_session.mode.description()
                                            );
                                        }
                                        SlashCommandResult::Status => {
                                            let tokens = estimate_session_tokens(&chat_session);
                                            let duration = chrono::Utc::now()
                                                .signed_duration_since(chat_session.created_at);
                                            println!(
                                                "\n[{}] {} | ~{} tokens | {} min\n",
                                                chat_session.mode.display(),
                                                chat_session.model,
                                                tokens,
                                                duration.num_minutes()
                                            );
                                        }
                                        SlashCommandResult::Export => {
                                            export_chat_session(&chat_session);
                                        }
                                        _ => {
                                            // Other actions print their own messages via execute_key_action
                                        }
                                    }
                                }
                            } // end else (non-Google streaming)
                        } else {
                            let status = response.status();
                            let content_type = response
                                .headers()
                                .get("content-type")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            let body = response.text().await.unwrap_or_default();
                            if content_type.contains("text/html") {
                                eprintln!("\nError {status}: (HTML response — check your provider configuration)\n");
                            } else {
                                eprintln!("\nError {status}: {body}\n");
                            }
                            // Save before removing the failed user message
                            let _ = save_chat_session(&chat_session);
                            chat_session.messages.pop();
                        }
                    }
                    Err(e) => {
                        spinner.finish_and_clear();
                        eprintln!("\nRequest failed: {e}\n");
                        let _ = save_chat_session(&chat_session);
                        chat_session.messages.pop();
                    }
                }

                // Autosave session after each response (protects against terminal close)
                save_session_to_short_term_memory(&chat_session, memory_db.as_ref());

                // Auto-compact check: warn at 85%, compact at 90% of context window
                if chat_session.messages.len() > 6 {
                    let est = estimate_session_tokens(&chat_session);
                    let (should_warn, should_compact, pct) =
                        openclaudia::compaction::check_context_budget(est, &model);
                    if should_compact {
                        eprintln!("\x1b[33m⚠ Context at {pct:.0}% — auto-compacting...\x1b[0m");
                        let (before, after) = compact_chat_session(&mut chat_session);
                        eprintln!("\x1b[32m✓ Compacted: {before} → {after} messages\x1b[0m");
                    } else if should_warn {
                        eprintln!(
                            "\x1b[33m⚠ Context at {pct:.0}% — use /compact to free space\x1b[0m"
                        );
                    }
                }
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
                eprintln!("Error: {err:?}");
                break;
            }
        }
    }

    finalize_chat(
        &mut auto_learner,
        &chat_session,
        memory_db.as_ref(),
        &mut rl,
        &history_path,
    );

    println!("\nGoodbye!");
    Ok(())
}

// ============================================================================
// Tests for cmd_chat helpers (crosslink #262 decomposition).
//
// These pure-function tests would have been impossible when the logic
// lived inline inside cmd_chat — the 3200-line function made unit
// testing of any slice impossible. Each extraction enables the test
// cases below.
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_prefers_explicit_override() {
        let got = resolve_model_name(
            Some("custom-model".to_string()),
            Some("provider-default".to_string()),
            "anthropic",
        );
        assert_eq!(got, "custom-model");
    }

    #[test]
    fn resolve_model_falls_back_to_provider_config() {
        let got = resolve_model_name(None, Some("provider-default".to_string()), "openai");
        assert_eq!(got, "provider-default");
    }

    #[test]
    fn resolve_model_per_target_defaults() {
        assert_eq!(resolve_model_name(None, None, "anthropic"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_name(None, None, "openai"), "gpt-5.2");
        assert_eq!(resolve_model_name(None, None, "google"), "gemini-2.5-flash");
        assert_eq!(resolve_model_name(None, None, "zai"), "glm-5");
        assert_eq!(resolve_model_name(None, None, "deepseek"), "deepseek-chat");
        assert_eq!(resolve_model_name(None, None, "qwen"), "qwen3.5-plus");
        // Unknown target falls back to the OpenAI default.
        assert_eq!(resolve_model_name(None, None, "unknown-provider"), "gpt-5.2");
    }

    #[test]
    fn parse_initial_mode_none_is_default() {
        let got = parse_initial_behavior_mode(None).expect("default always succeeds");
        let default = openclaudia::modes::BehaviorMode::default();
        // Compare via display name rather than relying on `Eq`.
        assert_eq!(got.display_name(), default.display_name());
    }

    #[test]
    fn parse_initial_mode_unknown_preset_returns_err() {
        let err = parse_initial_behavior_mode(Some("this-preset-does-not-exist"))
            .expect_err("unknown preset should fail");
        // The error string must be user-facing — cmd_chat prints it.
        assert!(!err.is_empty());
    }
}
