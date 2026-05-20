//! Confabulation tracking and false positive detection.
//!
//! Tracks false positive rates across adversarial iterations to detect when
//! the adversary starts hallucinating problems (confabulation threshold).
//! Also provides finding signature hashing for deterministic duplicate
//! detection and common false positive pattern matching.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use regex::Regex;

use crate::vdd::finding::{Finding, Severity};

// ==========================================================================
// Compiled regex patterns for false-positive detection (crosslink #346)
//
// Previously these were `regex::Regex::new(pattern)` calls inside
// `is_common_false_positive`, executed once per finding per iteration.
// Promoting to module-level `LazyLock<Vec<Regex>>` makes compilation a
// one-time cost. Failure to compile is a constant-data bug, so we panic
// at first access rather than silently treat the match as `false`.
// ==========================================================================

const FALSE_POSITIVE_REGEX_PATTERNS: &[&str] = &[
    r"test\s+(code|file|module)\s+(requires|needs|uses)\s+deterministic",
    r"admin[\-\s]configured\s+(endpoint|url|path)",
];

static FALSE_POSITIVE_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    FALSE_POSITIVE_REGEX_PATTERNS
        .iter()
        .map(|p| Regex::new(p).unwrap_or_else(|e| panic!("invalid FP regex {p:?}: {e}")))
        .collect()
});

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

    // Pre-compiled regex patterns (crosslink #346); compilation failure is
    // a startup panic, not a silent miss.
    for re in FALSE_POSITIVE_REGEXES.iter() {
        if re.is_match(&combined) {
            return true;
        }
    }

    false
}

/// Identifying tuple used to deduplicate findings across iterations.
///
/// Captures just enough to recompute a strong or weak signature for a
/// previously seen false positive without retaining the whole `Finding`.
/// See crosslink #349 — the previous Jaccard-over-whitespace similarity
/// was both false-negative (synonyms missed) and false-positive
/// (stop-word collisions); deterministic tuple hashing replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingIdentity {
    pub file_path: Option<String>,
    pub severity: Severity,
    pub cwe: Option<String>,
    pub line_range: Option<(usize, usize)>,
    /// Kept only for the weak-fallback path used when both `cwe` and
    /// `line_range` are absent (no other signal to distinguish findings).
    pub description: String,
}

impl FindingIdentity {
    #[must_use]
    pub fn from_finding(f: &Finding) -> Self {
        Self {
            file_path: f.file_path.clone(),
            severity: f.severity.clone(),
            cwe: f.cwe.clone(),
            line_range: f.line_range,
            description: f.description.clone(),
        }
    }

    /// Returns `true` when neither `cwe` nor `line_range` are populated and
    /// the strong tuple would be too coarse to be meaningful.
    #[must_use]
    pub const fn is_weak(&self) -> bool {
        self.cwe.is_none() && self.line_range.is_none()
    }

    /// Strong-tuple signature equivalent to [`finding_signature`].
    #[must_use]
    pub fn signature(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        "strong".hash(&mut hasher);
        self.file_path
            .as_deref()
            .map(str::to_lowercase)
            .hash(&mut hasher);
        self.severity.hash(&mut hasher);
        self.cwe.as_deref().map(str::to_lowercase).hash(&mut hasher);
        self.line_range.hash(&mut hasher);
        hasher.finish()
    }

    /// Weak-tuple signature equivalent to [`weak_finding_signature`].
    ///
    /// The description is folded to its first `PREFIX_BYTES` of lowercased
    /// content. 32 bytes is intentionally short — the whole point of the
    /// weak fallback is to collapse minor wording variations (a re-issued
    /// finding with extra trailing context, a `(re-reported)` suffix,
    /// etc.) onto the same signature so layer-1 dedup actually catches
    /// re-reports. Going beyond ~40 bytes lets every trailing word
    /// shift the hash and defeats the point. The byte cap is computed
    /// at a UTF-8 char boundary so multi-byte characters cannot land
    /// the slice mid-codepoint.
    #[must_use]
    pub fn weak_signature(&self) -> u64 {
        const PREFIX_BYTES: usize = 32;

        let mut hasher = DefaultHasher::new();
        "weak".hash(&mut hasher);
        self.file_path
            .as_deref()
            .map(str::to_lowercase)
            .hash(&mut hasher);
        self.severity.hash(&mut hasher);
        let lower = self.description.to_lowercase();
        // Walk char boundaries and accept each char whose *end* still
        // fits inside PREFIX_BYTES. `take_while` here keeps the running
        // cumulative byte length under the cap; the prefix is therefore
        // always a valid UTF-8 substring of `lower`.
        let prefix_end = lower
            .char_indices()
            .map(|(i, c)| i + c.len_utf8())
            .take_while(|end| *end <= PREFIX_BYTES)
            .last()
            .unwrap_or(0);
        let prefix = &lower[..prefix_end.min(lower.len())];
        prefix.hash(&mut hasher);
        hasher.finish()
    }
}

