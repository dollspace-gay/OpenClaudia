//! Grounded loop data shapes that sit above provider adapters.
//!
//! Providers should only translate wire formats. This module describes the
//! packet the core loop should assemble before provider calls: authoritative
//! ledger entries first, lower-authority navigation aids later.

use crate::ledger::{ObsId, ObservationIndexEntry};
use crate::task_spec::TaskSpec;
use serde::{Deserialize, Serialize};

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
