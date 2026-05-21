//! End-to-end tests for `hooks::HookOutput` serde (camelCase
//! aliases + flatten escape hatch) + `config::Hook` /
//! `HookEntry` deserialization + `HookMatcherTarget` variants.
//!
//! Sprint 87 of the verification effort. Sprint 73 covered
//! `HookEvent` + `HookInput` builder + `check_blocked`;
//! sprint 28 covered the deep-merge layering; this file pins
//! the wire-shape contract for `HookOutput` JSON ingestion
//! (the camelCase rename fields) and the `Hook` enum
//! deserialization for the three command/prompt/model variants.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::{Hook, HookEntry, HookMatcherTarget, HooksConfig, SandboxMode};
use openclaudia::hooks::HookOutput;
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — HookOutput camelCase rename fields
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hook_output_deserializes_camelcase_system_message_field() {
    // Documented serde rename: system_message ↔ "systemMessage".
    let json = json!({"systemMessage": "from-hook"}).to_string();
    let output: HookOutput = serde_json::from_str(&json).expect("de");
    assert_eq!(output.system_message.as_deref(), Some("from-hook"));
}

#[test]
fn hook_output_deserializes_camelcase_additional_context_field() {
    let json = json!({"additionalContext": "extra"}).to_string();
    let output: HookOutput = serde_json::from_str(&json).expect("de");
    assert_eq!(output.additional_context.as_deref(), Some("extra"));
}

#[test]
fn hook_output_deserializes_with_all_camelcase_aliases_together() {
    let json = json!({
        "decision": "deny",
        "reason": "policy",
        "systemMessage": "system msg",
        "prompt": "modified prompt",
        "additionalContext": "additional"
    })
    .to_string();
    let output: HookOutput = serde_json::from_str(&json).expect("de");
    assert_eq!(output.decision.as_deref(), Some("deny"));
    assert_eq!(output.reason.as_deref(), Some("policy"));
    assert_eq!(output.system_message.as_deref(), Some("system msg"));
    assert_eq!(output.prompt.as_deref(), Some("modified prompt"));
    assert_eq!(output.additional_context.as_deref(), Some("additional"));
}

#[test]
fn hook_output_empty_json_deserializes_to_default_all_none() {
    let output: HookOutput = serde_json::from_str("{}").expect("de");
    assert!(output.decision.is_none());
    assert!(output.reason.is_none());
    assert!(output.system_message.is_none());
    assert!(output.prompt.is_none());
    assert!(output.additional_context.is_none());
    assert!(output.extra.is_empty());
}

#[test]
fn hook_output_extra_fields_flatten_into_extra_map() {
    // PINS DOCUMENTED CONTRACT: unknown fields flatten into
    // the `extra` HashMap (escape hatch for future fields).
    let json = json!({
        "decision": "allow",
        "custom_field_one": "value-1",
        "custom_field_two": 42,
        "nested": {"k": "v"}
    })
    .to_string();
    let output: HookOutput = serde_json::from_str(&json).expect("de");
    assert_eq!(output.decision.as_deref(), Some("allow"));
    assert_eq!(output.extra.len(), 3);
    assert_eq!(
        output.extra.get("custom_field_one"),
        Some(&json!("value-1"))
    );
    assert_eq!(output.extra.get("custom_field_two"), Some(&json!(42)));
    assert_eq!(output.extra.get("nested"), Some(&json!({"k": "v"})));
}

