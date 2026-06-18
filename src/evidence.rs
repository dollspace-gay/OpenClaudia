//! Evidence helpers for decisions grounded in the reality ledger.

use crate::ledger::{Authority, ObsId, Observation, RealityLedger};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{reason}")]
pub struct Denial {
    reason: String,
}

impl Denial {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Hydrate evidence ids and enforce that every cited observation can be used
/// as proof.
///
/// Model summaries and stale file reads may be useful for navigation, but they
/// are not authoritative evidence.
pub fn authoritative_evidence<'a>(
    evidence: &[ObsId],
    ledger: &'a RealityLedger,
    empty_reason: &str,
) -> Result<Vec<&'a Observation>, Denial> {
    if evidence.is_empty() {
        return Err(Denial::new(empty_reason));
    }

    evidence
        .iter()
        .map(|id| {
            let observation = ledger
                .get(*id)
                .ok_or_else(|| Denial::new(format!("unknown evidence observation {id}")))?;
            if observation.authority == Authority::ModelSummary {
                return Err(Denial::new("summary is not authoritative evidence"));
            }
            if ledger.is_stale(*id) {
                return Err(Denial::new(format!(
                    "stale observation {id} cannot be evidence"
                )));
            }
            Ok(observation)
        })
        .collect()
}

/// Like [`authoritative_evidence`], but also accepts an empty evidence set.
pub fn optional_authoritative_evidence<'a>(
    evidence: &[ObsId],
    ledger: &'a RealityLedger,
) -> Result<Vec<&'a Observation>, Denial> {
    if evidence.is_empty() {
        return Ok(Vec::new());
    }
    authoritative_evidence(evidence, ledger, "evidence is required")
}
