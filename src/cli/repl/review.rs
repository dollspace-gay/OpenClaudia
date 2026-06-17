use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::LazyLock;

/// Absolute, PATH-independent location of `git` for review helpers.
static GIT_BIN: LazyLock<Result<PathBuf, String>> =
    LazyLock::new(|| which::which("git").map_err(|e| format!("git binary not found on PATH: {e}")));

fn git_bin() -> Result<&'static Path, String> {
    match &*GIT_BIN {
        Ok(path) => Ok(path.as_path()),
        Err(msg) => Err(msg.clone()),
    }
}

fn git_output(args: &[&str]) -> Result<Output, String> {
    Command::new(git_bin()?)
        .args(args)
        .output()
        .map_err(|e| e.to_string())
}

fn git_failure_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        message.to_string()
    }
}

/// Review uncommitted git changes or compare against a branch
pub fn review_git_changes(args: &str) {
    match git_output(&["rev-parse", "--git-dir"]) {
        Ok(output) if output.status.success() => {}
        Ok(_) => {
            println!("\nNot a git repository.\n");
            return;
        }
        Err(e) => {
            eprintln!("\nFailed to run git: {e}\n");
            return;
        }
    }

    println!();

    if args.is_empty() {
        review_uncommitted_changes();
    } else {
        review_branch_comparison(args.trim());
    }
}

fn review_uncommitted_changes() {
    println!("=== Git Status ===\n");
    let status = git_output(&["status", "--short"]);

    match status {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.is_empty() {
                println!("No changes detected.\n");
                return;
            }
            println!("{stdout}");
        }
        Ok(output) => {
            eprintln!(
                "Failed to run git status: {}\n",
                git_failure_message(&output)
            );
            return;
        }
        Err(e) => {
            eprintln!("Failed to run git status: {e}\n");
            return;
        }
    }

    println!("=== Uncommitted Changes ===\n");
    let diff = git_output(&["diff", "HEAD"]);

    match diff {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.is_empty() {
                println!("No diff to show (changes may be staged).\n");
            } else {
                let lines: Vec<&str> = stdout.lines().collect();
                if lines.len() > 100 {
                    for line in lines.iter().take(100) {
                        println!("{line}");
                    }
                    println!(
                        "\n... ({} more lines, use git diff directly for full output)\n",
                        lines.len() - 100
                    );
                } else {
                    println!("{stdout}");
                }
            }
        }
        Ok(output) => eprintln!("Failed to run git diff: {}\n", git_failure_message(&output)),
        Err(e) => eprintln!("Failed to run git diff: {e}\n"),
    }
}

fn review_branch_comparison(branch: &str) {
    println!("=== Comparing against '{branch}' ===\n");

    let verify_ref = format!("{branch}^{{commit}}");
    let branch_check = git_output(&[
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        verify_ref.as_str(),
    ]);

    let base_commit = match branch_check {
        Ok(output) if output.status.success() => {
            let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if commit.is_empty() {
                eprintln!("Branch '{branch}' not found.\n");
                return;
            }
            commit
        }
        Ok(_) => {
            eprintln!("Branch '{branch}' not found.\n");
            return;
        }
        Err(e) => {
            eprintln!("Failed to run git rev-parse: {e}\n");
            return;
        }
    };

    println!("Commits ahead of {branch}:\n");
    let range = format!("{base_commit}..HEAD");
    let log = git_output(&["log", "--oneline", range.as_str()]);

    match log {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.is_empty() {
                println!("  (no commits ahead)\n");
            } else {
                for line in stdout.lines() {
                    println!("  {line}");
                }
                println!();
            }
        }
        Ok(output) => eprintln!("Failed to run git log: {}\n", git_failure_message(&output)),
        Err(e) => eprintln!("Failed to run git log: {e}\n"),
    }

    println!("Changed files:\n");
    let diff_stat = git_output(&["diff", "--stat", base_commit.as_str()]);

    match diff_stat {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.is_empty() {
                println!("  (no changes)\n");
            } else {
                println!("{stdout}");
            }
        }
        Ok(output) => eprintln!(
            "Failed to run git diff --stat: {}\n",
            git_failure_message(&output)
        ),
        Err(e) => eprintln!("Failed to run git diff --stat: {e}\n"),
    }
}

