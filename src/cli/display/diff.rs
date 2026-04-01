//! Color diff rendering for file edits.

use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::ExecutableCommand;
use similar::{ChangeTag, TextDiff};
use std::io;

/// Render a word-level color diff between old and new text.
pub fn render_color_diff(path: &str, old_text: &str, new_text: &str) {
    let mut stdout = io::stdout();

    // Header
    let _ = stdout.execute(SetForegroundColor(Color::DarkGrey));
    let _ = stdout.execute(Print(format!("  ── {path} ")));
    let _ = stdout.execute(ResetColor);
    let _ = stdout.execute(Print("\n"));

    let diff = TextDiff::from_lines::<str>(old_text, new_text);

    for (idx, group) in diff.grouped_ops(3).iter().enumerate() {
        if idx > 0 {
            let _ = stdout.execute(SetForegroundColor(Color::DarkGrey));
            let _ = stdout.execute(Print("  ···\n"));
            let _ = stdout.execute(ResetColor);
        }

        for op in group {
            for change in diff.iter_inline_changes(op) {
                let (sign, line_color) = match change.tag() {
                    ChangeTag::Delete => ("-", Color::Red),
                    ChangeTag::Insert => ("+", Color::Green),
                    ChangeTag::Equal => (" ", Color::Reset),
                };

                // Line number gutter
                if let Some(line_no) = change.old_index().or_else(|| change.new_index()) {
                    let _ = stdout.execute(SetForegroundColor(Color::DarkGrey));
                    let _ = stdout.execute(Print(format!("  {:>4} ", line_no + 1)));
                }

                let _ = stdout.execute(SetForegroundColor(line_color));
                let _ = stdout.execute(Print(sign));
                let _ = stdout.execute(Print(" "));

                // Word-level highlighting within changed lines
                for (emphasized, value) in change.iter_strings_lossy() {
                    if emphasized {
                        match change.tag() {
                            ChangeTag::Delete => {
                                let _ = stdout.execute(SetBackgroundColor(Color::DarkRed));
                                let _ = stdout.execute(SetForegroundColor(Color::White));
                            }
                            ChangeTag::Insert => {
                                let _ = stdout.execute(SetBackgroundColor(Color::DarkGreen));
                                let _ = stdout.execute(SetForegroundColor(Color::White));
                            }
                            ChangeTag::Equal => {}
                        }
                        let _ = stdout.execute(Print(&value));
                        let _ = stdout.execute(ResetColor);
                        let _ = stdout.execute(SetForegroundColor(line_color));
                    } else {
                        let _ = stdout.execute(Print(&value));
                    }
                }

                let _ = stdout.execute(ResetColor);
                // Ensure newline if the change doesn't end with one
                if change.missing_newline() {
                    let _ = stdout.execute(Print("\n"));
                }
            }
        }
    }
}
