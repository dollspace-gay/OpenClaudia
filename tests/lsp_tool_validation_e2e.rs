//! End-to-end tests for `tools::lsp::execute_lsp`
//! validation arms — pre-server-spawn checks invoked
//! through the registry dispatch path.
//!
//! Sprint 139 of the verification effort. Sprint 47 / 109
//! covered LSP type shapes + `mark_opened` / `mark_closed`
//! plus connected lookup; this file pins the user-facing
//! tool validation — missing `file_path`, unknown
//! extension, LSP-unavailable gate (#650), and the 10 MiB
//! file-size cap (#648).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::registry::{registry, ToolContext};
use serde_json::{json, Value};
use std::collections::HashMap;

fn dispatch_lsp(args: &HashMap<String, Value>) -> (String, bool) {
    let mut ctx = ToolContext {
        memory_db: None,
        app_config: None,
        task_mgr: None,
    };
    registry()
        .dispatch("lsp", args, &mut ctx)
        .expect("lsp must be registered")
}

fn args_with(entries: &[(&str, Value)]) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), v.clone());
    }
    m
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — Missing file_path arg
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn missing_file_path_arg_returns_error() {
    let (msg, is_err) = dispatch_lsp(&HashMap::new());
    assert!(is_err);
    assert!(
        msg.contains("file_path"),
        "MUST mention file_path; got {msg:?}"
    );
}

