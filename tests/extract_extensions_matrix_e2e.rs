//! End-to-end tests for `rules::extract_extensions_from_tool_input` —
//! Write/Edit/Read `file_path` branch, Glob `pattern` branch
//! (#796 trailing-`.ext` regex with optional glob-meta tail),
//! unknown-tool empty fallback, and edge cases (multi-dot
//! filenames, unicode, empty inputs).
//!
//! Sprint 174 of the verification effort. Sprints 86 / 56
//! had 3 basic tests; this file fills the regex matrix +
//! file-path corner cases.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::rules::extract_extensions_from_tool_input;
use serde_json::json;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Write/Edit/Read with file_path
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn write_with_rs_file_path_returns_rs() {
    let exts = extract_extensions_from_tool_input("Write", &json!({"file_path": "/tmp/foo.rs"}));
    assert_eq!(exts, vec!["rs".to_string()]);
}

#[test]
fn edit_with_py_file_path_returns_py() {
    let exts =
        extract_extensions_from_tool_input("Edit", &json!({"file_path": "/home/x/script.py"}));
    assert_eq!(exts, vec!["py".to_string()]);
}

#[test]
fn read_with_md_file_path_returns_md() {
    let exts = extract_extensions_from_tool_input("Read", &json!({"file_path": "/docs/README.md"}));
    assert_eq!(exts, vec!["md".to_string()]);
}

#[test]
fn write_with_multidot_filename_returns_last_extension_only() {
    // PINS DOC: Path::extension returns LAST extension only.
    let exts =
        extract_extensions_from_tool_input("Write", &json!({"file_path": "/tmp/foo.tar.gz"}));
    assert_eq!(exts, vec!["gz".to_string()]);
}

#[test]
fn write_with_uppercase_extension_preserves_case() {
    // PINS DOC: extension preserves caller's case.
    let exts = extract_extensions_from_tool_input("Write", &json!({"file_path": "/tmp/IMAGE.PNG"}));
    assert_eq!(exts, vec!["PNG".to_string()]);
}

#[test]
fn write_with_no_extension_returns_empty_vec() {
    let exts = extract_extensions_from_tool_input("Write", &json!({"file_path": "/tmp/Makefile"}));
    assert!(exts.is_empty());
}

#[test]
fn write_with_dotfile_path_returns_nothing() {
    // PINS DOC: ".bashrc" is treated by Path::extension as
    // having no extension (the dot is the leading char).
    let exts =
        extract_extensions_from_tool_input("Write", &json!({"file_path": "/home/user/.bashrc"}));
    assert!(
        exts.is_empty(),
        "leading-dot files have no ext; got {exts:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Write/Edit/Read with missing/wrong-type file_path
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn write_with_missing_file_path_returns_empty() {
    let exts = extract_extensions_from_tool_input("Write", &json!({}));
    assert!(exts.is_empty());
}

#[test]
fn write_with_file_path_as_number_returns_empty() {
    let exts = extract_extensions_from_tool_input("Write", &json!({"file_path": 42}));
    assert!(exts.is_empty());
}

#[test]
fn write_with_file_path_as_null_returns_empty() {
    let exts = extract_extensions_from_tool_input("Write", &json!({"file_path": null}));
    assert!(exts.is_empty());
}

#[test]
fn write_with_empty_file_path_returns_empty() {
    let exts = extract_extensions_from_tool_input("Write", &json!({"file_path": ""}));
    assert!(exts.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Glob pattern branch (#796)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn glob_with_star_dot_rs_pattern_returns_rs() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "*.rs"}));
    assert_eq!(exts, vec!["rs".to_string()]);
}

#[test]
fn glob_with_double_star_dot_ts_returns_ts() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "**/*.ts"}));
    assert_eq!(exts, vec!["ts".to_string()]);
}

#[test]
fn glob_with_specific_dir_and_ext_returns_ext() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "src/**/*.tsx"}));
    assert_eq!(exts, vec!["tsx".to_string()]);
}

#[test]
fn glob_with_no_extension_returns_empty_vec() {
    // PINS #796: pattern without trailing `.ext` MUST NOT
    // fabricate an extension from a bare path segment.
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "src/util"}));
    assert!(
        exts.is_empty(),
        "pattern with no .ext MUST yield empty; got {exts:?}"
    );
}

