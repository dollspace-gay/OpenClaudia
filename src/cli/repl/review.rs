use std::fs;
use std::path::Path;

/// Review uncommitted git changes or compare against a branch
pub fn review_git_changes(args: &str) {
    use std::process::Command;

    let git_check = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output();

    if git_check.is_err() || !git_check.unwrap().status.success() {
        println!("\nNot a git repository.\n");
        return;
    }

    println!();

    if args.is_empty() {
        println!("=== Git Status ===\n");
        let status = Command::new("git").args(["status", "--short"]).output();

        match status {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.is_empty() {
                    println!("No changes detected.\n");
                    return;
                }
                println!("{stdout}");
            }
            Err(e) => {
                eprintln!("Failed to run git status: {e}\n");
                return;
            }
        }

        println!("=== Uncommitted Changes ===\n");
        let diff = Command::new("git").args(["diff", "HEAD"]).output();

        match diff {
            Ok(output) => {
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
            Err(e) => eprintln!("Failed to run git diff: {e}\n"),
        }
    } else {
        let branch = args.trim();
        println!("=== Comparing against '{branch}' ===\n");

        let branch_check = Command::new("git")
            .args(["rev-parse", "--verify", branch])
            .output();

        if branch_check.is_err() || !branch_check.unwrap().status.success() {
            eprintln!("Branch '{branch}' not found.\n");
            return;
        }

        println!("Commits ahead of {branch}:\n");
        let log = Command::new("git")
            .args(["log", "--oneline", &format!("{branch}..HEAD")])
            .output();

        match log {
            Ok(output) => {
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
            Err(e) => eprintln!("Failed to run git log: {e}\n"),
        }

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
                    println!("{stdout}");
                }
            }
            Err(e) => eprintln!("Failed to run git diff --stat: {e}\n"),
        }
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
