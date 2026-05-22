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
    config, guardrails, memory,
    permissions::PermissionManager,
    plugins, prompt,
    proxy::normalize_base_url,
    tools::{self},
    tui, vdd,
};

use clap::{builder::PossibleValuesParser, Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Re-import the extracted helpers still used by main.rs after the
// `cmd_chat` god-function decomposition (crosslink #262). The bulk
// of the REPL lives in `cli::chat_repl` now.
use cli::display::tips::get_random_tip;
use cli::repl::session_io::{
    compact_chat_session, estimate_session_tokens, save_session_to_short_term_memory,
};
use cli::repl::{get_history_path, list_chat_sessions, ChatSession};

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
    #[arg(
        long,
        value_name = "PRESET",
        value_parser = PossibleValuesParser::new(openclaudia::modes::SUPPORTED_PRESETS),
    )]
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

        /// Target provider (anthropic, openai, google, gemini, deepseek, qwen, alibaba, zai, glm, zhipu, ollama, local, lmstudio, localai, text-generation-webui)
        #[arg(
            short,
            long,
            value_parser = PossibleValuesParser::new(openclaudia::providers::SUPPORTED_PROVIDERS),
        )]
        target: Option<String>,
    },

    /// Show current configuration
    Config,

    /// Check configuration and connectivity
    Doctor,

    /// Start ACP server on stdin/stdout for agent interoperability (acpx)
    Acp {
        /// Target provider (overrides config)
        #[arg(
            short,
            long,
            value_parser = PossibleValuesParser::new(openclaudia::providers::SUPPORTED_PROVIDERS),
        )]
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

        /// Target provider (anthropic, openai, google, gemini, deepseek, qwen, alibaba, zai, glm, zhipu, ollama, local, lmstudio, localai, text-generation-webui)
        #[arg(
            short,
            long,
            value_parser = PossibleValuesParser::new(openclaudia::providers::SUPPORTED_PROVIDERS),
        )]
        target: Option<String>,
    },
}

// OpenClaudia is a single-user CLI; a current-thread runtime is sufficient
// and keeps all futures on one thread, which is required by the `onig`-backed
// StreamingMarkdownRenderer (holds `*mut` raw pointers that are not Send).
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging. The full-screen ratatui TUI owns the terminal, so
    // writing log lines to stderr would smear them across the rendered frame.
    // In that mode we redirect tracing to a per-run log file under
    // .openclaudia/logs/; everywhere else we keep the stderr writer.
    let filter = if cli.verbose {
        "openclaudia=debug,tower_http=debug"
    } else {
        "openclaudia=info,tower_http=warn"
    };

    let tui_mode_active = cli.command.is_none() && !cli.tui_mode;
    let log_writer: tracing_subscriber::fmt::writer::BoxMakeWriter = if tui_mode_active {
        let dir = std::path::Path::new(".openclaudia/logs");
        let file = std::fs::create_dir_all(dir).ok().and_then(|()| {
            let path = dir.join(format!("tui-{}.log", std::process::id()));
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        });
        file.map_or_else(
            || tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::sink),
            |f| tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::sync::Mutex::new(f)),
        )
    } else {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr)
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(!tui_mode_active)
                .with_writer(log_writer),
        )
        .init();

    // Run on-disk schema migrations before any subsystem touches the
    // stores they manage. Failures never abort startup — the runner
    // logs each and continues.
    let _ =
        openclaudia::migrations::run_all(&openclaudia::migrations::MigrationContext::from_env());

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
    // Crosslink #797: every configuration-load / provider-resolve /
    // auth-resolve failure path used to print to stderr and return
    // `Ok(())`, giving exit code 0 even on a broken setup. `set -e`
    // wrappers and orchestration that branches on exit status saw success
    // and continued. Each failure now propagates as `anyhow::Error` so
    // main() exits non-zero; the human-readable `eprintln!` messages stay
    // for friendly framing, but the error-vs-non-error distinction is no
    // longer collapsed at the exit boundary.
    let mut config = config::load_config().map_err(|e| {
        if config::config_file_exists() {
            eprintln!("Failed to parse configuration: {e}");
            anyhow::anyhow!("invalid configuration: {e}")
        } else {
            eprintln!("No configuration found. Run 'openclaudia init' first.");
            anyhow::anyhow!("no configuration found")
        }
    })?;

    // Auto-detect provider from model name
    if let Some(ref model) = model_override {
        let detected = openclaudia::proxy::determine_provider(model, &config);
        if detected != config.proxy.target {
            config.proxy.target = detected;
        }
    }

    let Some(provider) = config.active_provider() else {
        eprintln!(
            "No provider configured for target '{}'",
            config.proxy.target
        );
        anyhow::bail!(
            "no provider configured for target '{}'",
            config.proxy.target
        );
    };

    let Some(ChatAuth {
        api_key,
        claude_code_token,
    }) = resolve_chat_auth(&config.proxy.target, provider).await?
    else {
        // resolve_chat_auth already printed the user-facing error; surface
        // as a non-zero exit so shell wrappers detect the failure.
        anyhow::bail!(
            "could not resolve authentication for target '{}'",
            config.proxy.target
        );
    };

    let model = resolve_model_name(model_override, provider.model.clone(), &config.proxy.target);
    // Crosslink #433: a typo'd `proxy.target` now surfaces as an explicit
    // error here, instead of being silently mapped to `OpenAIAdapter` and
    // producing 4xx responses from the upstream that the user can't
    // attribute to a config typo.
    let endpoint = openclaudia::pipeline::resolve_endpoint(
        &config.proxy.target,
        &model,
        &provider.base_url,
        claude_code_token.as_deref(),
    )?;
    let headers = openclaudia::pipeline::resolve_headers(
        &config.proxy.target,
        api_key.as_ref(),
        claude_code_token.as_deref(),
        &provider
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>(),
    )?;

    guardrails::configure(&config.guardrails);
    tui_launch(&config, &model, endpoint, headers, claude_code_token)
}

