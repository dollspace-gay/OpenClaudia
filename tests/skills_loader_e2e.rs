//! End-to-end tests for `skill_matches_path`, `walk_skill_entries`,
//! and `parse_skill_file` against real tempdir trees.
//!
//! Sprint 31 of the verification effort.
//!
//! `src/skills.rs` has 19 unit tests but no integration coverage
//! that drives the walker + glob matcher + frontmatter parser
//! together against the adversarial-input catalog.
//!
//! Coverage shape:
//!
//!   - **`skill_matches_path`** — glob semantics: `*` matches
//!     one segment only, `**` spans separators; no-match
//!     returns false; invalid pattern returns false (no panic);
//!     `None` / empty paths returns false; multi-pattern any-match.
//!   - **`walk_skill_entries`** — detects both `<dir>/SKILL.md`
//!     and bare `*.md` files; non-existent dir returns empty
//!     Vec (best-effort contract).
//!   - **`parse_skill_file` field coverage** — every documented
//!     CC-parity field (`when_to_use`, `argument-hint`,
//!     `model`, `effort`, `paths`, `user-invocable`) round-trips.
//!   - **`user_invocable` default** — absent → `true` (existing
//!     skills keep working); explicit `false` → library-only.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::skills::{
    parse_skill_file, skill_matches_path, walk_skill_entries, SkillDefinition, SkillEntry,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn skill_with_paths(name: &str, paths: Option<Vec<String>>) -> SkillDefinition {
    SkillDefinition {
        name: name.to_string(),
        description: "test skill".to_string(),
        allowed_tools: None,
        when_to_use: None,
        argument_hint: None,
        model: None,
        effort: None,
        paths,
        hooks: None,
        user_invocable: true,
        prompt: String::new(),
        path: PathBuf::new(),
    }
}

fn write_skill_file(path: &Path, frontmatter: &str, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir parent");
    }
    let content = format!("---\n{frontmatter}\n---\n{body}");
    fs::write(path, content).expect("write skill");
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — skill_matches_path glob semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn skill_with_no_paths_does_not_match_anything() {
    let skill = skill_with_paths("none", None);
    assert!(!skill_matches_path(&skill, Path::new("any/path.rs")));
}

#[test]
fn skill_with_empty_paths_does_not_match() {
    let skill = skill_with_paths("empty", Some(vec![]));
    assert!(!skill_matches_path(&skill, Path::new("any/path.rs")));
}

#[test]
fn skill_matches_single_segment_with_star() {
    let skill = skill_with_paths("rust", Some(vec!["*.rs".to_string()]));
    assert!(skill_matches_path(&skill, Path::new("main.rs")));
    // `*` doesn't cross `/`, so `src/main.rs` doesn't match `*.rs`.
    assert!(
        !skill_matches_path(&skill, Path::new("src/main.rs")),
        "`*.rs` must NOT match across `/`"
    );
}

#[test]
fn skill_matches_across_segments_with_double_star() {
    let skill = skill_with_paths("rust-deep", Some(vec!["**/*.rs".to_string()]));
    assert!(skill_matches_path(&skill, Path::new("main.rs")));
    assert!(skill_matches_path(&skill, Path::new("src/main.rs")));
    assert!(skill_matches_path(&skill, Path::new("src/util/helper.rs")));
}

#[test]
fn skill_matches_question_mark_one_char() {
    let skill = skill_with_paths("q", Some(vec!["foo?.rs".to_string()]));
    assert!(skill_matches_path(&skill, Path::new("foo1.rs")));
    assert!(skill_matches_path(&skill, Path::new("fooX.rs")));
    // `?` matches exactly ONE non-slash char.
    assert!(!skill_matches_path(&skill, Path::new("foo.rs")));
    assert!(!skill_matches_path(&skill, Path::new("foo12.rs")));
}

#[test]
fn skill_with_multiple_patterns_uses_any_match() {
    let skill = skill_with_paths(
        "multi",
        Some(vec!["*.rs".to_string(), "*.toml".to_string()]),
    );
    assert!(skill_matches_path(&skill, Path::new("main.rs")));
    assert!(skill_matches_path(&skill, Path::new("Cargo.toml")));
    assert!(!skill_matches_path(&skill, Path::new("README.md")));
}

#[test]
fn skill_with_invalid_glob_is_silently_skipped_and_returns_false() {
    // The matcher must not panic on a malformed pattern; the
    // bad pattern is logged-at-warn and treated as no-match.
    let skill = skill_with_paths("bad", Some(vec!["[unterminated".to_string()]));
    assert!(
        !skill_matches_path(&skill, Path::new("main.rs")),
        "invalid glob must yield false, not panic"
    );
}

