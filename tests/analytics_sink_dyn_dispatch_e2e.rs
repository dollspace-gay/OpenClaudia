//! End-to-end tests for `services::AnalyticsSink` trait
//! dispatch — pins `NoopAnalytics` no-op contract, `Arc<dyn>`
//! cross-thread Send+Sync, and a `CapturingSink` custom impl
//! to verify the `record()` method invokes per event.
//!
//! Sprint 220 milestone of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::services::{AnalyticsEvent, AnalyticsSink, NoopAnalytics};
use std::sync::{Arc, Mutex};

/// Test-only capturing sink — records every event into a Mutex-protected Vec.
struct CapturingSink {
    events: Mutex<Vec<AnalyticsEvent>>,
}

impl CapturingSink {
    const fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn captured(&self) -> Vec<AnalyticsEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl AnalyticsSink for CapturingSink {
    fn record(&self, event: AnalyticsEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — NoopAnalytics no-op contract
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn noop_analytics_record_is_safe_to_call_repeatedly() {
    let sink = NoopAnalytics;
    for _ in 0..100 {
        sink.record(AnalyticsEvent::SessionStart {
            session_id: "x".to_string(),
        });
    }
    // No-panic + no-side-effect: the test passes by simply
    // completing 100 calls.
}

#[test]
fn noop_analytics_record_accepts_every_variant() {
    let sink = NoopAnalytics;
    sink.record(AnalyticsEvent::SessionStart {
        session_id: "a".to_string(),
    });
    sink.record(AnalyticsEvent::SessionEnd {
        session_id: "a".to_string(),
        messages: 5,
    });
    sink.record(AnalyticsEvent::ToolUsed {
        tool: "bash".to_string(),
        success: true,
    });
    sink.record(AnalyticsEvent::PromptSubmitted { prompt_chars: 100 });
    sink.record(AnalyticsEvent::ContextCompacted {
        trigger: "auto",
        tokens_freed: 1000,
    });
    sink.record(AnalyticsEvent::ApiRequest {
        provider: "anthropic".to_string(),
        model: "claude".to_string(),
    });
    sink.record(AnalyticsEvent::ThinkingEmitted { budget: 8000 });
}

#[test]
fn noop_analytics_is_send_sync_for_arc_dyn_dispatch() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<NoopAnalytics>();
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — CapturingSink records events
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn capturing_sink_records_single_event() {
    let sink = CapturingSink::new();
    sink.record(AnalyticsEvent::SessionStart {
        session_id: "marker-220".to_string(),
    });
    let captured = sink.captured();
    assert_eq!(captured.len(), 1);
}

#[test]
fn capturing_sink_records_multiple_events_in_order() {
    let sink = CapturingSink::new();
    sink.record(AnalyticsEvent::SessionStart {
        session_id: "a".to_string(),
    });
    sink.record(AnalyticsEvent::ToolUsed {
        tool: "bash".to_string(),
        success: true,
    });
    sink.record(AnalyticsEvent::SessionEnd {
        session_id: "a".to_string(),
        messages: 1,
    });
    let captured = sink.captured();
    assert_eq!(captured.len(), 3);
    // PINS ORDER: events recorded FIFO.
    assert!(matches!(captured[0], AnalyticsEvent::SessionStart { .. }));
    assert!(matches!(captured[1], AnalyticsEvent::ToolUsed { .. }));
    assert!(matches!(captured[2], AnalyticsEvent::SessionEnd { .. }));
}

#[test]
fn capturing_sink_starts_empty() {
    let sink = CapturingSink::new();
    assert!(sink.captured().is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Arc<dyn AnalyticsSink> dyn dispatch
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn arc_dyn_sink_dispatches_record_through_trait_object() {
    let sink: Arc<dyn AnalyticsSink> = Arc::new(CapturingSink::new());
    sink.record(AnalyticsEvent::SessionStart {
        session_id: "via-dyn".to_string(),
    });
    // No way to downcast from Arc<dyn AnalyticsSink>, but we
    // verified record() was callable via trait object.
}

#[test]
fn arc_dyn_sink_cloneable_across_threads() {
    let sink: Arc<dyn AnalyticsSink> = Arc::new(CapturingSink::new());
    let s1 = Arc::clone(&sink);
    let s2 = Arc::clone(&sink);

    let h1 = std::thread::spawn(move || {
        s1.record(AnalyticsEvent::ToolUsed {
            tool: "t1".to_string(),
            success: true,
        });
    });
    let h2 = std::thread::spawn(move || {
        s2.record(AnalyticsEvent::ToolUsed {
            tool: "t2".to_string(),
            success: false,
        });
    });
    h1.join().expect("thread 1 ok");
    h2.join().expect("thread 2 ok");
}

#[test]
fn arc_dyn_noop_sink_through_trait_object() {
    let sink: Arc<dyn AnalyticsSink> = Arc::new(NoopAnalytics);
    sink.record(AnalyticsEvent::PromptSubmitted { prompt_chars: 5 });
    // No panic, no side effect.
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Capturing under concurrent record
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn capturing_sink_accumulates_across_2_threads_safely() {
    let sink: Arc<CapturingSink> = Arc::new(CapturingSink::new());
    let s1 = Arc::clone(&sink);
    let s2 = Arc::clone(&sink);

    let h1 = std::thread::spawn(move || {
        for _ in 0..10 {
            s1.record(AnalyticsEvent::ThinkingEmitted { budget: 1 });
        }
    });
    let h2 = std::thread::spawn(move || {
        for _ in 0..10 {
            s2.record(AnalyticsEvent::ThinkingEmitted { budget: 2 });
        }
    });
    h1.join().unwrap();
    h2.join().unwrap();

    let captured = sink.captured();
    assert_eq!(captured.len(), 20, "MUST capture all 20 events");
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Per-variant payload fidelity
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn capturing_sink_preserves_session_start_payload() {
    let sink = CapturingSink::new();
    sink.record(AnalyticsEvent::SessionStart {
        session_id: "payload-marker".to_string(),
    });
    let captured = sink.captured();
    if let AnalyticsEvent::SessionStart { session_id } = &captured[0] {
        assert_eq!(session_id, "payload-marker");
    } else {
        panic!("expected SessionStart");
    }
}

#[test]
fn capturing_sink_preserves_session_end_messages_count() {
    let sink = CapturingSink::new();
    sink.record(AnalyticsEvent::SessionEnd {
        session_id: "x".to_string(),
        messages: 999,
    });
    let captured = sink.captured();
    if let AnalyticsEvent::SessionEnd { messages, .. } = &captured[0] {
        assert_eq!(*messages, 999);
    }
}

#[test]
fn capturing_sink_preserves_tool_used_success_bit() {
    let sink = CapturingSink::new();
    sink.record(AnalyticsEvent::ToolUsed {
        tool: "x".to_string(),
        success: false,
    });
    let captured = sink.captured();
    if let AnalyticsEvent::ToolUsed { success, .. } = &captured[0] {
        assert!(!*success);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Send+Sync invariants on CapturingSink + NoopAnalytics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn capturing_sink_is_send_sync_for_arc_dyn() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CapturingSink>();
}

#[test]
fn noop_analytics_can_be_default_constructed_with_struct_unit_literal() {
    // PINS: NoopAnalytics is a unit struct, constructible directly.
    let _: NoopAnalytics = NoopAnalytics;
}

#[test]
fn dyn_analytics_sink_send_sync_required_by_trait_definition() {
    // PINS DOC: `AnalyticsSink: Send + Sync` trait bound.
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn AnalyticsSink>();
}
