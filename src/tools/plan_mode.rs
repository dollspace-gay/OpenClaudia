use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::HashMap;

use super::{ENTER_PLAN_MODE_MARKER, EXIT_PLAN_MODE_MARKER};

thread_local! {
    /// Thread-local flag set by the subagent runner while a `task` tool
    /// invocation is in flight. Plan-mode is a top-level operator concept and
    /// is not allowed to be entered from inside an agent task — Claude Code
    /// rejects `enter_plan_mode` whenever `context.agentId` is set. The
    /// harness mirrors that by toggling [`AgentContextGuard`] for the duration
    /// of any subagent task (crosslink #620).
    static IN_AGENT_TASK: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that marks the current thread as executing inside a subagent
/// task (crosslink #620).
///
/// While alive, [`execute_enter_plan_mode`] returns the
/// "cannot be entered from inside an agent task" error instead of producing
/// the activation marker. Nested guards are reference-counted-by-bool: only
/// the outermost guard clears the flag on drop, which matches the harness's
/// behaviour of strictly serialising agent-task execution.
///
/// The subagent runtime is the intended user of this guard; tests construct
/// it directly to exercise the gate without spinning up a full subagent.
pub struct AgentContextGuard {
    /// True iff *this* guard was the one that flipped the flag from
    /// `false` -> `true`. Only that guard clears on drop, making nested
    /// `AgentContextGuard` instances no-ops with respect to the outer
    /// guard's lifetime.
    owns: bool,
}

impl AgentContextGuard {
    /// Mark this thread as being inside a subagent task. The flag is cleared
    /// when the returned guard is dropped.
    #[must_use]
    pub fn enter() -> Self {
        let owns = IN_AGENT_TASK.with(|f| {
            if f.get() {
                false
            } else {
                f.set(true);
                true
            }
        });
        Self { owns }
    }
}

impl Drop for AgentContextGuard {
    fn drop(&mut self) {
        if self.owns {
            IN_AGENT_TASK.with(|f| f.set(false));
        }
    }
}

/// True iff the current thread is executing inside an agent task — used by
/// [`execute_enter_plan_mode`] to refuse activation (crosslink #620).
#[must_use]
pub fn in_agent_task() -> bool {
    IN_AGENT_TASK.with(Cell::get)
}

/// Execute the `enter_plan_mode` tool.
///
/// Returns a special marker that the main loop intercepts to activate plan
/// mode, **unless** the current thread is inside a subagent task — in which
/// case the call is refused with `is_error=true` per crosslink #620 (Claude
/// Code rejects when `context.agentId` is set).
pub fn execute_enter_plan_mode() -> (String, bool) {
    if in_agent_task() {
        return (
            "plan mode cannot be entered from inside an agent task".to_string(),
            true,
        );
    }
    let result = json!({
        "type": ENTER_PLAN_MODE_MARKER
    });
    (result.to_string(), false)
}

/// Execute the `exit_plan_mode` tool.
/// Returns a special marker that the main loop intercepts to show the plan for approval.
///
/// Perimeter defense: `allowed_prompts`, when present, MUST be a JSON array.
/// Earlier versions used `as_array().cloned().unwrap_or_default()` which
/// silently swallowed type errors — passing `allowed_prompts: "Bash"` would
/// be treated identically to an absent field, masking model mistakes
/// (crosslink #933). Now the wrong container shape is a hard error.
pub fn execute_exit_plan_mode(args: &HashMap<String, Value>) -> (String, bool) {
    let allowed_prompts: Vec<Value> = match args.get("allowed_prompts") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(arr)) => arr.clone(),
        Some(other) => {
            let kind = match other {
                Value::String(_) => "string",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::Object(_) => "object",
                Value::Array(_) | Value::Null => unreachable!(),
            };
            return (
                format!("allowed_prompts must be an array, got {kind}"),
                true,
            );
        }
    };

    // Validate allowed_prompts structure
    for (i, prompt) in allowed_prompts.iter().enumerate() {
        if !prompt.is_object() {
            return (
                format!("allowed_prompts[{i}] must be an object with 'tool' and 'prompt' fields"),
                true,
            );
        }
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

    /// #933: when `allowed_prompts` is present but is not an array, the tool
    /// rejects the call rather than silently treating it as empty. The
    /// previous behaviour (`as_array().cloned().unwrap_or_default()`) masked
    /// model mistakes by collapsing "wrong type" and "absent" into the same
    /// successful empty-array path.
    #[test]
    fn exit_plan_mode_rejects_non_array_allowed_prompts_933() {
        for bad in [json!("Bash"), json!(42), json!({"tool": "Bash"}), json!(true)] {
            let mut args = HashMap::new();
            args.insert("allowed_prompts".to_string(), bad.clone());
            let (msg, is_err) = execute_exit_plan_mode(&args);
            assert!(is_err, "non-array value {bad} must be rejected; got: {msg}");
            assert!(
                msg.contains("allowed_prompts must be an array"),
                "error must name the shape violation; got: {msg}"
            );
        }
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

    /// #618 fix: the EXIT marker still does not carry a `prePlanMode` payload
    /// (that is a session-level concern handled by the REPL via
    /// `PlanModeState::previous_mode`). The tool layer remains stateless —
    /// regression test pinning the contract.
    #[test]
    fn exit_plan_mode_marker_payload_has_no_pre_plan_mode_field_618() {
        let args = HashMap::new();
        let (output, _) = execute_exit_plan_mode(&args);
        let v: Value = from_str(&output).unwrap();
        assert!(
            v.get("prePlanMode").is_none(),
            "#618: tool-level marker stays stateless — session layer carries the snapshot"
        );
    }

    // ─── #620: agent-context guard for enter_plan_mode ─────────────────────────

    /// Outside a subagent task `enter_plan_mode` succeeds. Sanity test so a
    /// regression in the gate is observable as the *positive* case flipping
    /// to an error (crosslink #620).
    #[test]
    fn enter_plan_mode_outside_agent_task_succeeds_620() {
        // Defensive: ensure no other test on this thread left the flag set.
        IN_AGENT_TASK.with(|f| f.set(false));
        let (out, is_err) = execute_enter_plan_mode();
        assert!(!is_err, "outside an agent task, enter must succeed");
        let v: Value = from_str(&out).unwrap();
        assert_eq!(v["type"].as_str(), Some(ENTER_PLAN_MODE_MARKER));
    }

    /// Inside a subagent task `enter_plan_mode` returns an error matching the
    /// CC message family (crosslink #620). The guard is RAII so we exercise
    /// it via [`AgentContextGuard::enter`].
    #[test]
    fn enter_plan_mode_inside_agent_task_is_refused_620() {
        let _guard = AgentContextGuard::enter();
        let (msg, is_err) = execute_enter_plan_mode();
        assert!(is_err, "must produce is_error=true inside an agent task");
        assert!(
            msg.contains("plan mode cannot be entered from inside an agent task"),
            "error must name the gap; got: {msg}"
        );
    }

    /// The guard is RAII: after it drops, `enter_plan_mode` succeeds again.
    /// Pins the lifecycle so a future change can't silently turn the gate
    /// into a one-way flip (crosslink #620).
    #[test]
    fn enter_plan_mode_recovers_after_agent_guard_drops_620() {
        IN_AGENT_TASK.with(|f| f.set(false));
        {
            let _g = AgentContextGuard::enter();
            let (_, is_err) = execute_enter_plan_mode();
            assert!(is_err, "must refuse while guard alive");
        }
        let (_, is_err) = execute_enter_plan_mode();
        assert!(!is_err, "must succeed after guard drops");
    }

    /// Nested guards: only the *outermost* clears the flag on drop, so the
    /// inner drop must NOT prematurely unlock enter (crosslink #620).
    #[test]
    fn enter_plan_mode_nested_agent_guard_does_not_leak_620() {
        IN_AGENT_TASK.with(|f| f.set(false));
        let outer = AgentContextGuard::enter();
        {
            let _inner = AgentContextGuard::enter();
            assert!(in_agent_task(), "nested guard must still see flag set");
        }
        // Inner dropped, but outer is still alive: flag must stay set.
        assert!(
            in_agent_task(),
            "inner-guard drop must not clear the flag while outer is alive"
        );
        let (_, is_err) = execute_enter_plan_mode();
        assert!(is_err, "outer guard still alive: enter must remain refused");
        drop(outer);
        assert!(!in_agent_task(), "outer drop must clear the flag");
    }
}
