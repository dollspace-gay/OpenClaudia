//! Grounded loop data shapes that sit above provider adapters.
//!
//! Providers should only translate wire formats. This module describes the
//! packet the core loop should assemble before provider calls: authoritative
//! ledger entries first, lower-authority navigation aids later.

use crate::evidence::Denial;
use crate::ledger::{ObsId, ObservationIndexEntry};
use crate::ledger::{ObservationKind, RealityLedger};
use crate::task_spec::TaskSpec;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

pub const DEFAULT_GROUNDING_INDEX_LIMIT: usize = 64;
const MAX_RENDERED_TASK_CHARS: usize = 500;
const MAX_NAV_IDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingPriority {
    RealityLedger = 0,
    TaskSpec = 1,
    CurrentDiff = 2,
    VerifierResults = 3,
    Summaries = 4,
    Memory = 5,
    ProviderChatHistory = 6,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundedPromptPacket {
    pub task: TaskSpec,
    pub ledger_index: Vec<ObservationIndexEntry>,
    pub current_diff: Option<ObsId>,
    pub verifier_results: Vec<ObsId>,
    pub summaries: Vec<ObsId>,
    pub memory: Vec<String>,
    pub provider_chat_history: Vec<serde_json::Value>,
}

impl GroundedPromptPacket {
    #[must_use]
    pub fn new(task: TaskSpec, ledger_index: Vec<ObservationIndexEntry>) -> Self {
        Self {
            task,
            ledger_index,
            current_diff: None,
            verifier_results: Vec::new(),
            summaries: Vec::new(),
            memory: Vec::new(),
            provider_chat_history: Vec::new(),
        }
    }
}

pub fn build_prompt_packet(
    ledger: &RealityLedger,
    task_obs: ObsId,
    index_limit: usize,
    provider_chat_history: Vec<serde_json::Value>,
) -> Result<GroundedPromptPacket, Denial> {
    let task = TaskSpec::from_user_observation(ledger, task_obs)?;
    let mut packet = GroundedPromptPacket::new(task, ledger.observation_index(index_limit));
    packet.provider_chat_history = provider_chat_history;

    let observations = ledger.observations_chronological();
    packet.current_diff = observations
        .iter()
        .rev()
        .find(|obs| {
            matches!(obs.kind, ObservationKind::DiffObserved { .. }) && !ledger.is_stale(obs.id)
        })
        .map(|obs| obs.id);
    packet.verifier_results = observations
        .iter()
        .filter(|obs| {
            matches!(obs.kind, ObservationKind::Verification { .. }) && !ledger.is_stale(obs.id)
        })
        .rev()
        .take(MAX_NAV_IDS)
        .map(|obs| obs.id)
        .collect::<Vec<_>>();
    packet.verifier_results.reverse();
    packet.summaries = observations
        .iter()
        .filter(|obs| matches!(obs.kind, ObservationKind::Summary { .. }))
        .rev()
        .take(MAX_NAV_IDS)
        .map(|obs| obs.id)
        .collect::<Vec<_>>();
    packet.summaries.reverse();

    Ok(packet)
}

#[must_use]
pub fn render_grounding_system_message(packet: &GroundedPromptPacket) -> String {
    let mut out = String::new();
    out.push_str("Grounding hierarchy for this turn:\n");
    out.push_str(
        "Reality Ledger > TaskSpec > Current Diff > Verifier Results > Summaries > Memory > Provider Chat History\n\n",
    );
    let _ = writeln!(
        out,
        "TaskSpec [{}]: {}",
        packet.task.source_obs,
        truncate_for_prompt(&packet.task.content, MAX_RENDERED_TASK_CHARS)
    );
    if let Some(diff_id) = packet.current_diff {
        let _ = writeln!(out, "Current diff observation: [{diff_id}]");
    }
    if !packet.verifier_results.is_empty() {
        let ids = packet
            .verifier_results
            .iter()
            .map(|id| format!("[{id}]"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "Verifier observations: {ids}");
    }
    out.push_str("\nReality ledger index:\n");
    for entry in &packet.ledger_index {
        let stale = if entry.stale { " stale" } else { "" };
        let _ = writeln!(
            out,
            "- [{}] {:?}{stale}: {}",
            entry.id, entry.authority, entry.label
        );
    }
    out.push_str(
        "\nRules: Use memory, summaries, and provider chat history only as navigation aids. Treat facts as grounded only when backed by non-stale, non-summary ledger observations. Cite observation IDs for file, command, diff, and verification claims in final answers.\n",
    );
    out
}

fn truncate_for_prompt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Authority;

    #[test]
    fn prompt_packet_orders_authoritative_context_before_navigation() {
        let mut ledger = RealityLedger::new();
        let task = ledger
            .observe_user_task("Audit the binary commands.")
            .expect("task");
        let read = ledger
            .observe_file_read("src/main.rs", "fn main() {}", 1, 1, "fn main() {}")
            .expect("read");
        let diff = ledger
            .observe_diff(
                vec!["src/main.rs".to_string()],
                "diff --git a/src/main.rs b/src/main.rs",
            )
            .expect("diff");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = ledger
            .append(
                Authority::ModelSummary,
                ObservationKind::Summary {
                    text: "navigational only".to_string(),
                    source_obs: vec![read],
                },
            )
            .expect("summary");

        let packet = build_prompt_packet(&ledger, task, DEFAULT_GROUNDING_INDEX_LIMIT, Vec::new())
            .expect("packet");
        assert_eq!(packet.task.source_obs, task);
        assert_eq!(packet.current_diff, Some(diff));
        assert_eq!(packet.verifier_results, vec![verification]);
        assert_eq!(packet.summaries, vec![summary]);
        assert!(packet
            .ledger_index
            .iter()
            .any(|entry| entry.id == read && entry.stale));
    }

    #[test]
    fn grounding_message_states_summary_and_memory_are_not_evidence() {
        let mut ledger = RealityLedger::new();
        let task = ledger.observe_user_task("Run cargo test.").expect("task");
        let packet = build_prompt_packet(&ledger, task, DEFAULT_GROUNDING_INDEX_LIMIT, Vec::new())
            .expect("packet");

        let rendered = render_grounding_system_message(&packet);
        assert!(rendered.contains("Reality Ledger > TaskSpec"));
        assert!(rendered.contains(&format!("TaskSpec [{task}]")));
        assert!(rendered.contains("navigation aids"));
        assert!(rendered.contains("Cite observation IDs"));
    }
}
