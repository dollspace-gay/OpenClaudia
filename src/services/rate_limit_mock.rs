//! Rate-limit mocking for dev/test (crosslink #639).
//!
//! Most provider SDKs return `429 Too Many Requests` with a
//! `Retry-After: N` header (or equivalent JSON body) under quota
//! pressure. `OpenClaudia` handles those at the proxy layer, but the
//! handling is exercised only by live API calls — and exhausting
//! Anthropic's actual quota during a unit test is a non-starter. This
//! module is the fake the test layer plugs in.
//!
//! ## What ships
//!
//! * [`RateLimitMock`] — trait every dev-mode rate-limit emulator
//!   implements.
//! * [`MockRateLimit`] — deterministic emulator with a configurable
//!   "deny the next N calls" counter, a `retry_after`, and a switch to
//!   toggle behaviour on the fly.
//! * `record_call` / `next_response` pair so tests can step through
//!   the mock without relying on wall-clock timing.
//!
//! ## Where it plugs in (later)
//!
//! The proxy's provider-call path will call `next_response()` *before*
//! issuing the live request. When the mock is `Throttle`d, the proxy
//! short-circuits with the synthetic 429 instead of talking to the
//! upstream. The wiring is the follow-up — this commit lands the seam.

use std::sync::Mutex;
use std::time::Duration;

/// What the next live request should observe under mock pressure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockResponse {
    /// Request proceeds as normal — the mock is dormant.
    Proceed,
    /// Synthetic 429 — the caller should refuse and surface a retry-after.
    Throttle {
        /// Suggested back-off duration the proxy emits to the model.
        retry_after: Duration,
        /// Optional human-readable reason; pipes through to the
        /// `429` response body so tests can assert on the wording.
        reason: String,
    },
}

/// Trait for any dev/test rate-limit emulator. Production code uses
/// `()` for "no mock installed"; tests install [`MockRateLimit`].
pub trait RateLimitMock: Send + Sync {
    /// What the proxy should do for the *next* outbound request.
    fn next_response(&self) -> MockResponse;

    /// Record that a live request was actually sent. Used by mocks
    /// that track per-window counts.
    fn record_call(&self);
}

/// Deterministic mock with a step-counted throttle window.
///
/// State (behind a `Mutex` because the proxy hot path is multi-thread):
/// * `remaining_throttle` — counter that decrements each time
///   `next_response` returns `Throttle`. When it hits zero the mock
///   reverts to `Proceed`.
/// * `enabled` — kill switch tests use to disable the mock without
///   reconstructing it.
pub struct MockRateLimit {
    inner: Mutex<MockInner>,
}

struct MockInner {
    enabled: bool,
    remaining_throttle: usize,
    retry_after: Duration,
    reason: String,
    calls_recorded: usize,
}

impl MockRateLimit {
    /// Build a mock with everything turned off — equivalent to "no
    /// rate-limit pressure". Tests then enable / configure as needed.
    #[must_use]
    pub fn dormant() -> Self {
        Self {
            inner: Mutex::new(MockInner {
                enabled: false,
                remaining_throttle: 0,
                retry_after: Duration::from_secs(0),
                reason: "rate-limit mock dormant".to_string(),
                calls_recorded: 0,
            }),
        }
    }

    /// Configure the mock to throttle the next `count` requests with
    /// `retry_after` back-off and a custom `reason`.
    pub fn throttle_next(&self, count: usize, retry_after: Duration, reason: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.enabled = true;
            g.remaining_throttle = count;
            g.retry_after = retry_after;
            reason.clone_into(&mut g.reason);
        }
    }

    /// Flip the kill switch. `false` means "every request proceeds";
    /// `true` re-enables the configured throttle behaviour.
    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.enabled = enabled;
        }
    }

    /// Number of `record_call` invocations the mock has seen.
    #[must_use]
    pub fn calls_recorded(&self) -> usize {
        self.inner.lock().map_or(0, |g| g.calls_recorded)
    }

    /// Reset the mock to the dormant state.
    pub fn reset(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.enabled = false;
            g.remaining_throttle = 0;
            g.calls_recorded = 0;
        }
    }
}

impl Default for MockRateLimit {
    fn default() -> Self {
        Self::dormant()
    }
}

impl RateLimitMock for MockRateLimit {
    fn next_response(&self) -> MockResponse {
        let Ok(mut g) = self.inner.lock() else {
            return MockResponse::Proceed;
        };
        if !g.enabled || g.remaining_throttle == 0 {
            return MockResponse::Proceed;
        }
        g.remaining_throttle -= 1;
        MockResponse::Throttle {
            retry_after: g.retry_after,
            reason: g.reason.clone(),
        }
    }

    fn record_call(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.calls_recorded += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dormant_lets_every_request_through() {
        let mock = MockRateLimit::dormant();
        for _ in 0..5 {
            assert_eq!(mock.next_response(), MockResponse::Proceed);
        }
    }

    #[test]
    fn throttle_next_decrements_count() {
        let mock = MockRateLimit::dormant();
        mock.throttle_next(2, Duration::from_secs(7), "test quota");

        let MockResponse::Throttle {
            retry_after,
            reason,
        } = mock.next_response()
        else {
            panic!("expected throttle");
        };
        assert_eq!(retry_after, Duration::from_secs(7));
        assert_eq!(reason, "test quota");

        // Second call also throttles.
        assert!(matches!(
            mock.next_response(),
            MockResponse::Throttle { .. }
        ));

        // Third call is back to Proceed because the counter exhausted.
        assert_eq!(mock.next_response(), MockResponse::Proceed);
    }

    #[test]
    fn record_call_counts() {
        let mock = MockRateLimit::dormant();
        for _ in 0..3 {
            mock.record_call();
        }
        assert_eq!(mock.calls_recorded(), 3);
    }

    #[test]
    fn kill_switch_disables_pending_throttle() {
        let mock = MockRateLimit::dormant();
        mock.throttle_next(5, Duration::from_secs(1), "x");
        mock.set_enabled(false);
        assert_eq!(mock.next_response(), MockResponse::Proceed);
        mock.set_enabled(true);
        assert!(matches!(
            mock.next_response(),
            MockResponse::Throttle { .. }
        ));
    }

    #[test]
    fn reset_clears_state() {
        let mock = MockRateLimit::dormant();
        mock.throttle_next(3, Duration::from_secs(1), "x");
        mock.record_call();
        mock.reset();
        assert_eq!(mock.next_response(), MockResponse::Proceed);
        assert_eq!(mock.calls_recorded(), 0);
    }
}
