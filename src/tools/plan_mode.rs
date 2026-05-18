use serde_json::{json, Value};
use std::collections::HashMap;

use super::{ENTER_PLAN_MODE_MARKER, EXIT_PLAN_MODE_MARKER};

/// Execute the `enter_plan_mode` tool.
/// Returns a special marker that the main loop intercepts to activate plan mode.
pub fn execute_enter_plan_mode() -> (String, bool) {
    let result = json!({
        "type": ENTER_PLAN_MODE_MARKER
    });
    (result.to_string(), false)
}

/// Execute the `exit_plan_mode` tool.
/// Returns a special marker that the main loop intercepts to show the plan for approval.
pub fn execute_exit_plan_mode(args: &HashMap<String, Value>) -> (String, bool) {
    let allowed_prompts = args
        .get("allowed_prompts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Validate allowed_prompts structure
    for (i, prompt) in allowed_prompts.iter().enumerate() {
        if prompt.get("tool").and_then(|v| v.as_str()).is_none() {
            return (format!("allowed_prompts[{i}] missing 'tool' field"), true);
        }
        if prompt.get("prompt").and_then(|v| v.as_str()).is_none() {
            return (format!("allowed_prompts[{i}] missing 'prompt' field"), true);
        }
    }

    let result = json!({
        "type": EXIT_PLAN_MODE_MARKER,
        "allowed_prompts": allowed_prompts
    });
    (result.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::from_str;

    // ─── Spec §1: Plan-mode enforcement — entering blocks write/edit/bash ──────

    /// Contract: `enter_plan_mode` returns a JSON marker (not an error) and the
    /// `is_error` flag is false.  The REPL uses this marker to flip mode.
    #[test]
    fn enter_plan_mode_returns_marker_not_error() {
        let (output, is_err) = execute_enter_plan_mode();
        assert!(!is_err, "enter_plan_mode must not set is_error");
        let v: Value = from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            v["type"].as_str(),
            Some(ENTER_PLAN_MODE_MARKER),
            "output 'type' must equal ENTER_PLAN_MODE_MARKER"
        );
    }

    /// Contract: calling `enter_plan_mode` again (no args) still returns the
    /// same marker — the tool is stateless; the REPL layer is responsible for
    /// the no-op-if-already-in-plan-mode behaviour.
    #[test]
    fn enter_plan_mode_is_idempotent_at_tool_level() {
        let (first, _) = execute_enter_plan_mode();
        let (second, _) = execute_enter_plan_mode();
        let v1: Value = from_str(&first).unwrap();
        let v2: Value = from_str(&second).unwrap();
        assert_eq!(
            v1["type"], v2["type"],
            "repeated calls must produce the same marker"
        );
    }

    // ─── Spec §2: Plan-mode exit — restores permissions ────────────────────────

    /// Contract: `exit_plan_mode` with no args returns the EXIT marker (not error).
    #[test]
    fn exit_plan_mode_returns_marker_not_error() {
        let args = HashMap::new();
        let (output, is_err) = execute_exit_plan_mode(&args);
        assert!(!is_err, "exit_plan_mode must not set is_error on success");
        let v: Value = from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            v["type"].as_str(),
            Some(EXIT_PLAN_MODE_MARKER),
            "output 'type' must equal EXIT_PLAN_MODE_MARKER"
        );
    }

    /// Contract: `exit_plan_mode` propagates `allowed_prompts` into the marker
    /// payload so the REPL can surface them.
    #[test]
    fn exit_plan_mode_includes_allowed_prompts_in_marker() {
        let mut args = HashMap::new();
        args.insert(
            "allowed_prompts".to_string(),
            json!([{"tool": "Bash", "prompt": "run tests"}]),
        );
        let (output, is_err) = execute_exit_plan_mode(&args);
        assert!(!is_err);
        let v: Value = from_str(&output).unwrap();
        let prompts = v["allowed_prompts"].as_array().expect("array");
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["tool"].as_str(), Some("Bash"));
    }

    /// Contract: an `allowed_prompts` entry missing the `tool` field returns an
    /// error response (`is_error` = true).
    #[test]
    fn exit_plan_mode_rejects_allowed_prompt_missing_tool() {
        let mut args = HashMap::new();
        args.insert(
            "allowed_prompts".to_string(),
            json!([{"prompt": "do something"}]),
        );
        let (msg, is_err) = execute_exit_plan_mode(&args);
        assert!(is_err, "missing 'tool' field must produce is_error=true");
        assert!(
            msg.contains("missing 'tool'"),
            "error message must name the missing field; got: {msg}"
        );
    }

    /// Contract: an `allowed_prompts` entry missing the `prompt` field also
    /// returns `is_error=true`.
    #[test]
    fn exit_plan_mode_rejects_allowed_prompt_missing_prompt_field() {
        let mut args = HashMap::new();
        args.insert("allowed_prompts".to_string(), json!([{"tool": "Bash"}]));
        let (msg, is_err) = execute_exit_plan_mode(&args);
        assert!(is_err);
        assert!(
            msg.contains("missing 'prompt'"),
            "error message must name the missing field; got: {msg}"
        );
    }

    /// Contract: absent `allowed_prompts` key behaves the same as an empty
    /// array — the marker is returned with an empty `allowed_prompts` list.
    #[test]
    fn exit_plan_mode_absent_allowed_prompts_defaults_to_empty() {
        let args = HashMap::new();
        let (output, is_err) = execute_exit_plan_mode(&args);
        assert!(!is_err);
        let v: Value = from_str(&output).unwrap();
        let prompts = v["allowed_prompts"].as_array().expect("must be array");
        assert!(prompts.is_empty(), "absent key → empty array in marker");
    }

    /// Pin gap #618: OC has no `prePlanMode` snapshot.  This test documents the
    /// CURRENT behaviour — exit always produces the EXIT marker without restoring
    /// any pre-plan context (no `prePlanMode` field in the payload).
    #[test]
    fn exit_plan_mode_has_no_pre_plan_mode_snapshot_gap618() {
        let args = HashMap::new();
        let (output, _) = execute_exit_plan_mode(&args);
        let v: Value = from_str(&output).unwrap();
        assert!(
            v.get("prePlanMode").is_none(),
            "gap #618: OC marker must NOT contain prePlanMode field (not implemented)"
        );
    }
}
