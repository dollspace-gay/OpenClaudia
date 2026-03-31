use serde_json::{json, Value};
use std::collections::HashMap;

use super::{ENTER_PLAN_MODE_MARKER, EXIT_PLAN_MODE_MARKER};

/// Execute the enter_plan_mode tool.
/// Returns a special marker that the main loop intercepts to activate plan mode.
pub(crate) fn execute_enter_plan_mode() -> (String, bool) {
    let result = json!({
        "type": ENTER_PLAN_MODE_MARKER
    });
    (result.to_string(), false)
}

/// Execute the exit_plan_mode tool.
/// Returns a special marker that the main loop intercepts to show the plan for approval.
pub(crate) fn execute_exit_plan_mode(args: &HashMap<String, Value>) -> (String, bool) {
    let allowed_prompts = args
        .get("allowed_prompts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Validate allowed_prompts structure
    for (i, prompt) in allowed_prompts.iter().enumerate() {
        if prompt.get("tool").and_then(|v| v.as_str()).is_none() {
            return (format!("allowed_prompts[{}] missing 'tool' field", i), true);
        }
        if prompt.get("prompt").and_then(|v| v.as_str()).is_none() {
            return (
                format!("allowed_prompts[{}] missing 'prompt' field", i),
                true,
            );
        }
    }

    let result = json!({
        "type": EXIT_PLAN_MODE_MARKER,
        "allowed_prompts": allowed_prompts
    });
    (result.to_string(), false)
}
