/// Prompt user for permission to perform a sensitive operation
pub fn prompt_permission(
    operation: &str,
    details: &str,
    always_allowed: &mut std::collections::HashSet<String>,
) -> bool {
    use std::io::{self, Write};

    let key = format!("{operation}:{details}");

    if always_allowed.contains(&key) {
        return true;
    }

    if always_allowed.contains(&format!("!{key}")) {
        return false;
    }

    println!("\n=== Permission Required ===");
    println!("Operation: {operation}");
    println!("Details: {details}");
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
            always_allowed.insert(format!("!{key}"));
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
pub fn execute_shell_command_with_permission(
    cmd: &str,
    permissions: &mut std::collections::HashSet<String>,
) {
    let dangerous_patterns = ["rm -rf", "del /f", "format", "mkfs", "> /dev/", "sudo rm"];
    let is_dangerous = dangerous_patterns.iter().any(|p| cmd.contains(p));

    if is_dangerous && !prompt_permission("Dangerous Shell Command", cmd, permissions) {
        println!("Command blocked.\n");
        return;
    }

    execute_shell_command_internal(cmd);
}

/// Execute a shell command and print output
pub fn execute_shell_command_internal(cmd: &str) {
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
                    println!("(exit code: {code})");
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to execute command: {e}");
        }
    }
    println!();
}
