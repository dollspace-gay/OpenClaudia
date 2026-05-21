//! End-to-end tests for `tools::ToolControlSignal` parser +
//! `parse_user_questions` + `parse_exit_plan_mode_prompts` +
//! `check_tool_result_marker` legacy shim.
//!
//! Sprint 82 of the verification effort. These parsers
//! interpret the JSON `type` field embedded by control-flow
//! tools (`ask_user_question`, `enter_plan_mode`,
//! `exit_plan_mode`) so the dispatcher can flip session state.
//! Pure pattern-matching surface with NO integration coverage
//! prior to this sprint.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{
    check_tool_result_marker, parse_exit_plan_mode_prompts, parse_tool_control_signal,
    parse_user_questions, ToolControlSignal, ENTER_PLAN_MODE_MARKER, EXIT_PLAN_MODE_MARKER,
    USER_QUESTION_MARKER,
};
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Marker constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn marker_constants_match_documented_strings() {
    assert_eq!(USER_QUESTION_MARKER, "user_question");
    assert_eq!(ENTER_PLAN_MODE_MARKER, "enter_plan_mode");
    assert_eq!(EXIT_PLAN_MODE_MARKER, "exit_plan_mode");
}

#[test]
fn marker_constants_are_pairwise_distinct() {
    assert_ne!(USER_QUESTION_MARKER, ENTER_PLAN_MODE_MARKER);
    assert_ne!(ENTER_PLAN_MODE_MARKER, EXIT_PLAN_MODE_MARKER);
    assert_ne!(USER_QUESTION_MARKER, EXIT_PLAN_MODE_MARKER);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — ToolControlSignal::marker
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn signal_marker_returns_correct_string_per_variant() {
    assert_eq!(
        ToolControlSignal::UserQuestion.marker(),
        USER_QUESTION_MARKER
    );
    assert_eq!(
        ToolControlSignal::EnterPlanMode.marker(),
        ENTER_PLAN_MODE_MARKER
    );
    assert_eq!(
        ToolControlSignal::ExitPlanMode.marker(),
        EXIT_PLAN_MODE_MARKER
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — parse_tool_control_signal
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_signal_recognises_user_question_type() {
    let content = json!({"type": "user_question", "questions": []}).to_string();
    let outcome = parse_tool_control_signal(&content);
    assert!(matches!(outcome, Some(ToolControlSignal::UserQuestion)));
}

#[test]
fn parse_signal_recognises_enter_plan_mode_type() {
    let content = json!({"type": "enter_plan_mode"}).to_string();
    let outcome = parse_tool_control_signal(&content);
    assert!(matches!(outcome, Some(ToolControlSignal::EnterPlanMode)));
}

#[test]
fn parse_signal_recognises_exit_plan_mode_type() {
    let content = json!({"type": "exit_plan_mode", "allowed_prompts": []}).to_string();
    let outcome = parse_tool_control_signal(&content);
    assert!(matches!(outcome, Some(ToolControlSignal::ExitPlanMode)));
}

#[test]
fn parse_signal_returns_none_for_non_signal_tool_result() {
    let content = json!({"result": "some tool output"}).to_string();
    assert!(parse_tool_control_signal(&content).is_none());
}

#[test]
fn parse_signal_returns_none_for_unknown_type_marker() {
    let content = json!({"type": "totally-unknown-marker"}).to_string();
    assert!(parse_tool_control_signal(&content).is_none());
}

#[test]
fn parse_signal_returns_none_for_non_json_content() {
    assert!(parse_tool_control_signal("just plain text").is_none());
    assert!(parse_tool_control_signal("").is_none());
    assert!(parse_tool_control_signal("not { valid json").is_none());
}

#[test]
fn parse_signal_returns_none_when_type_field_is_non_string() {
    let content = json!({"type": 42}).to_string();
    assert!(parse_tool_control_signal(&content).is_none());
}

#[test]
fn parse_signal_is_case_sensitive_on_marker_string() {
    // Markers must match exactly; "User_Question" with
    // wrong case MUST NOT match.
    let content = json!({"type": "User_Question"}).to_string();
    assert!(parse_tool_control_signal(&content).is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — check_tool_result_marker (legacy back-compat shim)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn check_marker_returns_marker_string_for_recognised_types() {
    let content = json!({"type": "user_question", "questions": []}).to_string();
    assert_eq!(
        check_tool_result_marker(&content).as_deref(),
        Some("user_question")
    );

    let content = json!({"type": "enter_plan_mode"}).to_string();
    assert_eq!(
        check_tool_result_marker(&content).as_deref(),
        Some("enter_plan_mode")
    );

    let content = json!({"type": "exit_plan_mode"}).to_string();
    assert_eq!(
        check_tool_result_marker(&content).as_deref(),
        Some("exit_plan_mode")
    );
}

#[test]
fn check_marker_returns_none_for_non_signal_content() {
    assert!(check_tool_result_marker("{}").is_none());
    assert!(check_tool_result_marker("plain text").is_none());
}

#[test]
fn check_marker_and_parse_signal_agree_on_every_input() {
    // PINS DOCUMENTED CONTRACT: check_tool_result_marker is
    // a back-compat shim around parse_tool_control_signal —
    // they MUST agree (None ↔ None, Some(m) ↔ Some(signal
    // with that marker)).
    let cases = vec![
        json!({"type": "user_question"}).to_string(),
        json!({"type": "enter_plan_mode"}).to_string(),
        json!({"type": "exit_plan_mode"}).to_string(),
        json!({"type": "unknown"}).to_string(),
        json!({}).to_string(),
        "garbage".to_string(),
    ];
    for content in cases {
        let typed = parse_tool_control_signal(&content);
        let legacy = check_tool_result_marker(&content);
        match (typed, legacy) {
            (None, None) => {}
            (Some(sig), Some(marker)) => assert_eq!(sig.marker(), marker),
            (a, b) => panic!("MUST agree for {content:?}; got typed={a:?} vs legacy={b:?}"),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — parse_user_questions
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_user_questions_returns_array_when_present() {
    let content = json!({
        "type": "user_question",
        "questions": [
            {"question": "What color?", "options": ["red", "blue"]},
            {"question": "Are you sure?", "options": ["yes", "no"]}
        ]
    })
    .to_string();
    let questions = parse_user_questions(&content).expect("MUST find questions");
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0]["question"], "What color?");
    assert_eq!(questions[1]["question"], "Are you sure?");
}

#[test]
fn parse_user_questions_returns_some_empty_for_explicit_empty_array() {
    let content = json!({"questions": []}).to_string();
    let questions = parse_user_questions(&content).expect("MUST find empty array");
    assert!(questions.is_empty());
}

#[test]
fn parse_user_questions_returns_none_when_questions_field_absent() {
    let content = json!({"some_other_field": "value"}).to_string();
    assert!(parse_user_questions(&content).is_none());
}

#[test]
fn parse_user_questions_returns_none_when_questions_field_is_non_array() {
    let content = json!({"questions": "not-an-array"}).to_string();
    assert!(parse_user_questions(&content).is_none());
}

#[test]
fn parse_user_questions_returns_none_for_non_json_content() {
    assert!(parse_user_questions("garbage").is_none());
    assert!(parse_user_questions("").is_none());
}

#[test]
fn parse_user_questions_preserves_arbitrary_question_shape() {
    // Documented contract: questions array is returned as
    // Vec<Value> without validation — the dispatcher
    // interprets the shape.
    let content = json!({
        "questions": [
            "string-question",
            42,
            {"nested": "object"},
            [1, 2, 3]
        ]
    })
    .to_string();
    let questions = parse_user_questions(&content).expect("Some");
    assert_eq!(questions.len(), 4);
    assert_eq!(questions[0], json!("string-question"));
    assert_eq!(questions[1], json!(42));
    assert_eq!(questions[3], json!([1, 2, 3]));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — parse_exit_plan_mode_prompts
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_exit_plan_prompts_returns_vec_when_present() {
    let content = json!({
        "type": "exit_plan_mode",
        "allowed_prompts": [
            {"tool": "bash", "prompt": "ls -la"},
            {"tool": "read_file", "prompt": "src/main.rs"}
        ]
    })
    .to_string();
    let prompts = parse_exit_plan_mode_prompts(&content);
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0].tool, "bash");
    assert_eq!(prompts[0].prompt, "ls -la");
    assert_eq!(prompts[1].tool, "read_file");
    assert_eq!(prompts[1].prompt, "src/main.rs");
}

#[test]
fn parse_exit_plan_prompts_returns_empty_when_field_absent() {
    let content = json!({"type": "exit_plan_mode"}).to_string();
    let prompts = parse_exit_plan_mode_prompts(&content);
    assert!(prompts.is_empty());
}

#[test]
fn parse_exit_plan_prompts_skips_items_missing_required_fields() {
    // Items must have BOTH "tool" + "prompt"; one missing → skip.
    let content = json!({
        "allowed_prompts": [
            {"tool": "bash", "prompt": "ls"},
            {"tool": "no_prompt_field"},
            {"prompt": "no_tool_field"},
            {"unrelated": "value"},
        ]
    })
    .to_string();
    let prompts = parse_exit_plan_mode_prompts(&content);
    assert_eq!(
        prompts.len(),
        1,
        "MUST keep only well-formed items; got {} prompts",
        prompts.len()
    );
    assert_eq!(prompts[0].tool, "bash");
}

#[test]
fn parse_exit_plan_prompts_returns_empty_for_non_json() {
    let prompts = parse_exit_plan_mode_prompts("garbage");
    assert!(prompts.is_empty());
    let prompts = parse_exit_plan_mode_prompts("");
    assert!(prompts.is_empty());
}

#[test]
fn parse_exit_plan_prompts_returns_empty_when_allowed_prompts_is_non_array() {
    let content = json!({"allowed_prompts": "not-an-array"}).to_string();
    let prompts = parse_exit_plan_mode_prompts(&content);
    assert!(prompts.is_empty());
}
