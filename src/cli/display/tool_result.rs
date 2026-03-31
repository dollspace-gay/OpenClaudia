//! Centralized tool result display with per-tool formatting.

use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::ExecutableCommand;
use std::io;

use super::diff;

/// Display a tool result in the terminal with per-tool formatting.
pub fn display_tool_result(tool_name: &str, content: &str, is_error: bool) {
    let mut stdout = io::stdout();

    if content.is_empty() {
        return;
    }

    // Errors always get full display in red
    if is_error {
        let _ = stdout.execute(SetForegroundColor(Color::Red));
        let lines: Vec<&str> = content.lines().collect();
        let max = 30.min(lines.len());
        for line in &lines[..max] {
            let _ = stdout.execute(Print(format!("    {}\n", line)));
        }
        if lines.len() > max {
            let _ = stdout.execute(Print(format!(
                "    ... ({} more lines)\n",
                lines.len() - max
            )));
        }
        let _ = stdout.execute(ResetColor);
        return;
    }

    // Check for embedded diff data
    if let Some(diff_data) = extract_diff_block(content) {
        diff::render_color_diff(&diff_data.path, &diff_data.old_text, &diff_data.new_text);
        // Also show the success message (first line before the diff block)
        if let Some(msg) = content.split("@@DIFF_START@@").next() {
            let msg = msg.trim();
            if !msg.is_empty() {
                let _ = stdout.execute(SetForegroundColor(Color::Green));
                let _ = stdout.execute(Print(format!("    {}\n", msg)));
                let _ = stdout.execute(ResetColor);
            }
        }
        return;
    }

    // Per-tool display strategies
    let max_lines = match tool_name {
        "bash" | "bash_output" => 25,
        "read_file" => 15,
        "grep" | "glob" | "list_files" => 15,
        "write_file" => 3,
        _ => 20,
    };

    let color = match tool_name {
        "write_file" | "edit_file" => Color::Green,
        "bash" | "bash_output" => Color::White,
        _ => Color::DarkGrey,
    };

    let _ = stdout.execute(SetForegroundColor(color));
    let lines: Vec<&str> = content.lines().collect();
    let show = max_lines.min(lines.len());
    for line in &lines[..show] {
        let _ = stdout.execute(Print(format!("    {}\n", line)));
    }
    if lines.len() > show {
        let _ = stdout.execute(SetForegroundColor(Color::DarkGrey));
        let _ = stdout.execute(Print(format!(
            "    ... ({} more lines)\n",
            lines.len() - show
        )));
    }
    let _ = stdout.execute(ResetColor);
}

struct DiffBlock {
    path: String,
    old_text: String,
    new_text: String,
}

fn extract_diff_block(content: &str) -> Option<DiffBlock> {
    let start = content.find("@@DIFF_START@@")?;
    let end = content.find("@@DIFF_END@@")?;
    let json_str = content[start + "@@DIFF_START@@".len()..end].trim();
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    Some(DiffBlock {
        path: v["path"].as_str()?.to_string(),
        old_text: v["old"].as_str()?.to_string(),
        new_text: v["new"].as_str()?.to_string(),
    })
}
