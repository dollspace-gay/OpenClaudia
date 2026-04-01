//! Hook merging utilities
//!
//! Functions for merging hook configurations from multiple sources,
//! deep-merging JSON settings files, and converting Claude Code hooks
//! to `OpenClaudia` format.

use crate::config::{Hook, HookEntry, HooksConfig};
use serde_json::Value;
use std::fs;
use std::path::Path;
use tracing::{debug, warn};

use super::claude_compat::{ClaudeCodeHook, ClaudeCodeSettings};
use super::HookEvent;

/// Merge two `HooksConfig` structs, with `other` taking precedence.
/// Entries from `other` with the same matcher as an entry in `base` replace the base entry.
#[must_use]
pub fn merge_hooks_config(base: HooksConfig, other: HooksConfig) -> HooksConfig {
    let mut merged = base;

    dedup_hook_entries(&mut merged.session_start, other.session_start);
    dedup_hook_entries(&mut merged.session_end, other.session_end);
    dedup_hook_entries(&mut merged.pre_tool_use, other.pre_tool_use);
    dedup_hook_entries(&mut merged.post_tool_use, other.post_tool_use);
    dedup_hook_entries(&mut merged.user_prompt_submit, other.user_prompt_submit);
    dedup_hook_entries(&mut merged.stop, other.stop);
    dedup_hook_entries(&mut merged.pre_adversary_review, other.pre_adversary_review);
    dedup_hook_entries(
        &mut merged.post_adversary_review,
        other.post_adversary_review,
    );
    dedup_hook_entries(&mut merged.vdd_conflict, other.vdd_conflict);
    dedup_hook_entries(&mut merged.vdd_converged, other.vdd_converged);

    merged
}

/// Extend `base` with entries from `other`, replacing any base entry whose
/// matcher matches an entry in `other` (later source wins).
fn dedup_hook_entries(base: &mut Vec<HookEntry>, other: Vec<HookEntry>) {
    for entry in other {
        // Remove existing entries with the same matcher
        base.retain(|existing| existing.matcher != entry.matcher);
        base.push(entry);
    }
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

/// Merge Claude Code hooks into `OpenClaudia` `HooksConfig`
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
