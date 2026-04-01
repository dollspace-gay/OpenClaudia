use std::fs;

/// Display structured questions to the user and collect answers.
/// Returns a JSON string mapping question text to selected answer(s).
pub fn handle_user_questions(questions: &[serde_json::Value]) -> String {
    use std::io::{self, Write};

    let mut answers: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for q in questions {
        let question_text = q.get("question").and_then(|v| v.as_str()).unwrap_or("?");
        let header = q.get("header").and_then(|v| v.as_str()).unwrap_or("");
        let options = q
            .get("options")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let multi_select = q
            .get("multi_select")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Display the question
        println!(
            "\n\x1b[1;36m?\x1b[0m {}  \x1b[90m[{}]\x1b[0m",
            question_text, header
        );

        // Display options
        for (i, opt) in options.iter().enumerate() {
            let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or("?");
            let desc = opt
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            println!(
                "  \x1b[1m{}.\x1b[0m {} \x1b[90m- {}\x1b[0m",
                i + 1,
                label,
                desc
            );
        }
        // Always append "Other" option
        let other_num = options.len() + 1;
        println!(
            "  \x1b[1m{}.\x1b[0m Other \x1b[90m(type your answer)\x1b[0m",
            other_num
        );

        if multi_select {
            print!("\x1b[36m> \x1b[0m\x1b[90m(comma-separated numbers) \x1b[0m");
        } else {
            print!("\x1b[36m> \x1b[0m");
        }
        io::stdout().flush().ok();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            answers.insert(
                question_text.to_string(),
                serde_json::Value::String("(no input)".to_string()),
            );
            continue;
        }
        let input = input.trim();

        if multi_select {
            let mut selected: Vec<serde_json::Value> = Vec::new();
            for part in input.split(',') {
                let part = part.trim();
                if let Ok(num) = part.parse::<usize>() {
                    if num >= 1 && num <= options.len() {
                        if let Some(opt) = options.get(num - 1) {
                            let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                            selected.push(serde_json::Value::String(label.to_string()));
                        }
                    } else if num == other_num {
                        print!("  \x1b[36mYour answer: \x1b[0m");
                        io::stdout().flush().ok();
                        let mut other_input = String::new();
                        if io::stdin().read_line(&mut other_input).is_ok() {
                            selected
                                .push(serde_json::Value::String(other_input.trim().to_string()));
                        }
                    }
                }
            }
            answers.insert(
                question_text.to_string(),
                serde_json::Value::Array(selected),
            );
        } else if let Ok(num) = input.parse::<usize>() {
            if num >= 1 && num <= options.len() {
                if let Some(opt) = options.get(num - 1) {
                    let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                    answers.insert(
                        question_text.to_string(),
                        serde_json::Value::String(label.to_string()),
                    );
                }
            } else if num == other_num {
                print!("  \x1b[36mYour answer: \x1b[0m");
                io::stdout().flush().ok();
                let mut other_input = String::new();
                if io::stdin().read_line(&mut other_input).is_ok() {
                    answers.insert(
                        question_text.to_string(),
                        serde_json::Value::String(other_input.trim().to_string()),
                    );
                }
            } else {
                answers.insert(
                    question_text.to_string(),
                    serde_json::Value::String(input.to_string()),
                );
            }
        } else {
            answers.insert(
                question_text.to_string(),
                serde_json::Value::String(input.to_string()),
            );
        }
    }

    serde_json::Value::Object(answers).to_string()
}

/// Open external editor for composing a message
pub fn open_external_editor() -> Option<String> {
    use std::process::Command;

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            #[cfg(windows)]
            {
                "notepad".to_string()
            }
            #[cfg(not(windows))]
            {
                "vim".to_string()
            }
        });

    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join(format!("openclaudia_{}.txt", uuid::Uuid::new_v4()));

    println!("\nOpening {}...", editor);

    #[cfg(windows)]
    let status = Command::new("cmd")
        .args(["/C", &editor, temp_file.to_str().unwrap_or("")])
        .status();

    #[cfg(not(windows))]
    let status = Command::new(&editor).arg(&temp_file).status();

    match status {
        Ok(s) if s.success() => match fs::read_to_string(&temp_file) {
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
        },
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
pub fn expand_file_references(input: &str) -> String {
    use regex::Regex;

    let re = Regex::new(r#"@"([^"]+)"|@(\S+)"#).unwrap();

    let mut result = input.to_string();
    let mut replacements = Vec::new();

    for cap in re.captures_iter(input) {
        let full_match = cap.get(0).unwrap().as_str();
        let path = cap.get(1).or(cap.get(2)).unwrap().as_str();

        match fs::read_to_string(path) {
            Ok(content) => {
                let file_context =
                    format!("\n<file path=\"{}\">\n{}\n</file>\n", path, content.trim());
                replacements.push((full_match.to_string(), file_context));
            }
            Err(e) => {
                eprintln!("Warning: Could not read {}: {}", path, e);
                let error_context = format!("[File not found or unreadable: {} ({})]", path, e);
                replacements.push((full_match.to_string(), error_context));
            }
        }
    }

    for (from, to) in replacements {
        result = result.replace(&from, &to);
    }

    result
}
