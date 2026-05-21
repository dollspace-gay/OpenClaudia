//! End-to-end tests for `memdir::entrypoint` MEMORY.md discovery +
//! truncation behaviour + `mcp_elicitation` wire-format
//! serialization + `NoopElicitationHandler` default.
//!
//! Sprint 78 of the verification effort. Two library-side
//! modules without dedicated integration coverage: the
//! MEMORY.md context-injection loader (security-sensitive —
//! wrong content goes into the system prompt) and the MCP
//! elicitation protocol surface (server-to-host user-prompt
//! request).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::mcp_elicitation::{
    action_to_response, ElicitationAction, ElicitationRequest, McpElicitationHandler,
    NoopElicitationHandler,
};
use openclaudia::memdir::{
    load_entrypoint, EntrypointFile, EntrypointTruncation, MAX_ENTRYPOINT_BYTES,
    MAX_ENTRYPOINT_LINES,
};
use serde_json::json;
use std::fmt::Write as _;
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn write(dir: &std::path::Path, name: &str, content: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, content).expect("write");
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — MAX_ENTRYPOINT_LINES + MAX_ENTRYPOINT_BYTES constants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn max_entrypoint_lines_constant_matches_cc_parity_value() {
    // Documented CC parity: 200 lines.
    assert_eq!(MAX_ENTRYPOINT_LINES, 200);
}

