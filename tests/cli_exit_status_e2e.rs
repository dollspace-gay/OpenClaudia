use std::{
    fs,
    io::Write,
    process::{Command, Output, Stdio},
};

const CONFIG_ENV_VARS: &[&str] = &[
    "OPENCLAUDIA_PROXY_PORT",
    "OPENCLAUDIA_PROXY_HOST",
    "OPENCLAUDIA_PROXY_TARGET",
    "OPENCLAUDIA_SESSION_TIMEOUT_MINUTES",
    "OPENCLAUDIA_SESSION_PERSIST_PATH",
    "OPENCLAUDIA_PROVIDERS_ANTHROPIC_API_KEY",
    "OPENCLAUDIA_PROVIDERS_OPENAI_API_KEY",
    "OPENCLAUDIA_PROVIDERS_GOOGLE_API_KEY",
    "OPENCLAUDIA_PROVIDERS_ZAI_API_KEY",
    "OPENCLAUDIA_PROVIDERS_DEEPSEEK_API_KEY",
    "OPENCLAUDIA_PROVIDERS_QWEN_API_KEY",
    "OPENCLAUDIA_PROVIDERS_KIMI_API_KEY",
    "OPENCLAUDIA_PROVIDERS_MINIMAX_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GOOGLE_API_KEY",
    "ZAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "QWEN_API_KEY",
    "KIMI_API_KEY",
    "MOONSHOT_API_KEY",
    "MINIMAX_API_KEY",
];

fn assert_missing_config_is_failure(args: &[&str]) {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_openclaudia"));
    command
        .args(args)
        .current_dir(cwd.path())
        .env("HOME", home.path());
    for var in CONFIG_ENV_VARS {
        command.env_remove(var);
    }

    let output = command.output().expect("openclaudia command must run");

    assert!(
        !output.status.success(),
        "openclaudia {args:?} must fail without config; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("No configuration found") || combined.contains("no configuration found"),
        "missing-config failure should explain the problem; got {combined:?}"
    );
}

fn isolated_command(cwd: &tempfile::TempDir, home: &tempfile::TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openclaudia"));
    command.current_dir(cwd.path()).env("HOME", home.path());
    for var in CONFIG_ENV_VARS {
        command.env_remove(var);
    }
    command
}

fn run_auth_with_stdin(input: &str) -> Output {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let mut child = isolated_command(&cwd, &home)
        .arg("auth")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("openclaudia auth must spawn");

    child
        .stdin
        .take()
        .expect("auth stdin should be piped")
        .write_all(input.as_bytes())
        .expect("auth stdin should accept input");

    child.wait_with_output().expect("openclaudia auth must run")
}

#[test]
fn config_without_config_exits_nonzero() {
    assert_missing_config_is_failure(&["config"]);
}

#[test]
fn config_accepts_documented_local_provider_loopback_url() {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let config_dir = cwd.path().join(".openclaudia");
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("config.yaml"),
        r#"
proxy:
  port: 8080
  host: "127.0.0.1"
  target: local
providers:
  local:
    base_url: http://localhost:1234/v1
"#,
    )
    .expect("config file");

    let output = isolated_command(&cwd, &home)
        .arg("config")
        .output()
        .expect("openclaudia config must run");

    assert!(
        output.status.success(),
        "documented local provider localhost base_url must load; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Target: local"),
        "config output should show active local target; got {stdout:?}"
    );
}

#[test]
fn start_without_config_exits_nonzero() {
    assert_missing_config_is_failure(&["start"]);
}

#[test]
fn loop_without_config_exits_nonzero() {
    assert_missing_config_is_failure(&["loop", "--max-iterations", "1"]);
}

#[test]
fn acp_without_config_exits_nonzero() {
    assert_missing_config_is_failure(&["acp"]);
}

#[test]
fn doctor_without_config_exits_nonzero() {
    assert_missing_config_is_failure(&["doctor"]);
}

#[test]
fn legacy_repl_without_config_exits_nonzero() {
    assert_missing_config_is_failure(&["--tui-mode"]);
}

#[test]
fn doctor_does_not_create_session_state() {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let config_dir = cwd.path().join(".openclaudia");
    fs::create_dir_all(&config_dir).expect("config dir");
    fs::write(
        config_dir.join("config.yaml"),
        "proxy:\n  target: missing-provider\n",
    )
    .expect("config file");

    let output = isolated_command(&cwd, &home)
        .arg("doctor")
        .output()
        .expect("openclaudia doctor must run");

    assert!(
        !output.status.success(),
        "doctor should fail when active provider is missing; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !config_dir.join("session").exists(),
        "doctor must not create session state while diagnosing failures"
    );
}

#[test]
fn auth_without_code_exits_nonzero() {
    let output = run_auth_with_stdin("");

    assert!(
        !output.status.success(),
        "auth with empty stdin must fail; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("No code provided. Authentication cancelled."),
        "cancelled auth should explain the failure; got {combined:?}"
    );
}