/// Hash the normalized `(file_path, severity, cwe, line_range)` tuple into
/// a deterministic 64-bit signature for finding deduplication.
///
/// `file_path` and `cwe` are normalized to lowercase so trivial casing
/// differences from the adversary's output cannot defeat dedup. Delegates
/// to [`FindingIdentity::signature`] so the in-memory identity and the
/// finding itself always produce the same hash.
#[must_use]
pub fn finding_signature(f: &Finding) -> u64 {
    FindingIdentity::from_finding(f).signature()
}

/// Hash a deliberately weaker tuple — `(file_path, severity, description
/// prefix)` — for findings where both `cwe` and `line_range` are absent.
///
/// Used only as a fallback when [`FindingIdentity::is_weak`] is `true`;
/// callers are expected to log a warning before relying on it because
/// description-based matching is fragile (see crosslink #349). Delegates
/// to [`FindingIdentity::weak_signature`].
#[must_use]
pub fn weak_finding_signature(f: &Finding) -> u64 {
    FindingIdentity::from_finding(f).weak_signature()
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

    fn make_finding(
        file: Option<&str>,
        severity: Severity,
        cwe: Option<&str>,
        lines: Option<(usize, usize)>,
        description: &str,
    ) -> Finding {
        use crate::vdd::finding::FindingStatus;
        Finding {
            id: "test".to_string(),
            severity,
            cwe: cwe.map(str::to_string),
            description: description.to_string(),
            file_path: file.map(str::to_string),
            line_range: lines,
            status: FindingStatus::Genuine,
            adversary_reasoning: String::new(),
            iteration: 0,
        }
    }

    /// Two findings with identical (file, severity, cwe, `line_range`) hash
    /// to the same signature — the second is a duplicate of the first.
    /// This is the crosslink #349 happy path: deterministic tuple dedup.
    #[test]
    fn finding_signature_identical_tuples_collide() {
        let a = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-89"),
            Some((10, 20)),
            "SQL injection in users query",
        );
        let b = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-89"),
            Some((10, 20)),
            "String concatenation vulnerability in users table lookup",
        );
        assert_eq!(
            finding_signature(&a),
            finding_signature(&b),
            "synonym-worded findings with the same tuple must produce \
             the same signature (Jaccard would have missed this)"
        );
    }

    /// Different cwe on the same file+severity → different signatures.
    #[test]
    fn finding_signature_differs_on_cwe() {
        let a = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-89"),
            None,
            "issue",
        );
        let b = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-79"),
            None,
            "issue",
        );
        assert_ne!(finding_signature(&a), finding_signature(&b));
    }

    /// Different `line_range` on the same file+cwe+severity → different
    /// signatures (two genuinely different findings in the same file).
    #[test]
    fn finding_signature_differs_on_line_range() {
        let a = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-89"),
            Some((10, 20)),
            "issue",
        );
        let b = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-89"),
            Some((45, 50)),
            "issue",
        );
        assert_ne!(finding_signature(&a), finding_signature(&b));
    }

    /// Case differences in `file_path` / cwe must not defeat dedup.
    #[test]
    fn finding_signature_normalizes_casing() {
        let a = make_finding(
            Some("src/DB.rs"),
            Severity::High,
            Some("cwe-89"),
            Some((10, 20)),
            "x",
        );
        let b = make_finding(
            Some("src/db.rs"),
            Severity::High,
            Some("CWE-89"),
            Some((10, 20)),
            "x",
        );
        assert_eq!(finding_signature(&a), finding_signature(&b));
    }

    /// A weak finding (no cwe, no `line_range`) is flagged as such and the
    /// weak signature still collides for the same file+severity+description-prefix.
    #[test]
    fn weak_finding_signature_collides_on_same_prefix() {
        let a = make_finding(
            Some("src/x.rs"),
            Severity::Medium,
            None,
            None,
            "Possible panic in helper if input is malformed",
        );
        let b = make_finding(
            Some("src/x.rs"),
            Severity::Medium,
            None,
            None,
            "Possible panic in helper if input is malformed — additional context",
        );
        let id = FindingIdentity::from_finding(&a);
        assert!(id.is_weak(), "no cwe + no line_range must be weak");
        assert_eq!(weak_finding_signature(&a), weak_finding_signature(&b));
    }

    /// Long descriptions heavy in stop-words but with different files/severity
    /// must NOT collide under the weak signature — this was the Jaccard FP
    /// the previous implementation suffered from.
    #[test]
    fn weak_finding_signature_does_not_collapse_unrelated_findings() {
        // Both descriptions start with the same generic stop-word phrase,
        // but the file differs — the previous Jaccard impl would happily
        // collapse them; the tuple signature must not.
        let a = make_finding(
            Some("src/auth.rs"),
            Severity::Medium,
            None,
            None,
            "the issue is that a value in the helper is not checked",
        );
        let b = make_finding(
            Some("src/db.rs"),
            Severity::Medium,
            None,
            None,
            "the issue is that a value in the helper is not checked",
        );
        assert_ne!(
            weak_finding_signature(&a),
            weak_finding_signature(&b),
            "different files must produce different weak signatures"
        );
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