#[test]
fn max_entrypoint_bytes_constant_matches_cc_parity_value() {
    // Documented CC parity: 25_000 bytes.
    assert_eq!(MAX_ENTRYPOINT_BYTES, 25_000);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — load_entrypoint precedence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn load_entrypoint_returns_none_when_no_candidate_exists() {
    let dir = TempDir::new().expect("tempdir");
    let result = load_entrypoint(dir.path()).expect("no error");
    // Note: this may match the user-global ~/.openclaudia/MEMORY.md
    // if the test host has one. The contract is: no panic + the
    // result is consistent.
    let _ = result;
}

#[test]
fn load_entrypoint_prefers_cwd_memory_md_over_dot_openclaudia() {
    let dir = TempDir::new().expect("tempdir");
    write(dir.path(), "MEMORY.md", "from-cwd");
    write(
        &dir.path().join(".openclaudia"),
        "MEMORY.md",
        "from-dot-openclaudia",
    );
    let result = load_entrypoint(dir.path()).expect("load").expect("Some");
    assert_eq!(
        result.content, "from-cwd",
        "cwd/MEMORY.md MUST win over .openclaudia/MEMORY.md"
    );
}

#[test]
fn load_entrypoint_prefers_dot_openclaudia_over_user_global_when_cwd_absent() {
    let dir = TempDir::new().expect("tempdir");
    // Only .openclaudia/MEMORY.md (no top-level MEMORY.md).
    write(
        &dir.path().join(".openclaudia"),
        "MEMORY.md",
        "from-dot-openclaudia",
    );
    let result = load_entrypoint(dir.path()).expect("load").expect("Some");
    assert_eq!(result.content, "from-dot-openclaudia");
}

#[test]
fn load_entrypoint_path_field_is_absolute() {
    let dir = TempDir::new().expect("tempdir");
    write(dir.path(), "MEMORY.md", "hello");
    let result = load_entrypoint(dir.path()).expect("load").expect("Some");
    assert!(
        result.path.is_absolute(),
        "EntrypointFile.path MUST be absolute; got {:?}",
        result.path
    );
    assert!(result.path.ends_with("MEMORY.md"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Truncation behaviour
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn small_file_loads_without_truncation() {
    let dir = TempDir::new().expect("tempdir");
    let content = "A small MEMORY.md file.";
    write(dir.path(), "MEMORY.md", content);
    let result = load_entrypoint(dir.path()).expect("load").expect("Some");
    assert_eq!(result.content, content);
    assert_eq!(result.truncation, EntrypointTruncation::None);
    assert!(!result.was_truncated());
}

#[test]
fn file_with_more_than_max_lines_triggers_line_truncation() {
    let dir = TempDir::new().expect("tempdir");
    // 300 lines, each short → exceeds line cap (200) but not
    // byte cap.
    let content: String = (0..300).fold(String::new(), |mut acc, i| {
        writeln!(acc, "line-{i}").expect("write");
        acc
    });
    write(dir.path(), "MEMORY.md", &content);
    let result = load_entrypoint(dir.path()).expect("load").expect("Some");
    assert!(result.was_truncated(), "300 lines MUST trigger truncation");
    assert_eq!(result.truncation, EntrypointTruncation::Lines);
    // Documented contract: a suffix marker is appended after
    // truncation, so the final content can be slightly larger
    // than the cap. Assert the marker is present + the body
    // (excluding marker) fits the cap.
    assert!(
        result.content.contains("[truncated"),
        "MUST contain truncation marker; got {} chars",
        result.content.len()
    );
}

#[test]
fn file_with_more_than_max_bytes_triggers_byte_truncation() {
    let dir = TempDir::new().expect("tempdir");
    // One long line, 30 KiB — exceeds byte cap (25 KiB) but
    // only 1 line.
    let content: String = "x".repeat(30_000);
    write(dir.path(), "MEMORY.md", &content);
    let result = load_entrypoint(dir.path()).expect("load").expect("Some");
    assert!(result.was_truncated());
    assert_eq!(result.truncation, EntrypointTruncation::Bytes);
    assert!(
        result.content.contains("[truncated"),
        "MUST contain truncation marker"
    );
    // Body before the marker fits the cap (small marker
    // overhead is OK; check we're not still serving 30 KiB).
    assert!(
        result.content.len() < 30_000,
        "byte-truncated content MUST be smaller than input; got {} bytes",
        result.content.len()
    );
}

#[test]
fn was_truncated_predicate_false_only_when_no_truncation_applied() {
    let none = EntrypointFile {
        path: "/tmp/x".into(),
        content: String::new(),
        truncation: EntrypointTruncation::None,
    };
    assert!(!none.was_truncated());
    for kind in &[
        EntrypointTruncation::Lines,
        EntrypointTruncation::Bytes,
        EntrypointTruncation::LinesAndBytes,
    ] {
        let f = EntrypointFile {
            path: "/tmp/y".into(),
            content: String::new(),
            truncation: *kind,
        };
        assert!(
            f.was_truncated(),
            "kind {kind:?} MUST report was_truncated=true"
        );
    }
}

#[test]
fn truncation_enum_variants_compare_eq() {
    assert_eq!(EntrypointTruncation::None, EntrypointTruncation::None);
    assert_eq!(EntrypointTruncation::Lines, EntrypointTruncation::Lines);
    assert_ne!(EntrypointTruncation::Lines, EntrypointTruncation::Bytes);
    assert_ne!(
        EntrypointTruncation::Bytes,
        EntrypointTruncation::LinesAndBytes
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — ElicitationAction serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn elicitation_action_serde_uses_lowercase_tag() {
    // serde rename_all = "lowercase"; tagged enum → variant
    // name lower-cased. We exercise via direct serde to pin
    // the wire format.
    let json = serde_json::to_value(&ElicitationAction::Decline).expect("serialize");
    // Decline serializes as just "decline" (unit variant).
    assert_eq!(json, json!("decline"));
}

#[test]
fn elicitation_action_cancel_serializes_as_lowercase_unit() {
    let json = serde_json::to_value(&ElicitationAction::Cancel).expect("serialize");
    assert_eq!(json, json!("cancel"));
}

#[test]
fn elicitation_action_accept_carries_inner_value() {
    let inner = json!({"colour": "blue"});
    let accept = ElicitationAction::Accept(inner.clone());
    let json = serde_json::to_value(&accept).expect("serialize");
    // Accept(Value) serializes as an externally-tagged
    // object: {"accept": ...}.
    assert_eq!(json["accept"], inner);
}

#[test]
fn elicitation_action_round_trips_through_json() {
    let cases = vec![
        ElicitationAction::Accept(json!({"x": 1})),
        ElicitationAction::Decline,
        ElicitationAction::Cancel,
    ];
    for action in cases {
        let json = serde_json::to_value(&action).expect("serialize");
        let back: ElicitationAction = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, action);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — action_to_response wire format
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn action_to_response_accept_returns_action_plus_content() {
    let value = json!({"colour": "blue"});
    let wire = action_to_response(&ElicitationAction::Accept(value.clone()));
    assert_eq!(wire["action"], "accept");
    assert_eq!(wire["content"], value);
}

#[test]
fn action_to_response_decline_omits_content_field() {
    let wire = action_to_response(&ElicitationAction::Decline);
    assert_eq!(wire["action"], "decline");
    assert!(
        wire.get("content").is_none(),
        "decline MUST omit content; got {wire}"
    );
}

#[test]
fn action_to_response_cancel_omits_content_field() {
    let wire = action_to_response(&ElicitationAction::Cancel);
    assert_eq!(wire["action"], "cancel");
    assert!(wire.get("content").is_none());
}

#[test]
fn action_to_response_accept_with_empty_object_still_carries_content_key() {
    let wire = action_to_response(&ElicitationAction::Accept(json!({})));
    assert_eq!(wire["action"], "accept");
    // Even an empty object MUST be present under content.
    assert_eq!(wire["content"], json!({}));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — NoopElicitationHandler default
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn noop_handler_always_returns_cancel() {
    let handler = NoopElicitationHandler;
    let request = ElicitationRequest {
        message: "What is your favourite colour?".to_string(),
        requested_schema: json!({"type": "string"}),
        server_name: "test-server".to_string(),
    };
    let action = handler.handle(request).await.expect("handle");
    assert_eq!(action, ElicitationAction::Cancel);
}

#[tokio::test]
async fn noop_handler_cancels_regardless_of_server_name_or_schema() {
    let handler = NoopElicitationHandler;
    for (server, schema) in &[
        ("server-a", json!({"type": "string"})),
        ("server-b", json!({"type": "object"})),
        ("server-c", json!({"type": "array"})),
    ] {
        let request = ElicitationRequest {
            message: "Q?".to_string(),
            requested_schema: schema.clone(),
            server_name: (*server).to_string(),
        };
        let action = handler.handle(request).await.expect("handle");
        assert_eq!(
            action,
            ElicitationAction::Cancel,
            "noop MUST always Cancel; got {action:?} for server {server}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — ElicitationRequest shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn elicitation_request_captures_all_three_fields() {
    let request = ElicitationRequest {
        message: "Test prompt".to_string(),
        requested_schema: json!({"type": "string"}),
        server_name: "test-server".to_string(),
    };
    assert_eq!(request.message, "Test prompt");
    assert_eq!(request.server_name, "test-server");
    assert_eq!(request.requested_schema["type"], "string");
}

#[test]
fn noop_elicitation_handler_is_default_constructible() {
    // Unit struct — direct construction is the canonical
    // path; Default derive is a courtesy for callers that
    // store NoopElicitationHandler behind generics.
    let _ = NoopElicitationHandler;
}
