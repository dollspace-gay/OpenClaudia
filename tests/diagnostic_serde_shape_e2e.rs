//! End-to-end tests for `services::lsp_diagnostics::Diagnostic`
//! serde wire shape + per-field invariants — the source-is-optional
//! contract (`skip_serializing_if = "Option::is_none"`),
//! severity numeric vs string serde, and `PartialEq` derive.
//!
//! Sprint 202 of the verification effort. Sprint 117 / etc.
//! covered the `DiagnosticRegistry` accumulator; this file
//! pins the per-field serde shape of `Diagnostic` directly.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::services::{Diagnostic, DiagnosticSeverity};
use serde_json::json;

fn make(line: u32, message: &str, severity: DiagnosticSeverity) -> Diagnostic {
    Diagnostic {
        line,
        character: 0,
        severity,
        message: message.to_string(),
        source: None,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — Required fields
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn diagnostic_serializes_with_all_required_fields() {
    let d = make(42, "missing import", DiagnosticSeverity::Error);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert!(value.get("line").is_some());
    assert!(value.get("character").is_some());
    assert!(value.get("severity").is_some());
    assert!(value.get("message").is_some());
}

#[test]
fn diagnostic_line_serializes_as_u32_numeric() {
    let d = make(1000, "x", DiagnosticSeverity::Warning);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["line"].as_u64(), Some(1000));
}

#[test]
fn diagnostic_character_field_serializes_as_u32() {
    let d = Diagnostic {
        line: 1,
        character: 42,
        severity: DiagnosticSeverity::Warning,
        message: "x".to_string(),
        source: None,
    };
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["character"].as_u64(), Some(42));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — source is optional with skip_serializing_if
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn diagnostic_source_none_is_omitted_from_serialized_json() {
    // PINS DOC: skip_serializing_if = "Option::is_none" means
    // None source is NOT emitted at all (vs null).
    let d = make(1, "x", DiagnosticSeverity::Error);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert!(
        value.get("source").is_none(),
        "source=None MUST be omitted; got {value:?}"
    );
}

#[test]
fn diagnostic_source_some_is_included_in_serialized_json() {
    let d = Diagnostic {
        line: 1,
        character: 0,
        severity: DiagnosticSeverity::Error,
        message: "x".to_string(),
        source: Some("rust-analyzer".to_string()),
    };
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["source"].as_str(), Some("rust-analyzer"));
}

#[test]
fn diagnostic_deserializes_without_source_field() {
    // PINS: missing source field deserializes to None.
    let value = json!({
        "line": 1,
        "character": 0,
        "severity": "error",
        "message": "x"
    });
    let d: Diagnostic = serde_json::from_value(value).expect("de");
    assert!(d.source.is_none());
}

#[test]
fn diagnostic_deserializes_with_source_field() {
    let value = json!({
        "line": 1,
        "character": 0,
        "severity": "warning",
        "message": "x",
        "source": "tsserver"
    });
    let d: Diagnostic = serde_json::from_value(value).expect("de");
    assert_eq!(d.source.as_deref(), Some("tsserver"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Severity serde (lowercase)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn severity_error_serializes_lowercase() {
    let d = make(1, "x", DiagnosticSeverity::Error);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["severity"].as_str(), Some("error"));
}

#[test]
fn severity_warning_serializes_lowercase() {
    let d = make(1, "x", DiagnosticSeverity::Warning);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["severity"].as_str(), Some("warning"));
}

#[test]
fn severity_information_serializes_lowercase() {
    let d = make(1, "x", DiagnosticSeverity::Information);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["severity"].as_str(), Some("information"));
}

