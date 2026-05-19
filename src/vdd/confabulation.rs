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
///
/// Each history slot is `Some(rate)` when the iteration had at least one
/// finding, or `None` for zero-findings ("clean pass") iterations.
/// `None` entries are excluded from rate calculations and termination checks
/// so that a clean pass cannot be mistaken for 100% confabulation.
#[derive(Debug, Clone)]
pub struct ConfabulationTracker {
    /// FP rate per iteration; `None` means zero findings (clean pass)
    pub history: Vec<Option<f64>>,
    /// Threshold above which we consider the adversary is confabulating
    pub threshold: f64,
    /// Minimum iterations before checking threshold
    pub min_iterations: u32,
}

impl ConfabulationTracker {
    #[must_use]
    pub const fn new(threshold: f64, min_iterations: u32) -> Self {
        Self {
            history: Vec::new(),
            threshold,
            min_iterations,
        }
    }

    /// Record an iteration's finding counts.
    ///
    /// Returns `Some(rate)` when the iteration had at least one finding, or
    /// `None` when both counts are zero (a clean pass with no findings at all).
    /// `None` entries do **not** contribute to the confabulation rate so that
    /// "the adversary found nothing" cannot be misread as "everything was a
    /// hallucination".
    pub fn record_iteration(&mut self, genuine: u32, false_positives: u32) -> Option<f64> {
        let total = genuine + false_positives;
        let rate = if total > 0 {
            // f64's 53-bit mantissa represents every u32 exactly, so this
            // division is precise within f64 rounding.
            Some(f64::from(false_positives) / f64::from(total))
        } else {
            None // zero findings — clean pass, not a confabulation signal
        };
        self.history.push(rate);
        rate
    }

    /// Current cumulative false positive rate across iterations that had
    /// findings.  Returns `None` when no iteration with findings has been
    /// recorded yet (avoids the "0 / 0 = ambiguous" problem).
    #[must_use]
    pub fn current_rate(&self) -> Option<f64> {
        let rated: Vec<f64> = self.history.iter().flatten().copied().collect();
        if rated.is_empty() {
            return None;
        }
        let total: f64 = rated.iter().sum();
        // rated.len() is bounded by the iteration count (always fits in u32).
        // Convert usize → u32 (saturating) → f64 (exact for u32) so the divisor
        // round-trips without precision loss.
        let count = u32::try_from(rated.len()).unwrap_or(u32::MAX);
        Some(total / f64::from(count))
    }

    /// Most recent iteration's false positive rate, or `None` when no
    /// iteration with findings has occurred yet (including the case where the
    /// last recorded iteration was a zero-findings clean pass).
    #[must_use]
    pub fn latest_rate(&self) -> Option<f64> {
        self.history.iter().rev().find_map(|r| *r)
    }

    /// Should the loop terminate?  Checks both minimum iterations and
    /// threshold.  Zero-findings ("clean pass") iterations count toward
    /// `min_iterations` but are excluded from the rate comparison so they
    /// cannot trigger a false "confabulation convergence" signal.
    #[must_use]
    pub fn should_terminate(&self) -> bool {
        // Saturating conversion: if history somehow exceeded u32::MAX iterations
        // (impossible in practice), we'd already be past any sane min_iterations.
        let len = u32::try_from(self.history.len()).unwrap_or(u32::MAX);
        if len < self.min_iterations {
            return false;
        }
        // None means no rated iteration yet — cannot be above threshold
        self.latest_rate()
            .is_some_and(|rate| rate >= self.threshold)
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

    /// Regression test for #353: zero-findings iteration must NOT trigger
    /// confabulation termination. A clean pass returns None and
    /// `should_terminate` must return false even past `min_iterations`.
    #[test]
    fn test_confabulation_tracker_no_findings_does_not_terminate() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(1, 0); // genuine finding — 0% FP
        let result = tracker.record_iteration(0, 0); // zero findings — clean pass
        assert!(result.is_none(), "zero findings must return None");
        // Past min_iterations but latest_rate is None — must NOT terminate
        assert!(
            !tracker.should_terminate(),
            "zero-findings iteration must not trigger confabulation threshold"
        );
    }

    /// A sequence of only zero-finding iterations must not terminate and
    /// `current_rate` must return None (no data, not 0.0 or 1.0).
    #[test]
    fn test_confabulation_tracker_all_zero_findings_returns_none_rate() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(0, 0);
        tracker.record_iteration(0, 0);
        assert!(
            tracker.current_rate().is_none(),
            "current_rate must be None when every iteration had zero findings"
        );
        assert!(
            tracker.latest_rate().is_none(),
            "latest_rate must be None when every iteration had zero findings"
        );
        assert!(
            !tracker.should_terminate(),
            "all-zero-findings history must not trigger termination"
        );
    }

    /// All genuine findings (0% FP) — should return Some(0.0), not None,
    /// and should not terminate even past `min_iterations`.
    #[test]
    fn test_confabulation_tracker_all_clean_findings_returns_zero_rate() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);
        tracker.record_iteration(5, 0); // 0% FP
        tracker.record_iteration(3, 0); // 0% FP
        let rate = tracker
            .current_rate()
            .expect("all-genuine iterations must yield Some(rate)");
        assert!(rate.abs() < f64::EPSILON, "0% FP rate expected, got {rate}");
        assert!(
            !tracker.should_terminate(),
            "0% FP rate must not trigger confabulation threshold"
        );
    }

    /// Some confabulated (non-zero FP) — rate must be accurate.
    #[test]
    fn test_confabulation_tracker_partial_fp_rate_is_correct() {
        let mut tracker = ConfabulationTracker::new(0.75, 1);
        let r = tracker
            .record_iteration(1, 3) // 75% FP
            .expect("non-zero total must return Some(rate)");
        assert!((r - 0.75).abs() < f64::EPSILON, "expected 0.75, got {r}");
        // Exactly at threshold — should terminate (>= not >)
        assert!(
            tracker.should_terminate(),
            "rate == threshold must terminate"
        );
    }

    #[test]
    fn test_confabulation_tracker_current_rate() {
        let mut tracker = ConfabulationTracker::new(0.75, 1);
        tracker.record_iteration(2, 8); // 80%
        tracker.record_iteration(1, 4); // 80%
        let rate = tracker
            .current_rate()
            .expect("rated iterations must yield Some");
        assert!((rate - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_confabulation_tracker_empty() {
        let tracker = ConfabulationTracker::new(0.75, 2);
        assert!(tracker.current_rate().is_none());
        assert!(tracker.latest_rate().is_none());
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
