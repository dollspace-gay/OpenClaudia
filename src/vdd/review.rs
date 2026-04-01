//! VDD review session types and iteration tracking.
//!
//! Contains the data structures for tracking adversary reviews, VDD iterations,
//! and full VDD sessions across the adversarial loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::VddMode;
use crate::session::TokenUsage;

use super::finding::Finding;
use super::static_analysis::StaticAnalysisResult;

// ==========================================================================
// AdversaryResponse
// ==========================================================================

/// Parsed adversary response
#[derive(Debug, Deserialize)]
pub(crate) struct AdversaryResponse {
    pub(crate) findings: Option<Vec<super::finding::RawFinding>>,
    pub(crate) assessment: Option<String>,
}

// ==========================================================================
// AdversaryReview
// ==========================================================================

/// Result of a single adversary review iteration
#[derive(Debug, Clone, Serialize)]
pub struct AdversaryReview {
    pub iteration: u32,
    pub findings: Vec<Finding>,
    pub raw_response: String,
    pub tokens_used: TokenUsage,
    pub timestamp: DateTime<Utc>,
}

// ==========================================================================
// VddIteration
// ==========================================================================

/// A complete adversarial loop iteration (builder response + analysis + adversary review)
#[derive(Debug, Clone, Serialize)]
pub struct VddIteration {
    pub number: u32,
    pub builder_response: String,
    pub static_analysis: Vec<StaticAnalysisResult>,
    pub adversary_review: AdversaryReview,
    pub genuine_count: u32,
    pub false_positive_count: u32,
}

// ==========================================================================
// VddSession
// ==========================================================================

/// Full VDD session tracking across all iterations
#[derive(Debug, Clone, Serialize)]
pub struct VddSession {
    pub id: String,
    pub mode: VddMode,
    pub iterations: Vec<VddIteration>,
    pub total_findings: u32,
    pub total_genuine: u32,
    pub total_false_positives: u32,
    pub false_positive_rate: f32,
    pub converged: bool,
    pub termination_reason: Option<String>,
    pub builder_tokens: TokenUsage,
    pub adversary_tokens: TokenUsage,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl VddSession {
    pub(crate) fn new(mode: VddMode) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            mode,
            iterations: Vec::new(),
            total_findings: 0,
            total_genuine: 0,
            total_false_positives: 0,
            false_positive_rate: 0.0,
            converged: false,
            termination_reason: None,
            builder_tokens: TokenUsage::default(),
            adversary_tokens: TokenUsage::default(),
            started_at: Utc::now(),
            ended_at: None,
        }
    }

    pub(crate) fn record_iteration(&mut self, iteration: VddIteration) {
        self.total_findings += iteration.genuine_count + iteration.false_positive_count;
        self.total_genuine += iteration.genuine_count;
        self.total_false_positives += iteration.false_positive_count;
        #[allow(clippy::cast_precision_loss)]
        {
            self.false_positive_rate = if self.total_findings > 0 {
                self.total_false_positives as f32 / self.total_findings as f32
            } else {
                0.0
            };
        }
        self.adversary_tokens
            .accumulate(&iteration.adversary_review.tokens_used);
        self.iterations.push(iteration);
    }

    pub(crate) fn finalize(&mut self, converged: bool, reason: &str) {
        self.converged = converged;
        self.termination_reason = Some(reason.to_string());
        self.ended_at = Some(Utc::now());
    }
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vdd_session_record_iteration() {
        let mut session = VddSession::new(VddMode::Blocking);
        let iteration = VddIteration {
            number: 1,
            builder_response: "code here".to_string(),
            static_analysis: Vec::new(),
            adversary_review: AdversaryReview {
                iteration: 1,
                findings: Vec::new(),
                raw_response: "{}".to_string(),
                tokens_used: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    ..Default::default()
                },
                timestamp: Utc::now(),
            },
            genuine_count: 2,
            false_positive_count: 3,
        };

        session.record_iteration(iteration);
        assert_eq!(session.total_findings, 5);
        assert_eq!(session.total_genuine, 2);
        assert_eq!(session.total_false_positives, 3);
        assert!((session.false_positive_rate - 0.6).abs() < 0.01);
        assert_eq!(session.adversary_tokens.input_tokens, 100);
    }

    #[test]
    fn test_vdd_session_finalize() {
        let mut session = VddSession::new(VddMode::Advisory);
        session.finalize(true, "Confabulation threshold reached");
        assert!(session.converged);
        assert_eq!(
            session.termination_reason,
            Some("Confabulation threshold reached".to_string())
        );
        assert!(session.ended_at.is_some());
    }
}
