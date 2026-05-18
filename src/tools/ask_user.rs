use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::USER_QUESTION_MARKER;

/// Claude Code-compatible chip width for the question `header` field.
/// Matches `ASK_USER_QUESTION_TOOL_CHIP_WIDTH` in
/// `claude-code/tools/AskUserQuestionTool/prompt.ts`.
const HEADER_CHIP_WIDTH: usize = 12;

/// Execute the `ask_user_question` tool.
/// Returns a special JSON result that signals the main loop to collect user input.
pub fn execute_ask_user_question(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(questions) = args.get("questions").and_then(|v| v.as_array()) else {
        return ("Missing 'questions' argument".to_string(), true);
    };

    if questions.is_empty() || questions.len() > 4 {
        return ("Must provide 1-4 questions".to_string(), true);
    }

    // Validate each question shape + enforce CC-compatible uniqueness:
    // question texts unique across the array, option labels unique
    // within each question. See
    // claude-code/tools/AskUserQuestionTool/AskUserQuestionTool.tsx
    // (UNIQUENESS_REFINE).
    let mut seen_question_texts: HashSet<&str> = HashSet::new();
    for (i, q) in questions.iter().enumerate() {
        let Some(question_text) = q.get("question").and_then(|v| v.as_str()) else {
            return (format!("Question {i} missing 'question' field"), true);
        };
        if !seen_question_texts.insert(question_text) {
            return (
                format!("Question texts must be unique; '{question_text}' appears more than once"),
                true,
            );
        }

        let Some(header) = q.get("header").and_then(|v| v.as_str()) else {
            return (format!("Question {i} missing 'header' field"), true);
        };
        // CC uses `.length`, which for ASCII matches byte count. For
        // multi-byte UTF-8 we use chars().count() so a header like
        // "日本語" (3 chars, 9 bytes) fits the same way users expect in CC.
        if header.chars().count() > HEADER_CHIP_WIDTH {
            return (
                format!(
                    "Question {i} header '{header}' exceeds {HEADER_CHIP_WIDTH} character limit"
                ),
                true,
            );
        }

        // `multiSelect` is CC's name; `multi_select` is OC's original
        // camelCase-impaired spelling. Accept either for back-compat.
        // Both must be booleans when present.
        for key in ["multiSelect", "multi_select"] {
            if let Some(v) = q.get(key) {
                if !v.is_boolean() {
                    return (format!("Question {i} '{key}' must be a boolean"), true);
                }
            }
        }

        let Some(opts) = q.get("options").and_then(|v| v.as_array()) else {
            return (format!("Question {i} missing 'options' field"), true);
        };
        if opts.len() < 2 || opts.len() > 4 {
            return (
                format!("Question {} must have 2-4 options, got {}", i, opts.len()),
                true,
            );
        }
        let mut seen_labels: HashSet<&str> = HashSet::new();
        for (j, opt) in opts.iter().enumerate() {
            let Some(label) = opt.get("label").and_then(|v| v.as_str()) else {
                return (format!("Question {i} option {j} missing 'label'"), true);
            };
            if !seen_labels.insert(label) {
                return (
                    format!(
                        "Question {i} option labels must be unique; '{label}' appears more than once"
                    ),
                    true,
                );
            }
            if opt.get("description").and_then(|v| v.as_str()).is_none() {
                return (
                    format!("Question {i} option {j} missing 'description'"),
                    true,
                );
            }
            // `preview` is optional (CC parity). When present it must
            // be a string — fail loudly rather than silently dropping it.
            if let Some(v) = opt.get("preview") {
                if !v.is_string() {
                    return (
                        format!("Question {i} option {j} 'preview' must be a string"),
                        true,
                    );
                }
            }
        }
    }

    // Normalize: copy every question, making sure `multiSelect` is the
    // canonical output key so downstream renderers see one spelling.
    let normalized: Vec<Value> = questions
        .iter()
        .map(|q| {
            let mut out = q.clone();
            if let Some(obj) = out.as_object_mut() {
                if !obj.contains_key("multiSelect") {
                    if let Some(legacy) = obj.remove("multi_select") {
                        obj.insert("multiSelect".to_string(), legacy);
                    }
                }
            }
            out
        })
        .collect();

    let result = json!({
        "type": USER_QUESTION_MARKER,
        "questions": normalized,
    });

    (result.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_args(questions: Value) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("questions".to_string(), questions);
        m
    }

    #[test]
    fn rejects_duplicate_question_text() {
        let args = make_args(json!([
            {
                "question": "Which library?",
                "header": "Library",
                "options": [
                    {"label": "A", "description": "a"},
                    {"label": "B", "description": "b"},
                ]
            },
            {
                "question": "Which library?",
                "header": "Library",
                "options": [
                    {"label": "C", "description": "c"},
                    {"label": "D", "description": "d"},
                ]
            },
        ]));
        let (msg, is_err) = execute_ask_user_question(&args);
        assert!(is_err, "duplicate text should error");
        assert!(msg.contains("unique"));
    }

    #[test]
    fn rejects_duplicate_option_labels() {
        let args = make_args(json!([{
            "question": "Pick one",
            "header": "Pick",
            "options": [
                {"label": "Same", "description": "x"},
                {"label": "Same", "description": "y"},
            ]
        }]));
        let (msg, is_err) = execute_ask_user_question(&args);
        assert!(is_err);
        assert!(msg.contains("unique"));
    }

    #[test]
    fn accepts_multi_select_and_preview() {
        let args = make_args(json!([{
            "question": "Pick",
            "header": "Pick",
            "multiSelect": true,
            "options": [
                {"label": "A", "description": "a", "preview": "```\nexample\n```"},
                {"label": "B", "description": "b"},
            ]
        }]));
        let (msg, is_err) = execute_ask_user_question(&args);
        assert!(
            !is_err,
            "valid multi_select + preview should succeed: {msg}"
        );
        // Canonical output uses `multiSelect`.
        assert!(msg.contains("\"multiSelect\":true"));
    }

    #[test]
    fn accepts_legacy_multi_select_spelling() {
        let args = make_args(json!([{
            "question": "Pick",
            "header": "Pick",
            "multi_select": true,
            "options": [
                {"label": "A", "description": "a"},
                {"label": "B", "description": "b"},
            ]
        }]));
        let (msg, is_err) = execute_ask_user_question(&args);
        assert!(!is_err);
        // Legacy spelling is rewritten to the canonical one.
        assert!(msg.contains("\"multiSelect\":true"));
        assert!(!msg.contains("multi_select"));
    }

    #[test]
    fn rejects_non_string_preview() {
        let args = make_args(json!([{
            "question": "Pick",
            "header": "Pick",
            "options": [
                {"label": "A", "description": "a", "preview": 42},
                {"label": "B", "description": "b"},
            ]
        }]));
        let (_, is_err) = execute_ask_user_question(&args);
        assert!(is_err);
    }
}