#[test]
fn severity_hint_serializes_lowercase() {
    let d = make(1, "x", DiagnosticSeverity::Hint);
    let value: serde_json::Value = serde_json::to_value(&d).expect("ser");
    assert_eq!(value["severity"].as_str(), Some("hint"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn diagnostic_full_round_trip_preserves_all_fields() {
    let original = Diagnostic {
        line: 42,
        character: 17,
        severity: DiagnosticSeverity::Error,
        message: "type mismatch".to_string(),
        source: Some("rust-analyzer".to_string()),
    };
    let json = serde_json::to_value(&original).expect("ser");
    let back: Diagnostic = serde_json::from_value(json).expect("de");
    assert_eq!(back, original, "full round-trip MUST preserve all fields");
}

#[test]
fn diagnostic_round_trip_with_source_none_preserves_none() {
    let original = make(1, "x", DiagnosticSeverity::Warning);
    let json = serde_json::to_value(&original).expect("ser");
    let back: Diagnostic = serde_json::from_value(json).expect("de");
    assert_eq!(back.source, None);
    assert_eq!(back, original);
}

#[test]
fn diagnostic_all_four_severity_variants_round_trip() {
    for sev in [
        DiagnosticSeverity::Error,
        DiagnosticSeverity::Warning,
        DiagnosticSeverity::Information,
        DiagnosticSeverity::Hint,
    ] {
        let original = make(1, "msg", sev);
        let json = serde_json::to_value(&original).expect("ser");
        let back: Diagnostic = serde_json::from_value(json).expect("de");
        assert_eq!(back, original, "round-trip failed for {sev:?}");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Clone + PartialEq derive
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn diagnostic_clone_preserves_all_fields() {
    let original = Diagnostic {
        line: 100,
        character: 50,
        severity: DiagnosticSeverity::Hint,
        message: "marker_202".to_string(),
        source: Some("source_marker".to_string()),
    };
    let cloned = original.clone();
    assert_eq!(cloned, original);
    assert_eq!(cloned.line, 100);
    assert_eq!(cloned.character, 50);
    assert_eq!(cloned.severity, DiagnosticSeverity::Hint);
    assert_eq!(cloned.message, "marker_202");
}

#[test]
fn diagnostic_partial_eq_distinguishes_different_lines() {
    let d1 = make(1, "x", DiagnosticSeverity::Error);
    let d2 = make(2, "x", DiagnosticSeverity::Error);
    assert_ne!(d1, d2);
}

#[test]
fn diagnostic_partial_eq_distinguishes_different_severities() {
    let d1 = make(1, "x", DiagnosticSeverity::Error);
    let d2 = make(1, "x", DiagnosticSeverity::Warning);
    assert_ne!(d1, d2);
}

#[test]
fn diagnostic_partial_eq_distinguishes_different_sources() {
    let d1 = Diagnostic {
        line: 1,
        character: 0,
        severity: DiagnosticSeverity::Error,
        message: "x".to_string(),
        source: Some("a".to_string()),
    };
    let d2 = Diagnostic {
        line: 1,
        character: 0,
        severity: DiagnosticSeverity::Error,
        message: "x".to_string(),
        source: Some("b".to_string()),
    };
    assert_ne!(d1, d2);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Robustness
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn diagnostic_with_empty_message_round_trips() {
    let d = make(1, "", DiagnosticSeverity::Error);
    let json = serde_json::to_value(&d).expect("ser");
    let back: Diagnostic = serde_json::from_value(json).expect("de");
    assert_eq!(back.message, "");
}

#[test]
fn diagnostic_with_unicode_message_preserves_bytes() {
    let d = make(1, "日本語のエラー", DiagnosticSeverity::Error);
    let json = serde_json::to_value(&d).expect("ser");
    let back: Diagnostic = serde_json::from_value(json).expect("de");
    assert_eq!(back.message, "日本語のエラー");
}

#[test]
fn diagnostic_with_huge_line_number_serializes_as_decimal() {
    let d = make(u32::MAX, "x", DiagnosticSeverity::Error);
    let json = serde_json::to_value(&d).expect("ser");
    assert_eq!(json["line"].as_u64(), Some(u64::from(u32::MAX)));
}