/// Configure API key for a provider interactively
pub fn configure_provider_api_key() {
    use std::io::{self, Write};

    let providers = [
        ("anthropic", "Anthropic (Claude)", "ANTHROPIC_API_KEY"),
        ("openai", "OpenAI (GPT)", "OPENAI_API_KEY"),
        ("google", "Google (Gemini)", "GOOGLE_API_KEY"),
        ("deepseek", "DeepSeek", "DEEPSEEK_API_KEY"),
        ("qwen", "Qwen (Alibaba)", "QWEN_API_KEY"),
        ("zai", "Z.AI (GLM)", "ZAI_API_KEY"),
        (
            "kimi",
            "Kimi (Moonshot)",
            "KIMI_API_KEY or MOONSHOT_API_KEY",
        ),
        ("minimax", "MiniMax", "MINIMAX_API_KEY"),
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

    println!("\nConfiguring {provider_name}...");
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
        println!("Skipped. Set {env_var} environment variable instead.\n");
        return;
    }

    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("openclaudia");

    if let Err(e) = fs::create_dir_all(&config_dir) {
        eprintln!("Failed to create config directory: {e}\n");
        return;
    }

    let config_path = config_dir.join("config.yaml");

    match upsert_provider_api_key_config(&config_path, provider_id, provider_name, api_key) {
        Ok(ProviderConfigUpdate::AlreadyConfigured) => {
            println!("\nProvider already configured in config file.");
            println!("Edit {} to update.\n", config_path.display());
        }
        Ok(ProviderConfigUpdate::Saved) => {
            println!("\nSaved API key to: {}", config_path.display());
            println!("Restart the chat to use the new configuration.\n");
        }
        Err(e) => eprintln!("\n{e}\n"),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProviderConfigUpdate {
    AlreadyConfigured,
    Saved,
}

fn upsert_provider_api_key_config(
    config_path: &Path,
    provider_id: &str,
    provider_name: &str,
    api_key: &str,
) -> Result<ProviderConfigUpdate, String> {
    let mut config_content = match fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(format!(
                "Failed to read existing config {}: {e}",
                config_path.display()
            ));
        }
    };

    let key_pattern = format!("{provider_id}_api_key:");
    if config_content.contains(&key_pattern) {
        return Ok(ProviderConfigUpdate::AlreadyConfigured);
    }

    let quoted_api_key = serde_json::to_string(api_key).map_err(|e| {
        format!(
            "Failed to encode API key for config {}: {e}",
            config_path.display()
        )
    })?;
    let provider_section =
        format!("\n# {provider_name} configuration\n{provider_id}_api_key: {quoted_api_key}\n");
    config_content.push_str(&provider_section);

    fs::write(config_path, config_content)
        .map_err(|e| format!("Failed to save config {}: {e}", config_path.display()))?;

    Ok(ProviderConfigUpdate::Saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Value as YamlValue;

    #[test]
    fn review_git_helpers_use_resolved_binary_path() {
        let git = git_bin().expect("review tests require git on PATH");
        assert!(
            git.is_absolute(),
            "git_bin must resolve git to an absolute path, got {}",
            git.display()
        );

        let src = include_str!("review.rs");
        let cfg_test = src
            .find("#[cfg(test)]")
            .expect("test module marker must be present");
        let production = &src[..cfg_test];

        assert!(
            production.contains("\"--end-of-options\""),
            "branch verification must terminate git option parsing"
        );

        for (idx, raw_line) in production.lines().enumerate() {
            let code = raw_line.split("//").next().unwrap_or("");
            assert!(
                !code.contains("Command::new(\"git\")")
                    && !code.contains("std::process::Command::new(\"git\")"),
                "production review code must not invoke bare git; line {n}: {raw_line}",
                n = idx + 1,
            );
            assert!(
                !code.contains(".unwrap().status.success()"),
                "production review code must not unwrap git probes; line {n}: {raw_line}",
                n = idx + 1,
            );
        }
    }

    #[test]
    fn upsert_provider_api_key_config_rejects_unreadable_utf8_without_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        fs::write(&config_path, [0xff, 0xfe, 0xfd]).unwrap();

        let err = upsert_provider_api_key_config(&config_path, "openai", "OpenAI", "sk-new-key")
            .expect_err("invalid UTF-8 config must not be treated as empty");

        assert!(err.contains("Failed to read existing config"), "{err}");
        assert_eq!(fs::read(&config_path).unwrap(), vec![0xff, 0xfe, 0xfd]);
    }

    #[test]
    fn upsert_provider_api_key_config_writes_escaped_yaml_scalar() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        let api_key = "sk-quote\"and\\slash";

        let update = upsert_provider_api_key_config(&config_path, "openai", "OpenAI", api_key)
            .expect("new config should be written");

        assert_eq!(update, ProviderConfigUpdate::Saved);
        let config = fs::read_to_string(&config_path).unwrap();
        let parsed: YamlValue = serde_yaml::from_str(&config).unwrap();
        assert_eq!(parsed["openai_api_key"].as_str(), Some(api_key));
    }

    #[test]
    fn upsert_provider_api_key_config_preserves_existing_provider_key() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yaml");
        let original = "openai_api_key: \"sk-existing\"\n";
        fs::write(&config_path, original).unwrap();

        let update = upsert_provider_api_key_config(&config_path, "openai", "OpenAI", "sk-new")
            .expect("existing readable config should load");

        assert_eq!(update, ProviderConfigUpdate::AlreadyConfigured);
        assert_eq!(fs::read_to_string(&config_path).unwrap(), original);
    }
}
