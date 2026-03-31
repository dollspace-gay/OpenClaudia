use super::input::handle_user_questions;
use super::{AgentMode, ChatSession};
use openclaudia::tools;
use std::fs;

/// Handle entering plan mode. Creates plan file and sets up state.
pub fn handle_enter_plan_mode(chat_session: &mut ChatSession) -> String {
    let plans_dir = std::path::PathBuf::from(".openclaudia/plans");
    if let Err(e) = fs::create_dir_all(&plans_dir) {
        return format!("Failed to create plans directory: {}", e);
    }

    let plans_dir = std::fs::canonicalize(&plans_dir).unwrap_or(plans_dir);
    let plan_file = plans_dir.join(format!("{}.md", chat_session.id));

    if !plan_file.exists() {
        let header = format!(
            "# Implementation Plan\n\nSession: {}\nCreated: {}\n\n## Plan\n\n",
            chat_session.id,
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
        );
        if let Err(e) = fs::write(&plan_file, &header) {
            return format!("Failed to create plan file: {}", e);
        }
    }

    let plan_state = openclaudia::session::PlanModeState {
        active: true,
        plan_file: plan_file.clone(),
        allowed_prompts: Vec::new(),
    };

    chat_session.plan_mode = Some(plan_state);
    chat_session.mode = AgentMode::Plan;

    println!(
        "\n\x1b[1;33m>> Entered Plan Mode\x1b[0m\n\
         \x1b[90mWrite-access tools are now blocked.\n\
         Use write_file to write to: {}\n\
         Call exit_plan_mode when your plan is ready.\x1b[0m\n",
        plan_file.display()
    );

    format!(
        "Plan mode activated. Plan file: {}. \
         Only read-only tools, ask_user_question, and task are available. \
         Use write_file ONLY to write to the plan file at the path shown above. \
         Call exit_plan_mode when you are ready to present the plan for approval.",
        plan_file.display()
    )
}

/// Handle exiting plan mode. Reads plan file, shows to user for approval.
/// Returns (result_text, should_exit_plan_mode).
pub fn handle_exit_plan_mode(
    chat_session: &mut ChatSession,
    allowed_prompts_json: &str,
) -> (String, bool) {
    use std::io::{self, Write};

    let plan_state = match &chat_session.plan_mode {
        Some(state) if state.active => state.clone(),
        _ => {
            return ("Not currently in plan mode.".to_string(), false);
        }
    };

    let plan_content = match fs::read_to_string(&plan_state.plan_file) {
        Ok(content) => content,
        Err(e) => {
            return (
                format!(
                    "Failed to read plan file {}: {}",
                    plan_state.plan_file.display(),
                    e
                ),
                false,
            );
        }
    };

    println!("\n\x1b[1;36m{}\x1b[0m", "=".repeat(60));
    println!("\x1b[1;36m## Implementation Plan\x1b[0m\n");
    println!("{}", plan_content);
    println!("\x1b[1;36m{}\x1b[0m\n", "=".repeat(60));
    print!("\x1b[1;33mApprove? [y/n/edit]: \x1b[0m");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return ("Failed to read user input.".to_string(), false);
    }
    let input = input.trim().to_lowercase();

    match input.as_str() {
        "y" | "yes" => {
            let allowed_prompts = tools::parse_exit_plan_mode_prompts(allowed_prompts_json);

            chat_session.plan_mode = None;
            chat_session.mode = AgentMode::Build;
            chat_session.approved_plan = Some(plan_content.clone());

            println!(
                "\n\x1b[1;32m>> Plan Approved - Returning to Build Mode\x1b[0m\n\
                 \x1b[90mFull tool access restored. Plan injected as context.\x1b[0m\n"
            );

            chat_session.messages.push(serde_json::json!({
                "role": "system",
                "content": format!(
                    "[Approved Implementation Plan]\n\
                     The user has approved the following plan. Execute it step by step.\n\n{}\n\n{}",
                    plan_content,
                    if !allowed_prompts.is_empty() {
                        format!(
                            "Allowed operations:\n{}",
                            allowed_prompts
                                .iter()
                                .map(|p| format!("- {}: {}", p.tool, p.prompt))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    } else {
                        String::new()
                    }
                )
            }));

            (
                "Plan approved by user. Full tool access restored. Proceed with implementation according to the plan.".to_string(),
                true,
            )
        }
        "n" | "no" => {
            println!(
                "\n\x1b[1;31m>> Plan Rejected - Staying in Plan Mode\x1b[0m\n\
                 \x1b[90mRevise the plan and try again.\x1b[0m\n"
            );

            (
                "Plan rejected by user. You are still in plan mode. Please revise the plan based on user feedback and call exit_plan_mode again when ready.".to_string(),
                false,
            )
        }
        "edit" | "e" => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
            println!("\n\x1b[90mOpening plan in {}...\x1b[0m", editor);

            let edit_result = std::process::Command::new(&editor)
                .arg(&plan_state.plan_file)
                .status();

            match edit_result {
                Ok(status) if status.success() => {
                    let edited_content =
                        fs::read_to_string(&plan_state.plan_file).unwrap_or_default();

                    println!("\n\x1b[1;36m## Edited Plan\x1b[0m\n");
                    println!("{}", edited_content);
                    println!();
                    print!("\x1b[1;33mApprove edited plan? [y/n]: \x1b[0m");
                    io::stdout().flush().ok();

                    let mut input2 = String::new();
                    if io::stdin().read_line(&mut input2).is_err() {
                        return ("Failed to read user input.".to_string(), false);
                    }

                    if input2.trim().to_lowercase().starts_with('y') {
                        let allowed_prompts =
                            tools::parse_exit_plan_mode_prompts(allowed_prompts_json);

                        chat_session.plan_mode = None;
                        chat_session.mode = AgentMode::Build;
                        chat_session.approved_plan = Some(edited_content.clone());

                        println!("\n\x1b[1;32m>> Plan Approved - Returning to Build Mode\x1b[0m\n");

                        chat_session.messages.push(serde_json::json!({
                            "role": "system",
                            "content": format!(
                                "[Approved Implementation Plan (edited by user)]\n\
                                 The user has edited and approved the following plan. Execute it step by step.\n\n{}\n\n{}",
                                edited_content,
                                if !allowed_prompts.is_empty() {
                                    format!(
                                        "Allowed operations:\n{}",
                                        allowed_prompts
                                            .iter()
                                            .map(|p| format!("- {}: {}", p.tool, p.prompt))
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    )
                                } else {
                                    String::new()
                                }
                            )
                        }));

                        (
                            "Plan edited and approved by user. Full tool access restored. Proceed with implementation according to the edited plan.".to_string(),
                            true,
                        )
                    } else {
                        println!("\n\x1b[1;31m>> Plan Rejected - Staying in Plan Mode\x1b[0m\n");
                        (
                            "Edited plan rejected by user. Still in plan mode. Revise and try again.".to_string(),
                            false,
                        )
                    }
                }
                Ok(_) => (
                    "Editor exited with error. Plan unchanged. Still in plan mode.".to_string(),
                    false,
                ),
                Err(e) => {
                    println!("\x1b[31mFailed to open editor '{}': {}\x1b[0m", editor, e);
                    (
                        "Failed to open editor. Still in plan mode.".to_string(),
                        false,
                    )
                }
            }
        }
        _ => {
            println!("\x1b[90mUnrecognized input. Staying in plan mode.\x1b[0m");
            (
                "Unrecognized response. Still in plan mode. Call exit_plan_mode again when ready."
                    .to_string(),
                false,
            )
        }
    }
}