/// Build TUI system resources (memory, prompt, hooks, rules) and launch the app.
///
/// Extracted from `cmd_tui` to keep that function under the line limit.
fn tui_launch(
    config: &config::AppConfig,
    model: &str,
    endpoint: String,
    headers: Vec<(String, String)>,
    claude_code_token: Option<String>,
) -> anyhow::Result<()> {
    use openclaudia::hooks::{load_claude_code_hooks, merge_hooks_config, HookEngine};
    use openclaudia::rules::RulesEngine;

    let cwd_path = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let memory_db: Option<memory::MemoryDb> = memory::MemoryDb::open_for_project(&cwd_path).ok();

    let cwd = cwd_path.to_string_lossy().to_string();
    let tui_prompt_blocks = prompt::build_system_prompt_blocks(
        &openclaudia::modes::BehaviorMode::default(),
        None,
        None,
        memory_db.as_ref(),
        Some(&cwd),
    );
    let system_prompt = tui_prompt_blocks.to_combined();

    let claude_hooks = load_claude_code_hooks();
    let merged_hooks = merge_hooks_config(config.hooks.clone(), claude_hooks);
    let hook_engine = std::sync::Arc::new(HookEngine::new(merged_hooks));

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

    let mut app = tui::app::App::new(model, &config.proxy.target);
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

/// Returns `true` for tools that require an explicit permission decision before execution.
///
/// Read-only / informational tools (e.g. `read_file`, `grep`, `web_fetch`) return `false`
/// and are always executed without prompting. Write / destructive tools (`bash`,
/// `write_file`, `edit_file`) return `true`.
fn tool_needs_permission(tool_name: &str) -> bool {
    !matches!(
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
    )
}

/// Build a human-readable description of a tool call for the permission prompt.
fn tool_call_description(tool_name: &str, tool_args: &serde_json::Value) -> String {
    match tool_name {
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
    }
}

/// Check whether a tool call requires interactive permission and prompt the user if so.
///
/// Read-only / informational tools execute without prompting. Write/destructive tools
/// (`bash`, `write_file`, `edit_file`, etc.) prompt the user unless the tool has been
/// marked "always allow" for this session via a previous `a` response.
///
/// Use [`check_tool_unrestricted`] instead when running in headless/non-interactive mode
/// where all tool calls must be auto-approved (e.g. `--dangerously-skip-permissions`).
///
/// # Fix #284
///
/// This function replaces the old `check_tool_permission_interactive(skip_permissions: bool, …)`
/// boolean-flag anti-pattern. The two distinct behaviors are now two distinct functions.
///
/// Returns `Allowed` to proceed, or `Denied(message)` to send back to the model.
fn check_tool_permission_interactive(
    tool_name: &str,
    tool_args: &serde_json::Value,
    always_allowed: &mut std::collections::HashSet<String>,
) -> ToolPermissionResult {
    use std::io::Write as _;

    if !tool_needs_permission(tool_name) {
        return ToolPermissionResult::Allowed;
    }

    // Check session-level "always allow" cache
    if always_allowed.contains(tool_name) {
        return ToolPermissionResult::Allowed;
    }

    let description = tool_call_description(tool_name, tool_args);

    eprint!("\x1b[33m⚠ {description}\x1b[0m [y/n/a(lways)] ");
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

/// Bypass permission checks and auto-approve all tool calls.
///
/// This is the explicit bypass path used when `--dangerously-skip-permissions` is set.
/// Unlike [`check_tool_permission_interactive`], this function never prompts the user and
/// always returns `Allowed`.
///
/// # Fix #284
///
/// Replaces the old `skip_permissions: bool` boolean-flag parameter on
/// `check_tool_permission_interactive`. The caller's intent is now expressed by calling
/// this function, not by passing a bool.
///
/// # Safety
///
/// Calling this function grants unrestricted tool execution. Only call it when the
/// user has explicitly opted in via `--dangerously-skip-permissions`.
const fn check_tool_unrestricted(
    _tool_name: &str,
    _tool_args: &serde_json::Value,
) -> ToolPermissionResult {
    ToolPermissionResult::Allowed
}

/// Interactive chat mode (default command)
/// Read multiline continuation lines after the initial input ends
/// with a trailing backslash. Replaces each trailing `\\` with a
/// newline and appends the next prompted line, stopping when the user
/// submits a line that does NOT end with `\\` or when readline errors
/// (EOF / interrupt).
///
/// Extracted from `cmd_chat` per crosslink #262.
fn read_multiline_continuation(input: &mut String, rl: &mut rustyline::DefaultEditor) {
    while input.ends_with('\\') {
        input.pop(); // remove the trailing backslash
        match rl.readline("... ") {
            Ok(cont_line) => {
                input.push('\n');
                input.push_str(cont_line.trim());
            }
            Err(_) => break,
        }
    }
}

/// Check whether the session has grown close to the model's context
/// window and auto-compact or warn accordingly.
///
/// Invariants preserved from the inline version:
/// - Skips entirely when the session has 6 or fewer messages (the
///   compaction heuristic needs a minimum message count).
/// - `should_compact` implies compaction runs AND the message pops to
///   log the before/after counts.
/// - `should_warn` (without compact) prints a hint about `/compact`.
///
/// Extracted from `cmd_chat` per crosslink #262.
fn maybe_auto_compact(chat_session: &mut ChatSession, model: &str) {
    if chat_session.messages.len() <= 6 {
        return;
    }
    let est = estimate_session_tokens(chat_session);
    let (should_warn, should_compact, pct) =
        openclaudia::compaction::check_context_budget(est, model);
    if should_compact {
        eprintln!("\x1b[33m⚠ Context at {pct:.0}% — auto-compacting...\x1b[0m");
        let (before, after) = compact_chat_session(chat_session);
        eprintln!("\x1b[32m✓ Compacted: {before} → {after} messages\x1b[0m");
    } else if should_warn {
        eprintln!("\x1b[33m⚠ Context at {pct:.0}% — use /compact to free space\x1b[0m");
    }
}

/// Build a hook engine from config + Claude Code settings.json.
///
/// Extracted from `cmd_chat` per crosslink #262.
fn build_hook_engine(config: &config::AppConfig) -> openclaudia::hooks::HookEngine {
    let claude_hooks = openclaudia::hooks::load_claude_code_hooks();
    let merged_hooks = openclaudia::hooks::merge_hooks_config(config.hooks.clone(), claude_hooks);
    openclaudia::hooks::HookEngine::new(merged_hooks)
}

/// Clear the screen, render the TUI welcome panel, and fall back to a
/// plain-text banner when the TUI fails to render (e.g. non-TTY).
///
/// Extracted from `cmd_chat` per crosslink #262.
fn render_welcome_or_fallback(target: &str, model: &str) {
    let _ = tui::clear_screen();
    let welcome = tui::WelcomeScreen::new(env!("CARGO_PKG_VERSION"), target, model);
    if let Err(e) = welcome.render() {
        eprintln!("TUI render failed: {e}, using simple output");
        println!("OpenClaudia v{}", env!("CARGO_PKG_VERSION"));
        println!("Provider: {target} | Model: {model}");
        println!("Type /help for commands, /sessions to list saved chats");
        println!("Tip: {}\n", get_random_tip());
    }
}

/// Construct the library-layer `PermissionManager` with the config's
/// `default_allow` patterns. Extracted from `cmd_chat` per #262.
fn init_permission_manager(
    config: &config::AppConfig,
    dangerously_skip_permissions: bool,
) -> PermissionManager {
    // `--dangerously-skip-permissions` is the documented bypass. Lift it all
    // the way to the lower-level gate by constructing a permission manager
    // with `enabled = false`, which short-circuits `check()` to `Allowed`
    // (see `PermissionManager::unrestricted` + sprint-211 tests). Previously
    // the flag only affected the higher-level REPL gate and the inner
    // `execute_tool_with_*` path kept producing `PERMISSION_PROMPT` results
    // that the model could not satisfy in a non-interactive run.
    if dangerously_skip_permissions {
        return PermissionManager::unrestricted();
    }
    PermissionManager::new(
        std::path::PathBuf::from(".openclaudia/permissions.json"),
        true,
        config.permissions.default_allow.clone(),
    )
}

/// Apply `--resume` / `--session-id` to select a prior chat session.
///
/// If `resume` is true OR `session_id` is `Some`, looks up the saved
/// sessions and replaces the passed-in session in-place with the best
/// match (prefix match on `session_id`, else the most-recent one).
/// Prints a user-facing status line in either case.
///
/// Extracted from `cmd_chat` per crosslink #262.
fn maybe_resume_session(chat_session: &mut ChatSession, resume: bool, session_id: Option<&str>) {
    if !resume && session_id.is_none() {
        return;
    }
    let sessions = list_chat_sessions();
    let target = if let Some(id) = session_id {
        sessions.iter().find(|s| s.id.starts_with(id)).cloned()
    } else {
        sessions.into_iter().next()
    };
    if let Some(loaded) = target {
        eprintln!("Resuming session: {} ({})", loaded.title, &loaded.id[..8]);
        *chat_session = loaded;
    } else {
        eprintln!("No session found to resume. Starting new session.");
    }
}

/// Open the project-scoped memory database and print one-line status
/// banners for recent-session count and auto-learning stats.
///
/// Returns `None` if the database cannot be opened — the caller then
/// runs without memory (a `tracing::warn`! is logged, but the session
/// still starts). Extracted from `cmd_chat` per crosslink #262.
fn init_memory_with_banner() -> Option<memory::MemoryDb> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match memory::MemoryDb::open_for_project(&cwd) {
        Ok(db) => {
            let recent_count = db.get_recent_sessions(10).map_or(0, |s| s.len());
            if recent_count > 0 {
                println!("\x1b[90m📝 {recent_count} recent session(s) loaded from memory\x1b[0m");
            }

            if let Ok(stats) = db.auto_learn_stats() {
                let total = stats.coding_patterns
                    + stats.error_patterns
                    + stats.learned_preferences
                    + stats.file_relationships;
                if total > 0 {
                    println!(
                        "\x1b[90m🧠 Auto-learned: {} patterns, {} error fixes, {} preferences, {} file relationships\x1b[0m",
                        stats.coding_patterns,
                        stats.errors_resolved,
                        stats.learned_preferences,
                        stats.file_relationships
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
    }
}

/// Build the VDD engine if VDD is enabled in config, printing a status
/// banner. Returns `None` when disabled — `cmd_chat` passes that
/// through to every review call site so VDD is a no-op.
///
/// Uses a 120-second reqwest timeout (the per-request timeout added
/// in crosslink #496 applies inside the engine itself — this is the
/// outer transport timeout). Extracted from `cmd_chat` per #262.
fn init_vdd_engine_if_enabled(config: &config::AppConfig) -> Option<vdd::VddEngine> {
    if !config.vdd.enabled {
        return None;
    }
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_mins(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    println!(
        "\x1b[33m🔍 VDD enabled ({} mode) - adversary: {}\x1b[0m",
        config.vdd.mode, config.vdd.adversary.provider
    );
    Some(vdd::VddEngine::new(&config.vdd, config, http_client))
}

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
    // crosslink #893: try_new surfaces "no home directory" as an explicit
    // error. Production code logs it loudly and falls back to the
    // project-only manager so the operator sees the misconfiguration
    // rather than discovering it via missing plugins.
    let mut plugin_manager = match plugins::PluginManager::try_new() {
        Ok(pm) => pm,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "PluginManager: falling back to project-only search (no user home)"
            );
            plugins::PluginManager::new()
        }
    };
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

/// Canonical per-target default model table.
///
/// Single source of truth for the per-provider fallback model picked by
/// [`resolve_model_name`] when neither the `-m` flag nor the provider's
/// configured model is set (crosslink #802). The table is `&'static` so
/// callers can grep for every literal model string the proxy ships, and
/// the round-trip test `default_models_table_is_canonical` pins each entry
/// against the matching adapter, giving us a compile-/test-time guard
/// against silent drift (`claude-opus-4-6` → `4-7` → `4-8` …).
const DEFAULT_MODELS_BY_TARGET: &[(&str, &str)] = &[
    ("anthropic", "claude-opus-4-6"),
    ("google", "gemini-2.5-flash"),
    ("zai", "glm-5"),
    ("deepseek", "deepseek-chat"),
    ("qwen", "qwen3.5-plus"),
];

/// Fallback model for targets not listed in [`DEFAULT_MODELS_BY_TARGET`].
/// Currently every non-table target is treated as OpenAI-compatible.
const DEFAULT_MODEL_FALLBACK: &str = "gpt-5.2";

/// Look up the canonical default model for a target, or [`DEFAULT_MODEL_FALLBACK`].
fn default_model_for_target(target: &str) -> &'static str {
    DEFAULT_MODELS_BY_TARGET
        .iter()
        .find_map(|(t, m)| (*t == target).then_some(*m))
        .unwrap_or(DEFAULT_MODEL_FALLBACK)
}

/// Resolve the model name to use for a chat session.
///
/// Priority: explicit `-m` flag > provider's configured model > a
/// per-target default sourced from [`DEFAULT_MODELS_BY_TARGET`]. Pure
/// function — no I/O, no mutation. Extracted from `cmd_chat` per crosslink
/// #262.
fn resolve_model_name(
    model_override: Option<String>,
    provider_model: Option<String>,
    target: &str,
) -> String {
    model_override
        .or(provider_model)
        .unwrap_or_else(|| default_model_for_target(target).to_string())
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
            eprintln!("Install Claude Code and run `claude` to log in, or set ANTHROPIC_API_KEY.");
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

/// Build the provider-specific JSON request body for one chat turn.
///
/// Handles Anthropic multi-block system prompts, Google Gemini content
/// format, and the OpenAI-compatible format used by every other provider.
/// Also applies effort-level thinking parameters and injects the Claude
/// Code OAuth system prompt when `claude_code_token` is present.
///
/// Extracted from `cmd_chat` (crosslink #262) to reduce function length
/// and enable independent unit tests.
/// Run VDD adversarial review and print findings.
///
/// Extracted from `cmd_chat` (crosslink #262) — this block appears at three
/// call sites in the function.  The caller is responsible for the `!cancelled`
/// guard and the `vdd_engine.is_some()` check before calling.
async fn run_vdd_review(
    engine: &vdd::VddEngine,
    content: &str,
    messages: &mut Vec<serde_json::Value>,
    target: &str,
    api_key: Option<&openclaudia::providers::ApiKey>,
) {
    let user_task = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("");

    let builder = vdd::BuilderProvider::new(target, api_key);
    match engine.review_text(content, user_task, builder).await {
        Ok(result) => {
            if result.findings.is_empty() {
                println!("\n\x1b[32m✓ VDD Review: No issues found\x1b[0m");
            } else {
                let genuine_count = result
                    .findings
                    .iter()
                    .filter(|f| f.status == vdd::FindingStatus::Genuine)
                    .count();
                println!(
                    "\n\x1b[33m🔍 VDD Review: {} finding(s) ({} genuine)\x1b[0m",
                    result.findings.len(),
                    genuine_count
                );
                for finding in &result.findings {
                    let status_icon = match finding.status {
                        vdd::FindingStatus::Genuine => "⚠",
                        vdd::FindingStatus::FalsePositive => "✗",
                        vdd::FindingStatus::Disputed => "?",
                    };
                    println!(
                        "  {} [{}] {}",
                        status_icon, finding.severity, finding.description
                    );
                }
                if !result.context_injection.is_empty() {
                    messages.push(serde_json::json!({
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
            println!("\n\x1b[31m⚠ VDD review failed: {e}\x1b[0m");
        }
    }
}

/// Build the Anthropic-direct request body. Crosslink #918: extracted so
/// `build_chat_request_body` no longer carries the full cyclomatic load.
fn build_chat_body_anthropic(
    messages: &[serde_json::Value],
    model: &str,
    prompt_blocks: &openclaudia::prompt::SystemPromptBlocks,
) -> serde_json::Value {
    use openclaudia::providers::{convert_messages_to_anthropic, convert_tools_to_anthropic};
    let anthropic_messages = convert_messages_to_anthropic(messages);
    let openai_tools = tools::get_all_tool_definitions(true);
    let anthropic_tools = convert_tools_to_anthropic(openai_tools.as_array().unwrap_or(&vec![]));

    let mut req = serde_json::json!({
        "model": model,
        "messages": anthropic_messages,
        "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
        "stream": true,
        "tools": anthropic_tools
    });
    // Multi-block system prompt: stable prefix cached, dynamic suffix reprocessed.
    req["system"] = openclaudia::providers::build_system_blocks(prompt_blocks);
    req
}

/// Translate `OpenAI`-format tool definitions into Gemini function declarations.
fn build_gemini_functions() -> Vec<serde_json::Value> {
    let openai_tools = tools::get_all_tool_definitions(true);
    let tools_vec = openai_tools.as_array().cloned().unwrap_or_default();
    tools_vec
        .iter()
        .filter_map(|tool| {
            let func = tool.get("function")?;
            let description = func
                .get("description")
                .cloned()
                .unwrap_or_else(|| serde_json::json!(""));
            let parameters = func
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Some(serde_json::json!({
                "name": func.get("name")?,
                "description": description,
                "parameters": parameters
            }))
        })
        .collect()
}

/// Split messages into (`gemini_contents`, `system_text`) for Gemini's
/// distinct `contents` / `systemInstruction` shape.
fn split_messages_for_gemini(
    messages: &[serde_json::Value],
) -> (Vec<serde_json::Value>, Option<String>) {
    let mut contents = Vec::new();
    let mut system_text: Option<String> = None;
    for msg in messages {
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
    (contents, system_text)
}

/// Build the Gemini request body.
fn build_chat_body_google(messages: &[serde_json::Value]) -> serde_json::Value {
    let functions = build_gemini_functions();
    let (contents, system_text) = split_messages_for_gemini(messages);
    let mut req = serde_json::json!({
        "contents": contents,
        "generationConfig": {"maxOutputTokens": 4096},
        "tools": [{"functionDeclarations": functions}]
    });
    if let Some(sys) = system_text {
        req["systemInstruction"] = serde_json::json!({"parts": [{"text": sys}]});
    }
    req
}

/// Build the generic OpenAI-compatible request body.
fn build_chat_body_openai_like(messages: &[serde_json::Value], model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": openclaudia::DEFAULT_MAX_TOKENS,
        "stream": true,
        "tools": tools::get_all_tool_definitions(true)
    })
}

/// Apply Anthropic-specific `effort_level` mapping in place.
fn apply_anthropic_effort_level(request_body: &mut serde_json::Value, effort_level: &str) {
    match effort_level {
        "high" => {
            request_body["thinking"] =
                serde_json::json!({"type": "enabled", "budget_tokens": 10000});
            request_body["max_tokens"] = serde_json::json!(16000);
        }
        "low" => {
            request_body["max_tokens"] = serde_json::json!(2048);
        }
        _ => {} // medium = default
    }
}

/// Build a per-turn chat request body for the configured target provider.
///
/// Crosslink #918: the original 100-line function had three deeply nested
/// provider branches plus two cross-cutting mutations and a cyclomatic
/// complexity > 15. It has been decomposed into per-provider helpers
/// (`build_chat_body_{anthropic,google,openai_like}`) plus
/// `apply_anthropic_effort_level` and the cross-cutting OAuth prefix
/// injection. Each helper handles a single provider's shape; this
/// function is now a thin orchestrator.
fn build_chat_request_body(
    target: &str,
    messages: &[serde_json::Value],
    model: &str,
    prompt_blocks: &openclaudia::prompt::SystemPromptBlocks,
    effort_level: &str,
    claude_code_token: Option<&str>,
) -> serde_json::Value {
    let mut request_body = match target {
        "anthropic" => build_chat_body_anthropic(messages, model, prompt_blocks),
        "google" => build_chat_body_google(messages),
        _ => build_chat_body_openai_like(messages, model),
    };

    // Inject Claude Code OAuth system prompt when using OAuth auth.
    if claude_code_token.is_some() {
        openclaudia::claude_credentials::inject_system_prompt(&mut request_body);
    }

    // Effort-level thinking parameters are Anthropic-only today.
    if target == "anthropic" {
        apply_anthropic_effort_level(&mut request_body, effort_level);
    }

    request_body
}

/// Build the per-turn API endpoint URL and auth headers.
///
/// Handles Claude Code OAuth (direct Anthropic) vs key-based auth
/// and merges any custom headers from the provider configuration.
///
/// Extracted from `cmd_chat` (crosslink #262).
fn build_chat_endpoint_and_headers(
    target: &str,
    model: &str,
    provider: &config::ProviderConfig,
    adapter: &dyn openclaudia::providers::ProviderAdapter,
    api_key: Option<&openclaudia::providers::ApiKey>,
    claude_code_token: Option<&str>,
) -> (String, Vec<(String, String)>) {
    let _ = target; // used only for documentation clarity; routing is on claude_code_token
    let endpoint = if claude_code_token.is_some() {
        openclaudia::claude_credentials::get_oauth_endpoint(model)
    } else {
        format!(
            "{}{}",
            normalize_base_url(&provider.base_url),
            adapter.chat_endpoint(model)
        )
    };

    let mut headers: Vec<(String, String)> = claude_code_token.map_or_else(
        || api_key.map_or_else(Vec::new, |key| adapter.get_headers(key)),
        openclaudia::claude_credentials::get_oauth_headers,
    );
    // Merge in any custom headers from provider config
    headers.extend(provider.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
    (endpoint, headers)
}

async fn cmd_chat(
    model_override: Option<String>,
    resume: bool,
    session_id: Option<String>,
    coordinator: bool,
    dangerously_skip_permissions: bool,
    mode_arg: Option<String>,
) -> anyhow::Result<()> {
    // The original \~2.4k-line `cmd_chat` body was decomposed into
    // `cli::chat_repl::ChatRepl` (crosslink #262) so each method fits
    // under the clippy::too_many_lines threshold. Behaviour is
    // preserved — see `src/cli/chat_repl.rs` for the loop body, slash
    // dispatcher, and provider-specific response handlers.
    let Some(repl) = cli::chat_repl::ChatRepl::new(cli::chat_repl::ChatReplArgs {
        model_override,
        resume,
        session_id,
        coordinator,
        dangerously_skip_permissions,
        mode_arg,
    })
    .await?
    else {
        // `ChatRepl::new` already printed a user-facing error.
        return Ok(());
    };
    repl.run().await
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
        assert_eq!(
            resolve_model_name(None, None, "anthropic"),
            "claude-opus-4-6"
        );
        assert_eq!(resolve_model_name(None, None, "openai"), "gpt-5.2");
        assert_eq!(resolve_model_name(None, None, "google"), "gemini-2.5-flash");
        assert_eq!(resolve_model_name(None, None, "zai"), "glm-5");
        assert_eq!(resolve_model_name(None, None, "deepseek"), "deepseek-chat");
        assert_eq!(resolve_model_name(None, None, "qwen"), "qwen3.5-plus");
        // Unknown target falls back to the OpenAI default.
        assert_eq!(
            resolve_model_name(None, None, "unknown-provider"),
            "gpt-5.2"
        );
    }

    /// Crosslink #802: the per-target default model table is the single
    /// source of truth for [`resolve_model_name`]. This test pins every
    /// entry against the resolver so that:
    ///
    /// * any new entry added to [`DEFAULT_MODELS_BY_TARGET`] is exercised
    ///   end-to-end without anyone having to remember to update a parallel
    ///   match arm,
    /// * removing or renaming an entry forces the test to be updated in
    ///   lockstep (no silent drift between the table and the resolver),
    /// * the literal model strings themselves are pinned — a stray edit
    ///   from e.g. `claude-opus-4-6` to `claude-opus-4-7` will fail the
    ///   round-trip and force a deliberate version bump.
    #[test]
    fn default_models_table_is_canonical_for_resolver() {
        for (target, expected_model) in DEFAULT_MODELS_BY_TARGET {
            let got = resolve_model_name(None, None, target);
            assert_eq!(
                got, *expected_model,
                "DEFAULT_MODELS_BY_TARGET entry for `{target}` must round-trip through resolve_model_name"
            );
            assert_eq!(
                default_model_for_target(target),
                *expected_model,
                "default_model_for_target must agree with DEFAULT_MODELS_BY_TARGET for `{target}`"
            );
        }
        // The fallback constant pins the openai/unknown default literal too.
        assert_eq!(
            default_model_for_target("definitely-not-a-known-target"),
            DEFAULT_MODEL_FALLBACK
        );
        assert_eq!(DEFAULT_MODEL_FALLBACK, "gpt-5.2");
    }

    /// #802 (companion): the table must not contain duplicate target keys —
    /// duplicates would silently shadow each other depending on iteration
    /// order. Also enforces that no target key is empty.
    #[test]
    fn default_models_table_keys_are_unique_and_non_empty() {
        use std::collections::HashSet;
        let mut seen: HashSet<&str> = HashSet::new();
        for (target, _) in DEFAULT_MODELS_BY_TARGET {
            assert!(!target.is_empty(), "target key must not be empty");
            assert!(
                seen.insert(target),
                "duplicate target key `{target}` in DEFAULT_MODELS_BY_TARGET"
            );
        }
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

    #[test]
    fn maybe_auto_compact_is_noop_for_small_sessions() {
        // Under the 6-message short-circuit, auto-compact must not touch
        // the session. Build the smallest possible ChatSession with an
        // empty message history.
        let mut session = ChatSession::new(
            "claude-sonnet-4-6",
            "anthropic",
            openclaudia::modes::BehaviorMode::default(),
        );
        let before_len = session.messages.len();
        // Any model name is fine — the short-circuit fires before the
        // model lookup.
        maybe_auto_compact(&mut session, "claude-sonnet-4-6");
        assert_eq!(session.messages.len(), before_len);
    }
}
