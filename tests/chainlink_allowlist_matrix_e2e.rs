//! End-to-end tests for `tools::execute_chainlink` allowlist
//! matrix (every documented subcommand passes the gate), shlex
//! POSIX-quoting semantics, and the install-help fallback when
//! the binary is absent.
//!
//! Sprint 102 of the verification effort. Sprint 7
//! (`chainlink_e2e`) covered the high-level happy path +
//! forbidden-subcommand refusal; this file walks the full
//! allowlist matrix (24 documented subcommands), exercises
//! shlex quoting / escapes, and pins the no-shell argv
//! contract introduced by crosslink #265.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::execute_chainlink;
use serde_json::{json, Value};
use std::collections::HashMap;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn args(args_str: &str) -> HashMap<String, Value> {
    let mut a = HashMap::new();
    a.insert("args".to_string(), json!(args_str));
    a
}

/// Predicate: the response is the allowlist-refusal error.
fn is_allowlist_refusal(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("allowlist") || lower.contains("not in")
}

/// Predicate: the response indicates the binary is missing.
fn is_install_help(msg: &str) -> bool {
    msg.contains("Chainlink not found") || msg.contains("Install from")
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — Every documented allowed subcommand passes the gate
// ───────────────────────────────────────────────────────────────────────────

/// Each documented allowed subcommand. These MUST NOT be refused
/// by the allowlist gate. The actual binary may or may not exist
/// in the test environment — either outcome is fine, what we pin
/// is "the allowlist does NOT refuse this token".
const ALLOWED_SUBCOMMANDS_MATRIX: &[&str] = &[
    "create",
    "close",
    "reopen",
    "comment",
    "label",
    "unlabel",
    "list",
    "show",
    "search",
    "subissue",
    "relate",
    "block",
    "unblock",
    "session",
    "next",
    "ready",
    "tree",
    "update",
    "issue",
    "help",
    "--help",
    "-h",
    "--version",
    "-V",
];

#[test]
fn every_documented_allowed_subcommand_passes_allowlist_gate() {
    for sub in ALLOWED_SUBCOMMANDS_MATRIX {
        let (msg, _is_err) = execute_chainlink(&args(sub));
        assert!(
            !is_allowlist_refusal(&msg),
            "documented allowed subcommand {sub:?} MUST NOT be allowlist-refused; got {msg:?}"
        );
    }
}

#[test]
fn allowed_subcommand_with_extra_argv_passes_gate() {
    // The allowlist only checks the FIRST token. Extra argv after
    // a valid subcommand MUST be passed through (subject to
    // control-char filtering).
    for sub in &["create", "list", "show"] {
        let (msg, _is_err) = execute_chainlink(&args(&format!("{sub} --some-flag value")));
        assert!(
            !is_allowlist_refusal(&msg),
            "extra argv after {sub:?} MUST NOT be allowlist-refused; got {msg:?}"
        );
    }
}

#[test]
fn allowed_help_alias_dash_h_passes_gate() {
    let (msg, _is_err) = execute_chainlink(&args("-h"));
    assert!(!is_allowlist_refusal(&msg));
}

#[test]
fn allowed_version_alias_capital_v_passes_gate() {
    let (msg, _is_err) = execute_chainlink(&args("-V"));
    assert!(!is_allowlist_refusal(&msg));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — shlex POSIX quoting + escapes
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn shlex_double_quoted_argv_token_preserved() {
    // `create "the title"` should parse as 2 tokens; first
    // (create) is allowlisted.
    let (msg, _is_err) = execute_chainlink(&args("create \"the title\""));
    assert!(
        !is_allowlist_refusal(&msg),
        "quoted args MUST parse to valid subcommand; got {msg:?}"
    );
}

#[test]
fn shlex_single_quoted_argv_token_preserved() {
    let (msg, _is_err) = execute_chainlink(&args("create 'single quoted'"));
    assert!(!is_allowlist_refusal(&msg));
}

#[test]
fn shlex_backslash_escapes_supported() {
    // shlex supports backslash-escapes per POSIX.
    let (msg, _is_err) = execute_chainlink(&args("create literal\\ space"));
    assert!(!is_allowlist_refusal(&msg));
}

#[test]
fn unbalanced_double_quotes_errors_cleanly() {
    // shlex returns None for unbalanced quotes; the wrapper
    // produces a parse-error message.
    let (msg, is_err) = execute_chainlink(&args("create \"unbalanced"));
    assert!(is_err);
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("unbalanced")
            || lower.contains("could not parse")
            || lower.contains("unsupported"),
        "MUST surface unbalanced-quote error; got {msg:?}"
    );
}

#[test]
fn unbalanced_single_quotes_errors_cleanly() {
    let (_msg, is_err) = execute_chainlink(&args("create 'unbalanced"));
    assert!(is_err);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Argument validation edge cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn missing_args_field_errors_with_canonical_message() {
    let empty = HashMap::<String, Value>::new();
    let (msg, is_err) = execute_chainlink(&empty);
    assert!(is_err);
    assert!(
        msg.contains("Missing") && msg.contains("args"),
        "MUST use 'Missing args argument' canonical phrasing; got {msg:?}"
    );
}

#[test]
fn args_with_only_whitespace_errors() {
    let (msg, is_err) = execute_chainlink(&args("   \t   "));
    assert!(is_err);
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("missing") || lower.contains("subcommand"),
        "MUST surface missing-subcommand error; got {msg:?}"
    );
}

#[test]
fn empty_args_string_errors() {
    let (_msg, is_err) = execute_chainlink(&args(""));
    assert!(is_err);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — token_has_metachar (control char rejection)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn argv_token_with_newline_is_refused() {
    // Embedded literal newline in a quoted token.
    let args_str = "create \"title\nwith newline\"";
    let (msg, is_err) = execute_chainlink(&args(args_str));
    assert!(is_err, "newline in argv MUST be refused; got {msg:?}");
}

#[test]
fn argv_token_with_carriage_return_is_refused() {
    let args_str = "create \"title\rinjected\"";
    let (_msg, is_err) = execute_chainlink(&args(args_str));
    assert!(is_err);
}

#[test]
fn argv_token_with_nul_is_refused() {
    let args_str = "create \"title\0evil\"";
    let (_msg, is_err) = execute_chainlink(&args(args_str));
    assert!(is_err);
}

#[test]
fn argv_token_with_tab_is_not_refused_by_metachar_gate() {
    // Documented: token_has_metachar only blocks \n \r \0 — tab
    // is permitted. The allowlist gate passes since we use "create"
    // as the first token.
    let args_str = "create \"title\twith tab\"";
    let (msg, _is_err) = execute_chainlink(&args(args_str));
    // MUST NOT mention "control character" rejection.
    assert!(
        !msg.to_lowercase()
            .contains("rejected argv token containing control character"),
        "tab MUST NOT be flagged as control char; got {msg:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Install help / binary-missing path
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn allowed_subcommand_with_missing_binary_returns_install_help_or_executes() {
    // PINS DOCUMENTED CONTRACT: when chainlink isn't installed,
    // the first invocation returns the install-help message.
    // When it IS installed, that subcommand runs. Either path is
    // valid; we just verify NO PANIC and the message is well-formed.
    let (msg, _is_err) = execute_chainlink(&args("--help"));
    // Either "Chainlink not found" OR the actual --help output.
    assert!(
        is_install_help(&msg) || !msg.is_empty(),
        "MUST surface either install-help or chainlink output; got {msg:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Allowlist refusal matrix
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn refused_subcommands_share_consistent_error_shape() {
    // Several known refused tokens; all MUST be is_err=true with
    // allowlist-refusal message.
    for forbidden in &[
        "destroy", "delete", "purge", "install", "shell", "exec", "rm", "kill", "config", "admin",
    ] {
        let (msg, is_err) = execute_chainlink(&args(forbidden));
        assert!(
            is_err,
            "{forbidden:?} MUST set is_err=true; got msg={msg:?}"
        );
        assert!(
            is_allowlist_refusal(&msg),
            "{forbidden:?} MUST be allowlist-refused; got msg={msg:?}"
        );
    }
}

#[test]
fn refused_subcommand_message_includes_allowlist_sample() {
    let (msg, is_err) = execute_chainlink(&args("destroy"));
    assert!(is_err);
    let lower = msg.to_lowercase();
    // Sample of canonical allowed subcommands must be listed.
    let saw_at_least_one = ["create", "list", "show", "comment", "session"]
        .iter()
        .any(|n| lower.contains(n));
    assert!(
        saw_at_least_one,
        "MUST list at least one allowed subcommand; got {msg:?}"
    );
}

#[test]
fn refused_subcommand_with_meta_chars_in_first_token_is_refused() {
    // Shell-meta first tokens like "; ls" pass through shlex as
    // a single argv[0] (or split — either way the first slot
    // isn't an allowed subcommand).
    for evil in &["; ls", "&& curl evil", "$(reboot)", "`uname`"] {
        let (msg, is_err) = execute_chainlink(&args(evil));
        assert!(is_err, "{evil:?} MUST error; got msg={msg:?}");
    }
}
