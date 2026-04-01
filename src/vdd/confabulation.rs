//! Confabulation tracking and false positive detection.
//!
//! Tracks false positive rates across adversarial iterations to detect when
//! the adversary starts hallucinating problems (confabulation threshold).
//! Also provides string similarity and common false positive pattern matching.

use std::collections::HashSet;

// ==========================================================================
// ConfabulationTracker
// ==========================================================================

/// Tracks false positive rates across iterations to detect when the adversary
/// starts hallucinating problems (confabulation threshold).
#[derive(Debug, Clone)]
pub struct ConfabulationTracker {
    /// FP rate per iteration
    pub history: Vec<f32>,
    /// Threshold above which we consider the adversary is confabulating
    pub threshold: f32,
    /// Minimum iterations before checking threshold
    pub min_iterations: u32,
}

impl ConfabulationTracker {
    #[must_use]
    pub const fn new(threshold: f32, min_iterations: u32) -> Self {
        Self {
            history: Vec::new(),
            threshold,
            min_iterations,
        }
    }

    /// Record an iteration's finding counts
    #[allow(clippy::cast_precision_loss)] // FP rates are small enough that f32 is fine
    pub fn record_iteration(&mut self, genuine: u32, false_positives: u32) {
        let total = genuine + false_positives;
        let rate = if total > 0 {
            false_positives as f32 / total as f32
        } else {
            // No findings at all = consider it a clean pass (FP rate 1.0 for convergence)
            1.0
        };
        self.history.push(rate);
    }

    /// Current cumulative false positive rate
    #[must_use]
    pub fn current_rate(&self) -> f32 {
        if self.history.is_empty() {
            return 0.0;
        }
        let total: f32 = self.history.iter().sum();
        #[allow(clippy::cast_precision_loss)] // history len is always small
        let len = self.history.len() as f32;
        total / len
    }

    /// Most recent iteration's false positive rate
    #[must_use]
    pub fn latest_rate(&self) -> f32 {
        self.history.last().copied().unwrap_or(0.0)
    }

    /// Should the loop terminate? Checks both minimum iterations and threshold.
    #[must_use]
    pub fn should_terminate(&self) -> bool {
        #[allow(clippy::cast_possible_truncation)] // history len won't exceed u32::MAX
        if (self.history.len() as u32) < self.min_iterations {
            return false;
        }
        self.latest_rate() >= self.threshold
    }
}

// ==========================================================================
// False Positive Detection Helpers
// ==========================================================================

/// Detect common false positive patterns in adversary findings.
pub(crate) fn is_common_false_positive(description: &str, reasoning: &str) -> bool {
    let combined = format!("{description} {reasoning}");

    let false_positive_patterns = [
        // Standard Rust patterns the adversary may flag incorrectly
        "unwrap() on mutex",
        "poisoned mutex",
        "hardcoded password in test",
        "hardcoded key in test",
        "hardcoded secret in test",
        "deprecated api",
        // Standard library usage that's actually correct
        "silent fallback on mlock",
        "graceful degradation",
        // Protocol-mandated choices
        "hmac-sha1 in yubikey",
        "yubikey hardware uses",
        // Admin-configured values
        "ssrf via.*endpoint",
        "admin-configured.*trusted",
    ];

    for pattern in &false_positive_patterns {
        if combined.contains(pattern) {
            return true;
        }
    }

    // Check for regex patterns
    let regex_patterns = [
        r"test\s+(code|file|module)\s+(requires|needs|uses)\s+deterministic",
        r"admin[\-\s]configured\s+(endpoint|url|path)",
    ];

    for pattern in &regex_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if re.is_match(&combined) {
                return true;
            }
        }
    }

    false
}

/// Simple string similarity based on shared word overlap (Jaccard-like).
pub(crate) fn string_similarity(a: &str, b: &str) -> f32 {
    let words_a: HashSet<&str> = a.split_whitespace().collect();
    let words_b: HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        return 0.0;
    }

    #[allow(clippy::cast_precision_loss)] // word counts are small
    {
        intersection as f32 / union as f32
    }
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confabulation_tracker_below_min_iterations() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(0, 5); // 100% FP but only 1 iteration
        assert!(!tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_terminates() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(2, 3); // 60% FP
        tracker.record_iteration(1, 5); // 83% FP — above threshold, past min
        assert!(tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_does_not_terminate_below_threshold() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(3, 2); // 40% FP
        tracker.record_iteration(2, 2); // 50% FP
        assert!(!tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_no_findings_terminates() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(1, 0); // some genuine first
        tracker.record_iteration(0, 0); // no findings = 1.0 FP rate
        assert!(tracker.should_terminate());
    }

    #[test]
    fn test_confabulation_tracker_current_rate() {
        let mut tracker = ConfabulationTracker::new(0.75, 1);
        tracker.record_iteration(2, 8); // 80%
        tracker.record_iteration(1, 4); // 80%
        assert!((tracker.current_rate() - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_confabulation_tracker_empty() {
        let tracker = ConfabulationTracker::new(0.75, 2);
        assert_eq!(tracker.current_rate(), 0.0);
        assert_eq!(tracker.latest_rate(), 0.0);
        assert!(!tracker.should_terminate());
    }

    #[test]
    fn test_string_similarity_identical() {
        assert!((string_similarity("hello world", "hello world") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_string_similarity_disjoint() {
        assert!((string_similarity("hello world", "foo bar")).abs() < 0.01);
    }

    #[test]
    fn test_string_similarity_partial() {
        let sim = string_similarity(
            "sql injection in query builder",
            "sql injection in db module",
        );
        assert!(sim > 0.3 && sim < 0.8);
    }

    #[test]
    fn test_string_similarity_empty() {
        assert!((string_similarity("", "") - 1.0).abs() < 0.01);
        assert!((string_similarity("hello", "")).abs() < 0.01);
    }

    #[test]
    fn test_is_common_false_positive() {
        assert!(is_common_false_positive(
            "mutex unwrap() on poisoned mutex could panic",
            "the code uses unwrap() on mutex"
        ));
        assert!(is_common_false_positive(
            "hardcoded password in test file",
            "test code has password = 'test123'"
        ));
        assert!(!is_common_false_positive(
            "sql injection in user input handler",
            "string concatenation used for query"
        ));
    }
}