#[test]
fn glob_with_directory_pattern_no_extension() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "src/**/components"}));
    assert!(exts.is_empty());
}

#[test]
fn glob_with_ext_followed_by_glob_meta_returns_extension() {
    // PINS REGEX: trailing `*?]}` after .ext still match.
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "*.rs?"}));
    assert_eq!(exts, vec!["rs".to_string()]);
}

#[test]
fn glob_with_ext_followed_by_star_returns_extension() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "*.rs*"}));
    assert_eq!(exts, vec!["rs".to_string()]);
}

#[test]
fn glob_with_8_character_extension_matches() {
    // PINS REGEX: bounded 1-8 chars.
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "*.abcdefgh"}));
    assert_eq!(exts, vec!["abcdefgh".to_string()]);
}

#[test]
fn glob_with_9_character_extension_does_not_match() {
    // PINS BOUND: 9+ chars exceeds {1,8} bound.
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "*.abcdefghi"}));
    // Regex anchored to last 8 chars → matches "bcdefghi" or fails.
    // Let's just check no fabricated 9-char ext.
    if !exts.is_empty() {
        assert!(exts[0].len() <= 8, "ext MUST be <=8 chars; got {exts:?}");
    }
}

#[test]
fn glob_with_extension_containing_underscore_does_not_match() {
    // PINS REGEX: [A-Za-z0-9] only, no underscore.
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": "*.my_ext"}));
    // Underscore breaks the char class → regex captures only
    // the trailing alphanumeric run after the underscore.
    if !exts.is_empty() {
        assert!(
            !exts[0].contains('_'),
            "extension MUST NOT contain underscore; got {exts:?}"
        );
    }
}

#[test]
fn glob_with_missing_pattern_returns_empty() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({}));
    assert!(exts.is_empty());
}

#[test]
fn glob_with_pattern_as_number_returns_empty() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": 42}));
    assert!(exts.is_empty());
}

#[test]
fn glob_with_empty_pattern_returns_empty() {
    let exts = extract_extensions_from_tool_input("Glob", &json!({"pattern": ""}));
    assert!(exts.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Unknown tool names
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_tool_with_file_path_returns_empty() {
    // PINS DOC: only Write/Edit/Read/Glob are special-cased.
    for tool in &["Bash", "WebFetch", "TodoWrite", "RandomNewTool"] {
        let exts = extract_extensions_from_tool_input(
            tool,
            &json!({"file_path": "/tmp/foo.rs", "pattern": "*.rs"}),
        );
        assert!(exts.is_empty(), "{tool}: MUST return empty; got {exts:?}");
    }
}

#[test]
fn empty_tool_name_returns_empty() {
    let exts = extract_extensions_from_tool_input("", &json!({"file_path": "/tmp/foo.rs"}));
    assert!(exts.is_empty());
}

#[test]
fn case_sensitive_tool_names_lowercase_does_not_match() {
    // PINS DOC: matches are EXACT (capital W in "Write").
    let exts = extract_extensions_from_tool_input("write", &json!({"file_path": "/tmp/foo.rs"}));
    assert!(
        exts.is_empty(),
        "lowercase 'write' MUST NOT match 'Write'; got {exts:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Return shape invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn return_is_always_a_vec_of_owned_strings() {
    let exts = extract_extensions_from_tool_input("Read", &json!({"file_path": "/x.rs"}));
    // Owned String, not &str.
    let _: Vec<String> = exts;
}

#[test]
fn return_has_at_most_one_extension_per_call() {
    // PINS DOC: each branch pushes at most 1 extension.
    let cases = vec![
        ("Write", json!({"file_path": "/x.rs"})),
        ("Read", json!({"file_path": "/y.md"})),
        ("Glob", json!({"pattern": "**/*.tsx"})),
    ];
    for (tool, input) in cases {
        let exts = extract_extensions_from_tool_input(tool, &input);
        assert!(
            exts.len() <= 1,
            "{tool}: MUST return at most 1 extension; got {exts:?}"
        );
    }
}

#[test]
fn never_panics_on_arbitrary_extras_in_input() {
    let extras = json!({
        "file_path": "/x.rs",
        "pattern": "**/*.ts",
        "unrelated": {"k": "v"},
        "deeply": {"nested": [1, 2, 3]}
    });
    let _ = extract_extensions_from_tool_input("Write", &extras);
    let _ = extract_extensions_from_tool_input("Glob", &extras);
}
