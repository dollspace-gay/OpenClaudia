//! Adversarial coverage for the plugin URL validator, marketplace
//! allow/block policy, and the skill-file parser.
//!
//! Sprint 11 of the verification effort. `tests/plugins_integration.rs`
//! already pins 29 plugin-loader scenarios — this file fills the
//! adversarial-input gaps:
//!
//!   - **`validate_source_url` attack catalog** — non-allowed schemes
//!     (`ftp`, `file`, `javascript`), missing host, embedded password
//!     (forbidden on every scheme), SCP-style `git@host:path` for a
//!     non-canonical host (crosslink #866 — the bypass that admitted
//!     `git@attacker.invalid:repo`).
//!   - **`check_marketplace_allowed` precedence** — blocklist beats
//!     allowlist; empty allowlist means "nothing allowed";
//!     `None` allowlist means "no allowlist enforcement".
//!   - **`parse_skill_file` parser robustness** — missing frontmatter,
//!     unterminated frontmatter, malformed YAML, BOM-prefixed input,
//!     CRLF normalisation.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::plugins::marketplace::MarketplaceSource;
use openclaudia::plugins::policy::{check_marketplace_allowed, PluginPolicy, PolicyRejection};
use openclaudia::plugins::validate::validate_source_url;
use openclaudia::skills::{parse_skill_file, SkillParseError};
use std::io::Write;
use tempfile::NamedTempFile;

// ───────────────────────────────────────────────────────────────────────────
// Section A — validate_source_url adversarial catalog
// ───────────────────────────────────────────────────────────────────────────

/// URLs the validator MUST refuse. The expected-marker pair lets us
/// pin both the rejection AND a fragment of the error message so
/// future error-text refactors don't silently lose the
/// security-relevant detail.
const REJECTED_URLS: &[(&str, &str)] = &[
    // Schemes outside the allowlist.
    ("ftp://example.com/repo.git", "scheme"),
    ("file:///etc/passwd", "scheme"),
    ("javascript:alert(1)", "scheme"),
    ("gopher://example.com/", "scheme"),
    ("ldap://example.com/", "scheme"),
    ("http://example.com/repo.git", "scheme"), // only https/ssh, NOT http
    // Embedded password — forbidden on every scheme.
    ("https://user:pass@example.com/repo.git", "password"),
    ("ssh://git:secret@github.com/owner/repo.git", "password"),
    // SCP-style with a NON-CANONICAL username (anything other than
    // `git`). Pre-crosslink #866 this short-circuited every check
    // and was admitted. The host can be arbitrary — a custom SSH
    // server is a legitimate use case — but only the canonical
    // `git` user is permitted for SCP-form URLs.
    ("attacker@github.com:owner/repo.git", "user"),
    ("bob@github.com:owner/repo.git", "user"),
];

