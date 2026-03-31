//! Hook merging utilities
//!
//! Functions for merging hook configurations from multiple sources,
//! deep-merging JSON settings files, and converting Claude Code hooks
//! to OpenClaudia format.

use crate::config::{Hook, HookEntry, HooksConfig};
use serde_json::Value;
use std::fs;
use std::path::Path;
use tracing::{debug, warn};

use super::claude_compat::{ClaudeCodeHook, ClaudeCodeSettings};
use super::HookEvent;

/// Merge two HooksConfig structs, with `other` taking precedence
pub fn merge_hooks_config(base: HooksConfig, other: HooksConfig) -> HooksConfig {
    let mut merged = base;

    merged.session_start.extend(other.session_start);
    merged.session_end.extend(other.session_end);
    merged.pre_tool_use.extend(other.pre_tool_use);
    merged.post_tool_use.extend(other.post_tool_use);
    merged.user_prompt_submit.extend(other.user_prompt_submit);
    merged.stop.extend(other.stop);
    merged
        .pre_adversary_review
        .extend(other.pre_adversary_review);
    merged
        .post_adversary_review
        .extend(other.post_adversary_review);
    merged.vdd_conflict.extend(other.vdd_conflict);
    merged.vdd_converged.extend(other.vdd_converged);

    merged
}

/// Merge a settings file into the accumulator using deep merge semantics.
///
/// - Objects merge recursively
/// - Arrays concatenate
/// - Scalars from the new file override
pub(crate) fn merge_settings_file(target: &mut Value, path: &Path) {
    match fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<Value>(&content) {
            Ok(new_settings) => {
                deep_merge(target, &new_settings);
            }
            Err(e) => {
                warn!(path = ?path, error = %e, "Failed to parse settings file");
            }
        },
        Err(e) => {
            debug!(path = ?path, error = %e, "Could not read settings file");
        }
    }
}

/// Deep merge two JSON values.
///
/// - Objects: recursively merge keys
/// - Arrays: concatenate
/// - Scalars: `source` overrides `target`
pub(crate) fn deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_val) in source_map {
                let entry = target_map.entry(key.clone()).or_insert(Value::Null);
                deep_merge(entry, source_val);
            }
        }
        (Value::Array(target_arr), Value::Array(source_arr)) => {
            target_arr.extend(source_arr.iter().cloned());
        }
        (target, source) => {
            *target = source.clone();
        }
    }
}

/// Merge Claude Code hooks into OpenClaudia HooksConfig
pub(crate) fn merge_claude_hooks(config: &mut HooksConfig, settings: &ClaudeCodeSettings) {
    for (event_name, entries) in &settings.hooks {
        let Some(event) = HookEvent::from_claude_code_name(event_name) else {
            warn!(event = %event_name, "Unknown Claude Code hook event, skipping");
            continue;
        };

        // Convert Claude Code entries to OpenClaudia format
        let converted_entries: Vec<HookEntry> = entries
            .iter()
            .map(|entry| {
                let hooks: Vec<Hook> = entry
                    .hooks
                    .iter()
                    .map(|h| match h {
                        ClaudeCodeHook::Command { command, timeout } => Hook::Command {
                            command: command.clone(),
                            timeout: timeout.unwrap_or(60),
                        },
                    })
                    .collect();

                HookEntry {
                    matcher: entry.matcher.clone().filter(|m| !m.is_empty()),
                    hooks,
                }
            })
            .collect();

        // Append to the appropriate event list
        match event {
            HookEvent::SessionStart => config.session_start.extend(converted_entries),
            HookEvent::SessionEnd => config.session_end.extend(converted_entries),
            HookEvent::PreToolUse => config.pre_tool_use.extend(converted_entries),
            HookEvent::PostToolUse => config.post_tool_use.extend(converted_entries),
            HookEvent::UserPromptSubmit => config.user_prompt_submit.extend(converted_entries),
            HookEvent::Stop => config.stop.extend(converted_entries),
            // Other events not yet supported in HooksConfig
            _ => {
                debug!(event = ?event, "Event not yet supported in config, skipping");
            }
        }
    }
}
