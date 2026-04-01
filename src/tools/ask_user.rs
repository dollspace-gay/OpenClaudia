use serde_json::{json, Value};
use std::collections::HashMap;

use super::USER_QUESTION_MARKER;

/// Execute the `ask_user_question` tool.
/// Returns a special JSON result that signals the main loop to collect user input.
pub fn execute_ask_user_question(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(questions) = args.get("questions").and_then(|v| v.as_array()) else {
        return ("Missing 'questions' argument".to_string(), true);
    };

    if questions.is_empty() || questions.len() > 4 {
        return ("Must provide 1-4 questions".to_string(), true);
    }

    // Validate each question
    for (i, q) in questions.iter().enumerate() {
        let question_text = q.get("question").and_then(|v| v.as_str());
        let header = q.get("header").and_then(|v| v.as_str());
        let options = q.get("options").and_then(|v| v.as_array());

        if question_text.is_none() {
            return (format!("Question {i} missing 'question' field"), true);
        }
        if header.is_none() {
            return (format!("Question {i} missing 'header' field"), true);
        }
        if let Some(h) = header {
            if h.len() > 12 {
                return (
                    format!("Question {i} header '{h}' exceeds 12 character limit"),
                    true,
                );
            }
        }
        match options {
            None => return (format!("Question {i} missing 'options' field"), true),
            Some(opts) => {
                if opts.len() < 2 || opts.len() > 4 {
                    return (
                        format!("Question {} must have 2-4 options, got {}", i, opts.len()),
                        true,
                    );
                }
                for (j, opt) in opts.iter().enumerate() {
                    if opt.get("label").and_then(|v| v.as_str()).is_none() {
                        return (format!("Question {i} option {j} missing 'label'"), true);
                    }
                    if opt.get("description").and_then(|v| v.as_str()).is_none() {
                        return (
                            format!("Question {i} option {j} missing 'description'"),
                            true,
                        );
                    }
                }
            }
        }
    }

    // Return the special marker result for the main loop to intercept
    let result = json!({
        "type": USER_QUESTION_MARKER,
        "questions": questions
    });

    (result.to_string(), false)
}
