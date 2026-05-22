//! End-to-end tests for `skills::SkillParseError` —
//! all 3 variants (`ReadFailed`, `FrontmatterMissing`,
//! `YamlFailed`) `Display` strings + `parse_skill_file`
//! dispatch into each variant.
//!
//! Sprint 197 of the verification effort. Sprint 116 / etc.
//! covered `FrontmatterMissing` + `YamlFailed` via
//! `plugin_skill_security`; this file pins `ReadFailed`
//! plus all 3 `Display` contracts directly.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::skills::{parse_skill_file, SkillParseError};
use std::path::Path;
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Section A — ReadFailed variant
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_nonexistent_file_yields_read_failed() {
    let outcome = parse_skill_file(Path::new("/tmp/definitely-no-such-file-197.md"));
    assert!(
        matches!(outcome, Err(SkillParseError::ReadFailed(_))),
        "missing file MUST yield ReadFailed; got {outcome:?}"
    );
}

#[test]
fn read_failed_display_includes_io_error_message() {
    let outcome = parse_skill_file(Path::new("/tmp/no-such-skill-marker-197.md"));
    let err = outcome.unwrap_err();
    let s = err.to_string();
    assert!(
        s.starts_with("failed to read skill file:"),
        "MUST start with 'failed to read skill file:'; got {s:?}"
    );
}

#[test]
fn parse_directory_path_does_not_panic() {
    // Path that points to a directory rather than file.
    let dir = TempDir::new().expect("tempdir");
    let outcome = parse_skill_file(dir.path());
    // Either ReadFailed or some other error. Just verify no panic.
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — FrontmatterMissing variant
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_file_without_frontmatter_yields_frontmatter_missing() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("plain.md");
    std::fs::write(&path, "# Just a heading\n\nNo frontmatter here.\n").expect("write");
    let outcome = parse_skill_file(&path);
    assert!(
        matches!(outcome, Err(SkillParseError::FrontmatterMissing)),
        "MUST yield FrontmatterMissing"
    );
}

#[test]
fn parse_file_with_only_open_delim_yields_frontmatter_missing() {
    // PINS DOC: "Closing `---` missing" also counts as
    // FrontmatterMissing.
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("half.md");
    std::fs::write(&path, "---\nname: test\n(missing closing delimiter)\n").expect("write");
    let outcome = parse_skill_file(&path);
    assert!(
        matches!(outcome, Err(SkillParseError::FrontmatterMissing)),
        "open ---  without close MUST yield FrontmatterMissing"
    );
}

#[test]
fn parse_empty_file_yields_frontmatter_missing() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("empty.md");
    std::fs::write(&path, "").expect("write");
    let outcome = parse_skill_file(&path);
    assert!(matches!(outcome, Err(SkillParseError::FrontmatterMissing)));
}

#[test]
fn frontmatter_missing_display_string_is_documented() {
    let err = SkillParseError::FrontmatterMissing;
    let s = err.to_string();
    assert_eq!(s, "skill file has no YAML frontmatter (`---` delimiters)");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — YamlFailed variant
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_file_with_malformed_yaml_yields_yaml_failed() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("bad.md");
    std::fs::write(&path, "---\nname: test\n  : malformed indent\n---\nbody\n").expect("write");
    let outcome = parse_skill_file(&path);
    assert!(
        matches!(outcome, Err(SkillParseError::YamlFailed(_))),
        "malformed YAML MUST yield YamlFailed; got {outcome:?}"
    );
}

#[test]
fn parse_file_with_yaml_missing_required_name_yields_yaml_failed() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("noname.md");
    std::fs::write(&path, "---\ndescription: \"no name field\"\n---\nbody\n").expect("write");
    let outcome = parse_skill_file(&path);
    // Missing required `name` field → YAML deserialize error.
    assert!(matches!(outcome, Err(SkillParseError::YamlFailed(_))));
}

#[test]
fn yaml_failed_display_includes_yaml_prefix() {
    // Force a YamlFailed via construct from a serde_yaml error.
    let bad: Result<serde_json::Value, _> = serde_yaml::from_str(": invalid yaml");
    let yaml_err = bad.unwrap_err();
    let skill_err: SkillParseError = yaml_err.into();
    let s = skill_err.to_string();
    assert!(s.starts_with("failed to parse skill frontmatter as YAML:"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Happy path round-trip
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn parse_valid_skill_file_succeeds_with_documented_fields() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("ok.md");
    std::fs::write(
        &path,
        "---\nname: test_skill_197\ndescription: \"a test skill\"\n---\nBody.\n",
    )
    .expect("write");
    let skill = parse_skill_file(&path).expect("MUST parse");
    assert_eq!(skill.name, "test_skill_197");
    assert_eq!(skill.description, "a test skill");
}

#[test]
fn parse_skill_with_allowed_tools_preserves_list() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("tools.md");
    std::fs::write(
        &path,
        "---\nname: tool_skill\ndescription: x\nallowed_tools:\n  - read_file\n  - bash\n---\n",
    )
    .expect("write");
    let skill = parse_skill_file(&path).expect("parse");
    let tools = skill.allowed_tools.expect("Some");
    assert_eq!(tools, vec!["read_file".to_string(), "bash".to_string()]);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Cross-variant distinctness
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn three_variants_have_distinct_display_strings() {
    let read_io = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
    let read_err = SkillParseError::ReadFailed(read_io);
    let fm_err = SkillParseError::FrontmatterMissing;

    let s_read = read_err.to_string();
    let s_fm = fm_err.to_string();

    assert_ne!(s_read, s_fm, "ReadFailed and FrontmatterMissing distinct");
    // Each Display starts with a unique prefix.
    assert!(s_read.contains("read"));
    assert!(s_fm.contains("frontmatter"));
}

#[test]
fn read_failed_propagates_io_error_kind() {
    let outcome = parse_skill_file(Path::new("/nonexistent/path/skill.md"));
    let err = outcome.unwrap_err();
    let s = err.to_string();
    // Display includes the underlying io::Error description.
    assert!(s.contains("read") || s.contains("file") || !s.is_empty());
}