#[test]
fn auth_state_mismatch_exits_nonzero_before_token_exchange() {
    let output = run_auth_with_stdin("test-code#definitely-not-the-generated-state\n");

    assert!(
        !output.status.success(),
        "auth with mismatched state must fail; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("State mismatch! This could be a CSRF attack."),
        "state mismatch should explain the failure; got {combined:?}"
    );
    assert!(
        !combined.contains("Exchanging code for tokens"),
        "state mismatch must abort before network token exchange; got {combined:?}"
    );
}

#[test]
fn help_describes_tui_mode_as_legacy_repl_escape_hatch() {
    let output = Command::new(env!("CARGO_BIN_EXE_openclaudia"))
        .arg("--help")
        .output()
        .expect("openclaudia --help must run");

    assert!(output.status.success(), "--help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Launch legacy line-oriented REPL instead of the default full-screen TUI"),
        "--tui-mode help must describe the actual legacy REPL behavior; got {stdout:?}"
    );
    assert!(
        !stdout.contains("Launch full-screen interactive TUI"),
        "--tui-mode help must not claim to launch the default TUI"
    );
}

#[test]
fn readme_cli_examples_do_not_advertise_stale_tui_or_coordinator_modes() {
    let readme = include_str!("../README.md");

    assert!(
        readme.contains("openclaudia --tui-mode         # Legacy line-oriented REPL"),
        "README must match the binary's --tui-mode behavior"
    );
    assert!(
        readme.contains("openclaudia --coordinator --tui-mode"),
        "README must show --coordinator with the required legacy REPL flag"
    );
    assert!(
        !readme.contains("openclaudia --coordinator      # Multi-agent coordinator mode"),
        "README must not advertise the Phase 1 coordinator as a working binary mode"
    );
    assert!(
        !readme.contains("openclaudia --tui-mode         # Full-screen TUI"),
        "README must not claim --tui-mode launches the default full-screen TUI"
    );
    assert!(
        readme.contains("#   default_allow:"),
        "README permissions sample must use the supported config schema"
    );
    assert!(
        !readme.contains("denied_tools") && !readme.contains("denied_commands"),
        "README permissions sample must not advertise unsupported deny-list fields"
    );
    assert!(
        readme.contains("openclaudia loop -n 10         # Max 10 iterations"),
        "README loop example must use the loop subcommand's -n/--max-iterations flag"
    );
    assert!(
        !readme.contains("openclaudia loop -m 10"),
        "README must not claim global -m/--model controls loop iteration count"
    );
    assert!(
        readme.contains("## Slash Commands (Default TUI)"),
        "README slash-command docs must describe the default full-screen TUI"
    );
    assert!(
        readme.contains("## Keyboard Shortcuts (Default TUI)"),
        "README keyboard docs must describe the default full-screen TUI"
    );
    assert!(
        readme.contains("The `keybindings:` config map customizes the legacy line-oriented REPL"),
        "README must explain that configurable keybindings apply to the legacy REPL"
    );
    for stale_tui_shortcut in [
        "| `Ctrl-X N` | New session |",
        "| `Ctrl-X M` | Show models |",
        "| `F2` | Show models |",
    ] {
        assert!(
            !readme.contains(stale_tui_shortcut),
            "README default-TUI keyboard docs must not advertise legacy shortcut: {stale_tui_shortcut}"
        );
    }
    for stale_tui_claim in [
        "| `/connect`, `/auth` | Configure API keys |",
        "| `/config path` | Show config file locations |",
        "| `/model <name>` | Switch to different model mid-session |",
        "| `/continue <n>`, `/load <n>`, `/resume <n>` | Load session by number |",
    ] {
        assert!(
            !readme.contains(stale_tui_claim),
            "README default-TUI slash docs must not advertise unsupported/stale command: {stale_tui_claim}"
        );
    }
}

#[test]
fn init_template_marks_keybindings_as_legacy_repl_specific() {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let output = isolated_command(&cwd, &home)
        .arg("init")
        .output()
        .expect("openclaudia init must run");

    assert!(
        output.status.success(),
        "init should succeed in empty tempdir; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let config = fs::read_to_string(cwd.path().join(".openclaudia/config.yaml"))
        .expect("init should write config.yaml");
    assert!(
        config.contains("Legacy line REPL keybindings (`openclaudia --tui-mode`)"),
        "init template must label keybindings as legacy REPL-specific"
    );
    assert!(
        config.contains("uses its built-in shortcuts; type /help there to view them"),
        "init template must point default-TUI users at /help"
    );
    assert!(
        !config.contains("# Keyboard shortcuts - map key combinations to actions"),
        "init template must not imply keybindings customize the default TUI"
    );
}

#[test]
fn coordinator_without_legacy_repl_exits_nonzero() {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let output = isolated_command(&cwd, &home)
        .arg("--coordinator")
        .output()
        .expect("openclaudia --coordinator must run");

    assert!(
        !output.status.success(),
        "default TUI must not silently ignore --coordinator; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("--coordinator is only supported by the legacy REPL"),
        "coordinator failure should explain the required mode; got {combined:?}"
    );
}
