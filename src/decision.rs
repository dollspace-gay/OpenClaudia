//! Typed agent decisions and validation against the reality ledger.

use crate::evidence::{authoritative_evidence, Denial};
use crate::final_gate::{validate_final_answer, FinalGateReport};
use crate::ledger::{ObsId, ObservationKind, RealityLedger};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InspectTarget {
    File { path: String },
    Diff,
    Command { argv: Vec<String> },
    Observation { id: ObsId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDecision {
    Inspect {
        reason: String,
        target: InspectTarget,
    },
    Edit {
        reason: String,
        evidence: Vec<ObsId>,
        patch: String,
    },
    RunCommand {
        reason: String,
        evidence: Vec<ObsId>,
        argv: Vec<String>,
    },
    Final {
        summary: String,
        evidence: Vec<ObsId>,
        verification: Vec<ObsId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionValidation {
    Inspect,
    Edit { evidence: Vec<ObsId> },
    RunCommand { evidence: Vec<ObsId> },
    Final(FinalGateReport),
}

pub fn validate_decision(
    decision: &AgentDecision,
    ledger: &RealityLedger,
) -> Result<DecisionValidation, Denial> {
    match decision {
        AgentDecision::Inspect { reason, .. } => {
            if reason.trim().is_empty() {
                return Err(Denial::new("inspect requires a reason"));
            }
            Ok(DecisionValidation::Inspect)
        }
        AgentDecision::Edit {
            reason,
            evidence,
            patch,
        } => {
            if reason.trim().is_empty() {
                return Err(Denial::new("edit requires a reason"));
            }
            if patch.trim().is_empty() {
                return Err(Denial::new("empty patch"));
            }

            let observations = authoritative_evidence(evidence, ledger, "edit requires evidence")?;
            if !observations
                .iter()
                .any(|obs| matches!(obs.kind, ObservationKind::FileRead { .. }))
            {
                return Err(Denial::new("edit requires prior file observation"));
            }

            Ok(DecisionValidation::Edit {
                evidence: evidence.clone(),
            })
        }
        AgentDecision::RunCommand {
            reason,
            evidence,
            argv,
        } => {
            if reason.trim().is_empty() {
                return Err(Denial::new("command requires a reason"));
            }
            if argv.is_empty() {
                return Err(Denial::new("command argv cannot be empty"));
            }
            authoritative_evidence(evidence, ledger, "command requires evidence")?;
            Ok(DecisionValidation::RunCommand {
                evidence: evidence.clone(),
            })
        }
        AgentDecision::Final {
            summary,
            evidence,
            verification,
        } => validate_final_answer(summary, evidence, verification, ledger)
            .map(DecisionValidation::Final),
    }
}
