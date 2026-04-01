use std::fs;

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

    let mut config_content = if config_path.exists() {
        fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };

    let provider_section =
        format!("\n# {provider_name} configuration\n{provider_id}_api_key: \"{api_key}\"\n");

    let key_pattern = format!("{provider_id}_api_key:");
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
            Err(e) => eprintln!("\nFailed to save config: {e}\n"),
        }
    }
}