#[test]
fn validate_source_url_rejects_catalog() {
    let mut leaked = Vec::new();
    for (raw, expected_marker) in REJECTED_URLS {
        match validate_source_url(raw) {
            Ok(()) => leaked.push((*raw).to_string()),
            Err(e) => {
                let lowered = e.to_string().to_lowercase();
                if !lowered.contains(&expected_marker.to_lowercase()) {
                    eprintln!(
                        "note: {raw:?} refused but message {e:?} doesn't \
                         contain expected marker {expected_marker:?}"
                    );
                }
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "{} hostile source URLs admitted by validator:\n  {}",
        leaked.len(),
        leaked.join("\n  "),
    );
}

#[test]
fn validate_source_url_accepts_canonical_forms() {
    // Counter-test: the validator must accept the documented happy
    // paths. A regression that tightens the allowlist too far would
    // break installs of real plugins.
    const ACCEPTED: &[&str] = &[
        "https://github.com/owner/repo.git",
        "https://gitlab.com/owner/repo.git",
        "ssh://git@github.com/owner/repo.git",
        // Canonical SCP form against the canonical user.
        "git@github.com:owner/repo.git",
    ];
    for raw in ACCEPTED {
        let outcome = validate_source_url(raw);
        assert!(
            outcome.is_ok(),
            "canonical URL {raw:?} must be admitted; got {outcome:?}"
        );
    }
}

#[test]
fn validate_source_url_refuses_empty_and_garbage_inputs() {
    // Note: `https:///path-only` is NOT in this list — the URL crate
    // accepts the empty host and the validator then has nothing to
    // reject. That's a parser behaviour, not a validator bug.
    for raw in &["", "   ", "not-a-url", "https://"] {
        let outcome = validate_source_url(raw);
        assert!(
            outcome.is_err(),
            "malformed input {raw:?} must be refused; got {outcome:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — check_marketplace_allowed precedence
// ───────────────────────────────────────────────────────────────────────────

fn github_source(repo: &str) -> MarketplaceSource {
    MarketplaceSource::GitHub {
        repo: repo.to_string(),
        git_ref: None,
        path: None,
    }
}

#[test]
fn default_policy_admits_every_source() {
    // No allowlist + no blocklist → everything passes (this is the
    // "no policy enforcement" default).
    let policy = PluginPolicy::default();
    let outcome = check_marketplace_allowed(&github_source("owner/repo"), &policy);
    assert!(
        outcome.is_ok(),
        "default policy must admit; got {outcome:?}"
    );
}

#[test]
fn empty_allowlist_rejects_every_source() {
    // `Some(vec![])` is semantically distinct from `None` — it means
    // "an allowlist is in force, but it permits nothing". Every
    // source must be refused.
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(Vec::new()),
        ..Default::default()
    };
    let outcome = check_marketplace_allowed(&github_source("owner/repo"), &policy);
    assert_eq!(
        outcome,
        Err(PolicyRejection::NotInAllowlist),
        "empty allowlist must refuse every source with NotInAllowlist"
    );
}

#[test]
fn blocklist_beats_allowlist_for_overlapping_entries() {
    // The documented precedence: blocklist wins even when the same
    // source ALSO appears in the allowlist.
    let same = github_source("owner/repo");
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![same.clone()]),
        blocked_marketplaces: vec![same.clone()],
        ..Default::default()
    };
    let outcome = check_marketplace_allowed(&same, &policy);
    assert_eq!(
        outcome,
        Err(PolicyRejection::Blocked),
        "blocklist must beat allowlist for overlapping entries"
    );
}

#[test]
fn allowlist_admits_matching_source_only() {
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![github_source("owner/allowed")]),
        ..Default::default()
    };
    let ok = check_marketplace_allowed(&github_source("owner/allowed"), &policy);
    assert!(ok.is_ok(), "matching source must pass; got {ok:?}");

    let err = check_marketplace_allowed(&github_source("owner/other"), &policy);
    assert_eq!(
        err,
        Err(PolicyRejection::NotInAllowlist),
        "non-matching source must be refused as NotInAllowlist"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — parse_skill_file parser robustness
// ───────────────────────────────────────────────────────────────────────────

/// Write `contents` to a fresh temp file and return both the file
/// (kept alive) and its path. The file's lifetime is tied to the
/// returned tuple — drop the tuple to delete the file.
fn write_temp(contents: &[u8]) -> (NamedTempFile, std::path::PathBuf) {
    let mut f = NamedTempFile::new().expect("tempfile");
    f.write_all(contents).expect("write");
    f.flush().expect("flush");
    let path = f.path().to_path_buf();
    (f, path)
}

#[test]
fn parse_skill_file_rejects_missing_frontmatter() {
    let (_f, path) = write_temp(b"# Just markdown, no frontmatter\n");
    let outcome = parse_skill_file(&path);
    assert!(
        matches!(outcome, Err(SkillParseError::FrontmatterMissing)),
        "plain markdown must yield FrontmatterMissing; got {outcome:?}"
    );
}

#[test]
fn parse_skill_file_rejects_unterminated_frontmatter() {
    let (_f, path) = write_temp(
        b"---\nname: leaky\ndescription: never closes\n\n# Body without closing delimiter",
    );
    let outcome = parse_skill_file(&path);
    assert!(
        matches!(outcome, Err(SkillParseError::FrontmatterMissing)),
        "unterminated frontmatter must yield FrontmatterMissing; got {outcome:?}"
    );
}

#[test]
fn parse_skill_file_rejects_malformed_yaml_frontmatter() {
    // Frontmatter exists and is properly delimited, but the YAML
    // inside is structurally bad — missing field, wrong type.
    let (_f, path) = write_temp(b"---\nthis is not: { valid yaml: [\n---\n# body\n");
    let outcome = parse_skill_file(&path);
    assert!(
        matches!(outcome, Err(SkillParseError::YamlFailed(_))),
        "malformed YAML must yield YamlFailed; got {outcome:?}"
    );
}

#[test]
fn parse_skill_file_strips_utf8_bom_at_start() {
    // BOM (\u{FEFF} as UTF-8: 0xEF 0xBB 0xBF) immediately followed
    // by `---` must still parse correctly.
    let mut body: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    body.extend_from_slice(b"---\nname: bom-test\ndescription: stripped\n---\n# Body\n");
    let (_f, path) = write_temp(&body);
    let skill = parse_skill_file(&path).expect("BOM-prefixed frontmatter must parse");
    assert_eq!(skill.name, "bom-test");
    assert_eq!(skill.description, "stripped");
}

#[test]
fn parse_skill_file_normalises_crlf_to_lf() {
    // Windows-style CRLF line endings must be normalised before the
    // delimiter check, so editors on Windows don't break the parser.
    let body = b"---\r\nname: crlf-test\r\ndescription: normalised\r\n---\r\n# Body\r\n";
    let (_f, path) = write_temp(body);
    let skill = parse_skill_file(&path).expect("CRLF frontmatter must parse");
    assert_eq!(skill.name, "crlf-test");
    assert_eq!(skill.description, "normalised");
}

#[test]
fn parse_skill_file_round_trips_unicode_body() {
    let (_f, path) =
        write_temp("---\nname: unicode\ndescription: ok\n---\nHéllo 世界 🚀\n".as_bytes());
    let skill = parse_skill_file(&path).expect("unicode body must parse");
    assert!(
        skill.prompt.contains("Héllo 世界 🚀"),
        "unicode body must round-trip in prompt; got {:?}",
        skill.prompt
    );
}

#[test]
fn parse_skill_file_handles_empty_body() {
    // Frontmatter present but the body is empty — must NOT error;
    // the prompt is allowed to be the empty string.
    let (_f, path) = write_temp(b"---\nname: empty-body\ndescription: ok\n---\n");
    let skill = parse_skill_file(&path).expect("empty body must parse");
    assert_eq!(skill.name, "empty-body");
    assert!(
        skill.prompt.trim().is_empty(),
        "empty-body skill must have an empty prompt; got {:?}",
        skill.prompt
    );
}