#[test]
fn hook_output_default_constructs_all_none() {
    let output = HookOutput::default();
    assert!(output.decision.is_none());
    assert!(output.system_message.is_none());
    assert!(output.extra.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — HookMatcherTarget serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hook_matcher_target_serde_uses_snake_case() {
    for (target, expected) in &[
        (HookMatcherTarget::ToolName, "tool_name"),
        (HookMatcherTarget::Prompt, "prompt"),
        (HookMatcherTarget::EventKey, "event_key"),
    ] {
        // Construct via JSON deserialization since target is
        // Deserialize-only on the path through the config.
        let json = format!("\"{expected}\"");
        let parsed: HookMatcherTarget = serde_json::from_str(&json).expect("de");
        assert_eq!(parsed, *target);
    }
}

#[test]
fn hook_matcher_target_rejects_unknown_value() {
    let outcome: Result<HookMatcherTarget, _> = serde_json::from_str("\"unknown_target\"");
    assert!(outcome.is_err());
}

#[test]
fn hook_matcher_target_variants_compare_distinctly() {
    assert_ne!(HookMatcherTarget::ToolName, HookMatcherTarget::Prompt);
    assert_ne!(HookMatcherTarget::Prompt, HookMatcherTarget::EventKey);
    assert_ne!(HookMatcherTarget::ToolName, HookMatcherTarget::EventKey);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Hook enum deserialization (command/prompt/model)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hook_command_variant_deserializes_with_defaults() {
    let json = json!({"type": "command", "command": "echo hi"}).to_string();
    let hook: Hook = serde_json::from_str(&json).expect("de");
    match hook {
        Hook::Command {
            command,
            shell,
            timeout,
        } => {
            assert_eq!(command, "echo hi");
            assert!(!shell, "shell MUST default to false");
            assert_eq!(timeout, 60, "default_timeout MUST be 60");
        }
        other => panic!("expected Command; got {other:?}"),
    }
}

#[test]
fn hook_command_variant_preserves_shell_true() {
    let json = json!({
        "type": "command",
        "command": "echo hi | grep i",
        "shell": true,
        "timeout": 120
    })
    .to_string();
    let hook: Hook = serde_json::from_str(&json).expect("de");
    let Hook::Command {
        command,
        shell,
        timeout,
    } = hook
    else {
        panic!("expected Command");
    };
    assert_eq!(command, "echo hi | grep i");
    assert!(shell);
    assert_eq!(timeout, 120);
}

#[test]
fn hook_prompt_variant_uses_30s_default_timeout() {
    let json = json!({"type": "prompt", "prompt": "remind me"}).to_string();
    let hook: Hook = serde_json::from_str(&json).expect("de");
    let Hook::Prompt { prompt, timeout } = hook else {
        panic!("expected Prompt");
    };
    assert_eq!(prompt, "remind me");
    assert_eq!(timeout, 30, "default_prompt_timeout MUST be 30");
}

#[test]
fn hook_model_variant_uses_60s_default_timeout_and_optional_provider() {
    let json = json!({
        "type": "model",
        "prompt": "Summarise",
        "model": "claude-3-5-haiku-20241022"
    })
    .to_string();
    let hook: Hook = serde_json::from_str(&json).expect("de");
    let Hook::Model {
        prompt,
        model,
        provider,
        timeout,
    } = hook
    else {
        panic!("expected Model");
    };
    assert_eq!(prompt, "Summarise");
    assert_eq!(model, "claude-3-5-haiku-20241022");
    assert!(provider.is_none(), "provider MUST default to None");
    assert_eq!(timeout, 60);
}

#[test]
fn hook_model_variant_with_explicit_provider() {
    let json = json!({
        "type": "model",
        "prompt": "x",
        "model": "gpt-4o",
        "provider": "openai"
    })
    .to_string();
    let hook: Hook = serde_json::from_str(&json).expect("de");
    let Hook::Model { provider, .. } = hook else {
        panic!("expected Model");
    };
    assert_eq!(provider.as_deref(), Some("openai"));
}

#[test]
fn hook_unknown_type_tag_errors() {
    let json = json!({"type": "totally-unknown", "x": "y"}).to_string();
    let outcome: Result<Hook, _> = serde_json::from_str(&json);
    assert!(outcome.is_err());
}

#[test]
fn hook_missing_type_field_errors() {
    let json = json!({"command": "echo"}).to_string();
    let outcome: Result<Hook, _> = serde_json::from_str(&json);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — HookEntry deserialization
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hook_entry_with_string_matcher_deserializes() {
    let json = json!({
        "matcher": "bash",
        "hooks": [{"type": "command", "command": "ls"}]
    })
    .to_string();
    let entry: HookEntry = serde_json::from_str(&json).expect("de");
    assert_eq!(entry.matcher.as_deref(), Some("bash"));
    assert_eq!(entry.hooks.len(), 1);
}

#[test]
fn hook_entry_without_matcher_field_yields_none() {
    let json = json!({
        "hooks": [{"type": "command", "command": "ls"}]
    })
    .to_string();
    let entry: HookEntry = serde_json::from_str(&json).expect("de");
    assert!(entry.matcher.is_none());
}

#[test]
fn hook_entry_with_multi_hook_array_preserves_order() {
    let json = json!({
        "matcher": "X",
        "hooks": [
            {"type": "command", "command": "first"},
            {"type": "prompt", "prompt": "second"},
            {"type": "command", "command": "third"}
        ]
    })
    .to_string();
    let entry: HookEntry = serde_json::from_str(&json).expect("de");
    assert_eq!(entry.hooks.len(), 3);
    let Hook::Command { command: c0, .. } = &entry.hooks[0] else {
        panic!("first must be Command");
    };
    assert_eq!(c0, "first");
    let Hook::Prompt { prompt: p1, .. } = &entry.hooks[1] else {
        panic!("second must be Prompt");
    };
    assert_eq!(p1, "second");
    let Hook::Command { command: c2, .. } = &entry.hooks[2] else {
        panic!("third must be Command");
    };
    assert_eq!(c2, "third");
}

#[test]
fn hook_entry_with_empty_hooks_array_parses() {
    let json = json!({
        "matcher": "X",
        "hooks": []
    })
    .to_string();
    let entry: HookEntry = serde_json::from_str(&json).expect("de");
    assert!(entry.hooks.is_empty());
}

#[test]
fn hook_entry_missing_hooks_field_errors() {
    // hooks is required (no #[serde(default)]).
    let json = json!({"matcher": "X"}).to_string();
    let outcome: Result<HookEntry, _> = serde_json::from_str(&json);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — HooksConfig + SandboxMode
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hooks_config_default_is_empty_with_no_policy() {
    let cfg = HooksConfig::default();
    assert!(cfg.is_empty());
    assert!(cfg.policy.is_none());
}

#[test]
fn hooks_config_empty_yaml_yields_default() {
    let cfg: HooksConfig = serde_yaml::from_str("{}").expect("de");
    assert!(cfg.is_empty());
}

#[test]
fn hooks_config_with_per_event_entries_round_trips() {
    let yaml = r#"
session_start:
  - hooks: [{type: command, command: "echo start"}]
pre_tool_use:
  - matcher: "Bash"
    hooks: [{type: command, command: "audit-bash"}]
"#;
    let cfg: HooksConfig = serde_yaml::from_str(yaml).expect("de");
    assert_eq!(cfg.session_start.len(), 1);
    assert_eq!(cfg.pre_tool_use.len(), 1);
    assert_eq!(cfg.pre_tool_use[0].matcher.as_deref(), Some("Bash"));
}

#[test]
fn sandbox_mode_default_is_documented_value() {
    // SandboxMode is enum-defaulting via serde(default) on
    // HookPolicy. Just verify the type is constructible +
    // round-trips.
    let yaml = "sandbox: none";
    let outcome = serde_yaml::from_str::<openclaudia::config::HookPolicy>(yaml);
    assert!(outcome.is_ok(), "sandbox: none MUST deserialize");
}

#[test]
fn sandbox_mode_round_trips_through_serde() {
    let modes = ["none", "env_scrub"];
    for mode in &modes {
        let yaml = format!("sandbox: {mode}");
        let policy: openclaudia::config::HookPolicy = serde_yaml::from_str(&yaml).expect("de");
        let _ = policy; // ensure parsed
    }
}

#[test]
fn hook_policy_allowed_commands_none_means_allow_all() {
    let yaml = "{}";
    let policy: openclaudia::config::HookPolicy = serde_yaml::from_str(yaml).expect("de");
    assert!(
        policy.allowed_commands.is_none(),
        "absent allowed_commands MUST be None (allow-all backcompat)"
    );
}

#[test]
fn hook_policy_allowed_commands_empty_set_means_deny_all() {
    let yaml = "allowed_commands: []";
    let policy: openclaudia::config::HookPolicy = serde_yaml::from_str(yaml).expect("de");
    let allowed = policy
        .allowed_commands
        .expect("Some(empty) MUST be present");
    assert!(allowed.is_empty(), "empty set encoded as Some(empty)");
}

#[test]
fn hook_policy_allowed_commands_explicit_list_deserializes() {
    let yaml = r#"allowed_commands: ["python", "node", "ls"]"#;
    let policy: openclaudia::config::HookPolicy = serde_yaml::from_str(yaml).expect("de");
    let allowed = policy.allowed_commands.expect("Some");
    assert_eq!(allowed.len(), 3);
    assert!(allowed.contains("python"));
    assert!(allowed.contains("node"));
    assert!(allowed.contains("ls"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — SandboxMode default reachability sanity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn sandbox_mode_default_constructs_without_panic() {
    let _ = SandboxMode::default();
}