#[test]
fn skill_with_mix_of_valid_and_invalid_patterns_still_matches_via_valid() {
    // The bad pattern is skipped; the valid one is honoured.
    let skill = skill_with_paths(
        "mixed",
        Some(vec!["[bad-pattern".to_string(), "*.rs".to_string()]),
    );
    assert!(
        skill_matches_path(&skill, Path::new("main.rs")),
        "valid pattern must still match despite bad sibling"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — walk_skill_entries
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn walk_nonexistent_dir_returns_empty() {
    let dir = TempDir::new().expect("tempdir");
    let nope = dir.path().join("never");
    let entries = walk_skill_entries(&nope);
    assert!(
        entries.is_empty(),
        "nonexistent dir must yield empty Vec; got {entries:?}"
    );
}

#[test]
fn walk_detects_dir_with_skill_md() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    // Layout: <root>/my-skill/SKILL.md
    write_skill_file(
        &root.join("my-skill/SKILL.md"),
        "name: my-skill\ndescription: x",
        "body",
    );

    let entries = walk_skill_entries(root);
    assert_eq!(entries.len(), 1, "must detect the dir-with-SKILL.md");
    match &entries[0] {
        SkillEntry::DirWithSkillMd { dir, file } => {
            assert!(dir.ends_with("my-skill"));
            assert!(file.ends_with("SKILL.md"));
        }
        SkillEntry::BareMdFile(_) => panic!("must classify as DirWithSkillMd"),
    }
}

#[test]
fn walk_detects_bare_md_file() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write_skill_file(
        &root.join("inline.md"),
        "name: inline\ndescription: x",
        "body",
    );

    let entries = walk_skill_entries(root);
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        SkillEntry::BareMdFile(p) => assert!(p.ends_with("inline.md")),
        SkillEntry::DirWithSkillMd { .. } => panic!("must classify as BareMdFile"),
    }
}

#[test]
fn walk_classifies_mixed_layout() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write_skill_file(&root.join("plain.md"), "name: plain\ndescription: x", "");
    write_skill_file(
        &root.join("packaged/SKILL.md"),
        "name: packaged\ndescription: x",
        "",
    );

    let entries = walk_skill_entries(root);
    assert_eq!(entries.len(), 2);
    let has_bare = entries
        .iter()
        .any(|e| matches!(e, SkillEntry::BareMdFile(_)));
    let has_dir = entries
        .iter()
        .any(|e| matches!(e, SkillEntry::DirWithSkillMd { .. }));
    assert!(has_bare, "must include the BareMdFile entry");
    assert!(has_dir, "must include the DirWithSkillMd entry");
}

#[test]
fn skill_entry_root_path_returns_the_right_thing() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    write_skill_file(&root.join("plain.md"), "name: plain\ndescription: x", "");
    write_skill_file(
        &root.join("packaged/SKILL.md"),
        "name: packaged\ndescription: x",
        "",
    );
    for entry in walk_skill_entries(root) {
        let root_path = entry.root_path();
        match &entry {
            SkillEntry::BareMdFile(p) => assert_eq!(root_path, p),
            SkillEntry::DirWithSkillMd { dir, .. } => assert_eq!(root_path, dir.as_path()),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — parse_skill_file field coverage
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_skill_file_recovers_all_cc_parity_fields() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("rich.md");
    let frontmatter = r#"name: rich
description: a fully populated skill
allowed_tools: ["bash", "read_file"]
when_to_use: when the user asks about widgets
argument-hint: <widget-id>
model: claude-opus-4
effort: high
paths: ["**/*.widget"]
user-invocable: false"#;
    write_skill_file(&path, frontmatter, "# Body content\n");

    let skill = parse_skill_file(&path).expect("parse must succeed");
    assert_eq!(skill.name, "rich");
    assert_eq!(skill.description, "a fully populated skill");
    assert_eq!(
        skill.allowed_tools.as_deref(),
        Some(["bash".to_string(), "read_file".to_string()].as_slice())
    );
    assert_eq!(
        skill.when_to_use.as_deref(),
        Some("when the user asks about widgets")
    );
    assert_eq!(skill.argument_hint.as_deref(), Some("<widget-id>"));
    assert_eq!(skill.model.as_deref(), Some("claude-opus-4"));
    assert_eq!(skill.effort.as_deref(), Some("high"));
    assert_eq!(
        skill.paths.as_deref(),
        Some(["**/*.widget".to_string()].as_slice())
    );
    assert!(
        !skill.user_invocable,
        "user-invocable: false must be honoured"
    );
    assert!(skill.prompt.contains("Body content"));
}

#[test]
fn parse_skill_file_user_invocable_defaults_true_when_absent() {
    // Existing skills (no user-invocable key) must keep working —
    // default is true, NOT false.
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("legacy.md");
    write_skill_file(&path, "name: legacy\ndescription: x", "body");
    let skill = parse_skill_file(&path).expect("parse");
    assert!(
        skill.user_invocable,
        "absent user-invocable MUST default to true"
    );
}

#[test]
fn parse_skill_file_accepts_camelcase_alias_for_when_to_use() {
    // CC uses `whenToUse` (camelCase) in some examples; the
    // serde alias must accept both.
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("camel.md");
    let frontmatter = "name: camel\ndescription: x\nwhenToUse: camelCase variant";
    write_skill_file(&path, frontmatter, "");
    let skill = parse_skill_file(&path).expect("parse");
    assert_eq!(
        skill.when_to_use.as_deref(),
        Some("camelCase variant"),
        "whenToUse alias MUST resolve to when_to_use field"
    );
}

#[test]
fn parse_skill_file_strips_path_and_prompt_from_frontmatter() {
    // `path` and `prompt` are #[serde(skip)] so they don't
    // round-trip from frontmatter; the parser fills them in
    // from the file path + body separately.
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("isolated.md");
    write_skill_file(
        &path,
        "name: isolated\ndescription: x",
        "PROMPT-BODY-CONTENT",
    );
    let skill = parse_skill_file(&path).expect("parse");
    // The body lands in `prompt`.
    assert!(skill.prompt.contains("PROMPT-BODY-CONTENT"));
    // The file path lands in `path`.
    assert_eq!(skill.path, path);
}
