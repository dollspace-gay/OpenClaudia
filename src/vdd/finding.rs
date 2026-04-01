//! Finding types for VDD adversary reviews.
//!
//! Contains the core data structures for representing individual findings,
//! their severity levels, and their triage status.

use serde::{Deserialize, Serialize};

// ==========================================================================
// Severity
// ==========================================================================

/// Severity classification for adversary findings
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Critical => write!(f, "CRITICAL"),
            Self::High => write!(f, "HIGH"),
            Self::Medium => write!(f, "MEDIUM"),
            Self::Low => write!(f, "LOW"),
            Self::Info => write!(f, "INFO"),
        }
    }
}

// ==========================================================================
// FindingStatus
// ==========================================================================

/// Whether a finding is genuine or a false positive
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Genuine,
    FalsePositive,
    Disputed,
}

// ==========================================================================
// Finding
// ==========================================================================

/// A single finding from the adversary's review
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub cwe: Option<String>,
    pub description: String,
    pub file_path: Option<String>,
    pub line_range: Option<(usize, usize)>,
    pub status: FindingStatus,
    pub adversary_reasoning: String,
    pub iteration: u32,
}

// ==========================================================================
// RawFinding
// ==========================================================================

/// Raw finding from adversary JSON before triage
#[derive(Debug, Deserialize)]
pub(crate) struct RawFinding {
    pub(crate) severity: Option<String>,
    pub(crate) cwe: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) lines: Option<Vec<usize>>,
    pub(crate) reasoning: Option<String>,
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Critical < Severity::High);
        assert!(Severity::High < Severity::Medium);
        assert!(Severity::Medium < Severity::Low);
        assert!(Severity::Low < Severity::Info);
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(format!("{}", Severity::Critical), "CRITICAL");
        assert_eq!(format!("{}", Severity::High), "HIGH");
        assert_eq!(format!("{}", Severity::Medium), "MEDIUM");
        assert_eq!(format!("{}", Severity::Low), "LOW");
        assert_eq!(format!("{}", Severity::Info), "INFO");
    }
}
