//! Final-answer validation for grounded agent turns.

use crate::evidence::{authoritative_evidence, Denial};
use crate::ledger::{ObsId, ObservationKind, RealityLedger};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalGateReport {
    pub evidence: Vec<ObsId>,
    pub verification: Vec<ObsId>,
}

/// Validate that a final answer cites reality and verification.
///
/// A final can report failed verification, but it cannot omit verification.
/// If it mentions tests, it must also cite a concrete command observation.
pub fn validate_final_answer(
    summary: &str,
    evidence: &[ObsId],
    verification: &[ObsId],
    ledger: &RealityLedger,
) -> Result<FinalGateReport, Denial> {
    if summary.trim().is_empty() {
        return Err(Denial::new("final answer requires a non-empty summary"));
    }

    let hydrated_evidence =
        authoritative_evidence(evidence, ledger, "final answer requires evidence")?;

    if verification.is_empty() {
        return Err(Denial::new(
            "final answer requires verification observation",
        ));
    }

    let hydrated_verification = authoritative_evidence(
        verification,
        ledger,
        "final answer requires verification observation",
    )?;
    if !hydrated_verification
        .iter()
        .all(|obs| matches!(obs.kind, ObservationKind::Verification { .. }))
    {
        return Err(Denial::new(
            "final verification ids must reference verification observations",
        ));
    }

    if summary_mentions_tests(summary)
        && !hydrated_evidence
            .iter()
            .any(|obs| matches!(obs.kind, ObservationKind::CommandRun { .. }))
    {
        return Err(Denial::new(
            "final test claims require a command observation",
        ));
    }

    Ok(FinalGateReport {
        evidence: evidence.to_vec(),
        verification: verification.to_vec(),
    })
}

fn summary_mentions_tests(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    lower.contains("test") || lower.contains("cargo check") || lower.contains("verified")
}
