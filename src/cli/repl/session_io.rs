use super::{get_data_dir, ChatSession};
use openclaudia::memory;
use openclaudia::tools::safe_truncate;
use std::fmt::Write;
use std::fs;

/// Estimate tokens in a chat session (rough: ~4 chars per token)
pub fn estimate_session_tokens(session: &ChatSession) -> usize {
    session
        .messages
        .iter()
        .map(|msg| {
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            content.len() / 4 + 4 // content tokens + overhead
        })
        .sum()
}

/// Compact a chat session by summarizing older messages
pub fn compact_chat_session(session: &mut ChatSession) -> (usize, usize) {
    let before_tokens = estimate_session_tokens(session);
    let msg_count = session.messages.len();

    if msg_count <= 6 {
        println!("\nSession too short to compact ({msg_count} messages).\n");
        return (before_tokens, before_tokens);
    }

    let preserve_count = 4;
    let to_summarize = msg_count - preserve_count;

    let mut summary_parts = Vec::new();
    for msg in session.messages.iter().take(to_summarize) {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        let preview = if content.len() > 200 {
            format!("{}...", safe_truncate(content, 197))
        } else {
            content.to_string()
        };

        summary_parts.push(format!("[{role}]: {preview}"));
    }

    let summary = format!(
        "[CONVERSATION SUMMARY - {} messages compacted]\n{}",
        to_summarize,
        summary_parts.join("\n")
    );

    let preserved: Vec<_> = session
        .messages
        .iter()
        .skip(to_summarize)
        .cloned()
        .collect();

    session.messages.clear();
    session.messages.push(serde_json::json!({
        "role": "system",
        "content": summary
    }));
    session.messages.extend(preserved);
    session.touch();

    let after_tokens = estimate_session_tokens(session);
    (before_tokens, after_tokens)
}

/// Export chat session to markdown file
pub fn export_chat_session(session: &ChatSession) {
    let exports_dir = get_data_dir().join("exports");
    if let Err(e) = fs::create_dir_all(&exports_dir) {
        eprintln!("\nFailed to create exports directory: {e}\n");
        return;
    }

    let filename = format!("chat_{}.md", session.created_at.format("%Y%m%d_%H%M%S"));
    let path = exports_dir.join(&filename);

    let mut content = String::new();
    let _ = writeln!(content, "# {}\n", session.title);
    let _ = writeln!(
        content,
        "**Date:** {}  ",
        session.created_at.format("%Y-%m-%d %H:%M UTC")
    );
    let _ = writeln!(content, "**Model:** {}  ", session.model);
    let _ = writeln!(content, "**Provider:** {}  \n", session.provider);
    content.push_str("---\n\n");

    for msg in &session.messages {
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown");
        let msg_content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        match role {
            "user" => {
                content.push_str("## User\n\n");
                content.push_str(msg_content);
                content.push_str("\n\n");
            }
            "assistant" => {
                content.push_str("## Assistant\n\n");
                content.push_str(msg_content);
                content.push_str("\n\n");
            }
            _ => {
                let _ = writeln!(content, "## {role}\n");
                content.push_str(msg_content);
                content.push_str("\n\n");
            }
        }
    }

    match fs::write(&path, content) {
        Ok(()) => println!("\nExported to: {}\n", path.display()),
        Err(e) => eprintln!("\nFailed to export: {e}\n"),
    }
}

/// Save session summary to short-term memory for continuity across restarts
pub fn save_session_to_short_term_memory(
    session: &ChatSession,
    memory_db: Option<&memory::MemoryDb>,
) {
    let Some(db) = memory_db else {
        return;
    };

    let mut summary_parts = Vec::new();
    summary_parts.push(format!("Session: {}", session.title));

    let mut user_requests = Vec::new();
    let mut last_assistant_summary = String::new();

    for msg in &session.messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

        if role == "user" && !content.is_empty() {
            if let Some(first_line) = content.lines().next() {
                let truncated = if first_line.len() > 100 {
                    format!("{}...", safe_truncate(first_line, 100))
                } else {
                    first_line.to_string()
                };
                user_requests.push(truncated);
            }
        } else if role == "assistant" && !content.is_empty() {
            last_assistant_summary = content.lines().take(3).collect::<Vec<_>>().join(" ");
            if last_assistant_summary.len() > 200 {
                last_assistant_summary =
                    format!("{}...", safe_truncate(&last_assistant_summary, 200));
            }
        }
    }

    if !user_requests.is_empty() {
        summary_parts.push(format!("User requests: {}", user_requests.join("; ")));
    }
    if !last_assistant_summary.is_empty() {
        summary_parts.push(format!("Last action: {last_assistant_summary}"));
    }

    let summary = summary_parts.join("\n");

    let files_modified = db
        .get_session_files_modified(&session.id)
        .unwrap_or_default();
    let issues_worked = db.get_session_issues(&session.id).unwrap_or_default();

    let started_at = session.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
    match db.save_session_summary(
        &session.id,
        &summary,
        &files_modified,
        &issues_worked,
        &started_at,
    ) {
        Ok(_) => {
            tracing::debug!("Session saved to short-term memory");
        }
        Err(e) => {
            tracing::warn!("Failed to save session summary: {}", e);
        }
    }

    if let Ok((sessions, activities)) = db.cleanup_expired_short_term() {
        if sessions > 0 || activities > 0 {
            tracing::debug!(
                "Cleaned up {} expired sessions, {} activities",
                sessions,
                activities
            );
        }
    }
}