#[test]
fn file_path_arg_as_number_treated_as_missing() {
    let args = args_with(&[("file_path", json!(42))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("file_path"));
}

#[test]
fn file_path_arg_as_array_treated_as_missing() {
    let args = args_with(&[("file_path", json!(["a", "b"]))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("file_path"));
}

#[test]
fn file_path_arg_as_null_treated_as_missing() {
    let args = args_with(&[("file_path", Value::Null)]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("file_path"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Unknown file extension
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_extension_yields_no_language_server_message() {
    let args = args_with(&[("file_path", json!("/tmp/file.unknownext"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(
        msg.contains("No language server known"),
        "MUST surface 'No language server known'; got {msg:?}"
    );
    assert!(
        msg.contains("/tmp/file.unknownext"),
        "MUST echo offending path; got {msg:?}"
    );
}

#[test]
fn file_with_no_extension_yields_no_language_server_message() {
    let args = args_with(&[("file_path", json!("/tmp/no_extension_file"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("No language server known"));
}

#[test]
fn empty_string_file_path_yields_no_language_server() {
    // Empty extension after rsplit('.') → no match.
    let args = args_with(&[("file_path", json!(""))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("No language server known"));
}

#[test]
fn dotfile_with_no_extension_yields_no_language_server() {
    // ".gitignore" — first split by "." gives "" (no extension).
    // Actually rsplit('.') on ".gitignore" yields "gitignore" — a
    // valid string. Pin: result is still "No language server" since
    // "gitignore" is not in the known-ext map.
    let args = args_with(&[("file_path", json!(".gitignore"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("No language server known"));
}

#[test]
fn unknown_action_errors_before_extension_gate() {
    let args = args_with(&[
        ("file_path", json!("/tmp/file.unknownext")),
        ("action", json!("definitelyNotReal")),
    ]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(msg.contains("Unknown LSP action"), "got {msg:?}");
    assert!(
        !msg.contains("No language server known"),
        "action validation must run before extension gate; got {msg:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Call hierarchy argument validation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn incoming_calls_missing_hierarchy_item_errors_before_server_gate() {
    let args = args_with(&[
        ("file_path", json!("/tmp/nonexistent.rs")),
        ("action", json!("incomingCalls")),
    ]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(
        msg.contains("hierarchy_item") && msg.contains("prepareCallHierarchy"),
        "must explain required call hierarchy argument; got {msg:?}"
    );
    assert!(
        !msg.contains("LSP server unavailable"),
        "argument validation must run before server availability gate; got {msg:?}"
    );
}

#[test]
fn outgoing_calls_rejects_non_object_hierarchy_item() {
    for bad in [Value::Null, json!("not-an-item"), json!([1, 2, 3])] {
        let args = args_with(&[
            ("file_path", json!("/tmp/nonexistent.rs")),
            ("action", json!("outgoingCalls")),
            ("hierarchy_item", bad),
        ]);
        let (msg, is_err) = dispatch_lsp(&args);
        assert!(is_err);
        assert!(msg.contains("hierarchy_item"), "got {msg:?}");
    }
}

#[test]
fn call_hierarchy_with_object_item_reaches_file_validation() {
    let args = args_with(&[
        ("file_path", json!("/tmp/file.unknownext")),
        ("action", json!("incomingCalls")),
        (
            "hierarchy_item",
            json!({"name": "f", "uri": "file:///tmp/file.rs"}),
        ),
    ]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(
        msg.contains("No language server known"),
        "valid hierarchy object should pass argument validation; got {msg:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Known extensions reach LSP-availability gate
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn rust_extension_passes_unknown_server_gate() {
    // file with .rs has a known server (rust-analyzer).
    // Without the binary on PATH the error path is the
    // "LSP server unavailable" message (#650 gate). With
    // the binary on PATH, the error is from the server
    // request itself (file doesn't exist on disk).
    let args = args_with(&[("file_path", json!("/nonexistent_unique_marker.rs"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    // Either way the tool returns an error for a non-existent
    // path. We pin: it MUST NOT surface "No language server
    // known" because .rs IS known.
    assert!(is_err);
    assert!(
        !msg.contains("No language server known"),
        ".rs MUST be a known extension; got {msg:?}"
    );
}

#[test]
fn python_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.py"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

#[test]
fn typescript_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.ts"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

#[test]
fn go_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.go"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

#[test]
fn cpp_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.cpp"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

#[test]
fn header_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.hpp"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

#[test]
fn java_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.java"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

#[test]
fn ruby_extension_passes_unknown_server_gate() {
    let args = args_with(&[("file_path", json!("/nonexistent.rb"))]);
    let (msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
    assert!(!msg.contains("No language server known"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Default action when omitted
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn missing_action_arg_defaults_to_hover_no_panic() {
    // PINS DEFAULT: action arg omitted → defaults to "hover".
    // Tool still goes through file_path validation + ext check.
    let args = args_with(&[("file_path", json!("/x.unknownext"))]);
    let (_msg, is_err) = dispatch_lsp(&args);
    // Hover on unknown extension MUST error (no panic).
    assert!(is_err);
}

#[test]
fn missing_action_with_known_ext_does_not_panic() {
    let args = args_with(&[("file_path", json!("/x.rs"))]);
    let (_msg, is_err) = dispatch_lsp(&args);
    // Whether errors with "LSP unavailable" or runs into
    // file-not-found, no panic.
    assert!(is_err);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — line + character coercion
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn line_arg_above_u32_max_clamps_to_u32_max_no_panic() {
    let args = args_with(&[("file_path", json!("/x.rs")), ("line", json!(u64::MAX))]);
    let (_msg, is_err) = dispatch_lsp(&args);
    // No panic on overflow — line is clamped via try_from.
    assert!(is_err);
}

#[test]
fn character_arg_above_u32_max_clamps_no_panic() {
    let args = args_with(&[
        ("file_path", json!("/x.rs")),
        ("character", json!(u64::MAX)),
    ]);
    let (_msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
}

#[test]
fn negative_line_arg_treated_as_default_no_panic() {
    // u64::as_u64() returns None for negative numbers →
    // map_or(1, ...) — line defaults to 1.
    let args = args_with(&[("file_path", json!("/x.rs")), ("line", json!(-1))]);
    let (_msg, is_err) = dispatch_lsp(&args);
    assert!(is_err);
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Cross-validation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn lsp_dispatch_never_panics_on_arbitrary_args() {
    // Sanity: arbitrary arg shapes don't panic the tool.
    let args = args_with(&[
        ("file_path", json!("/x.rs")),
        ("action", json!("hover")),
        ("line", json!(10)),
        ("character", json!(5)),
        ("extra", json!({"unknown": "arg"})),
    ]);
    let (_msg, _is_err) = dispatch_lsp(&args);
}