/// Check if a tool call is blocked by plan mode and return an error message if so.
pub fn check_plan_mode_restriction(
    chat_session: &ChatSession,
    tool_name: &str,
    tool_args: &str,
) -> Option<String> {
    let plan_state = match &chat_session.plan_mode {
        Some(state) if state.active => state,
        _ => return None,
    };

    let args: serde_json::Value =
        serde_json::from_str(tool_args).unwrap_or(serde_json::Value::Null);

    if openclaudia::session::is_tool_allowed_in_plan_mode(tool_name, &plan_state.plan_file, &args) {
        None
    } else {
        Some(format!(
            "Tool '{}' is not available in plan mode. \
             Only read-only tools (read_file, list_files, grep, web_fetch, web_search), \
             ask_user_question, and task are allowed. \
             You can use write_file ONLY to write to the plan file at: {}",
            tool_name,
            plan_state.plan_file.display()
        ))
    }
}

/// Process a tool result, checking for special markers (user_question, plan mode).
/// Returns the (possibly replaced) result content and whether it was a special marker.
pub fn process_tool_result_marker(
    chat_session: &mut ChatSession,
    tool_name: &str,
    result_content: &str,
) -> (String, bool) {
    if let Some(marker) = tools::check_tool_result_marker(result_content) {
        match marker.as_str() {
            tools::USER_QUESTION_MARKER => {
                if let Some(questions) = tools::parse_user_questions(result_content) {
                    let answers = handle_user_questions(&questions);
                    return (answers, true);
                }
            }
            tools::ENTER_PLAN_MODE_MARKER => {
                let msg = handle_enter_plan_mode(chat_session);
                return (msg, true);
            }
            tools::EXIT_PLAN_MODE_MARKER => {
                let (msg, _approved) = handle_exit_plan_mode(chat_session, result_content);
                return (msg, true);
            }
            _ => {}
        }
    }
    let _ = tool_name; // suppress unused warning
    (result_content.to_string(), false)
}
