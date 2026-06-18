//! Final-answer validation for grounded agent turns.

use crate::evidence::{authoritative_evidence, Denial};
use crate::ledger::{Authority, ObsId, ObservationKind, RealityLedger};
use std::collections::HashSet;

const UUID_LEN: usize = 36;
const KNOWN_FILE_NAMES: &[&str] = &[
    "cargo.toml",
    "cargo.lock",
    "readme.md",
    "license",
    "makefile",
    "dockerfile",
];
const FILE_EXTENSIONS: &[&str] = &[
    ".rs", ".toml", ".lock", ".md", ".json", ".yaml", ".yml", ".ts", ".tsx", ".js", ".jsx", ".mjs",
    ".cjs", ".py", ".go", ".java", ".kt", ".swift", ".zig", ".c", ".h", ".cpp", ".hpp", ".sh",
    ".sql", ".html", ".css", ".scss", ".xml",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalGateReport {
    pub evidence: Vec<ObsId>,
    pub verification: Vec<ObsId>,
}

/// Validate that a final answer cites reality and verification.
///
/// A final can report failed verification, but it cannot omit verification.
/// If it mentions tests, it must also cite a concrete command observation.
///
/// # Errors
///
/// Returns [`Denial`] when the final answer is empty, lacks authoritative
/// evidence, lacks verifier observations, cites the wrong observation type, or
/// makes test/file/verification claims not backed by cited observations.
pub fn validate_final_answer(
    summary: &str,
    evidence: &[ObsId],
    verification: &[ObsId],
    ledger: &RealityLedger,
) -> Result<FinalGateReport, Denial> {
    if summary.trim().is_empty() {
        return Err(Denial::new("final answer requires a non-empty summary"));
    }

    let hydrated_evidence =
        authoritative_evidence(evidence, ledger, "final answer requires evidence")?;
    if !hydrated_evidence
        .iter()
        .any(|obs| is_final_evidence_observation(obs))
    {
        return Err(Denial::new(
            "final answer requires non-verification evidence",
        ));
    }

    if verification.is_empty() {
        return Err(Denial::new(
            "final answer requires verification observation",
        ));
    }

    let hydrated_verification = authoritative_evidence(
        verification,
        ledger,
        "final answer requires verification observation",
    )?;
    if !hydrated_verification
        .iter()
        .all(|obs| matches!(obs.kind, ObservationKind::Verification { .. }))
    {
        return Err(Denial::new(
            "final verification ids must reference verification observations",
        ));
    }
    if !hydrated_verification
        .iter()
        .all(|obs| obs.authority == Authority::Verifier)
    {
        return Err(Denial::new(
            "final verification ids must reference verifier observations",
        ));
    }
    if summary_claims_verification_success(summary)
        && !hydrated_verification
            .iter()
            .any(|obs| matches!(obs.kind, ObservationKind::Verification { passed: true, .. }))
    {
        return Err(Denial::new(
            "final successful verification claims require a passing verifier observation",
        ));
    }

    if summary_mentions_tests(summary)
        && !hydrated_evidence
            .iter()
            .any(|obs| matches!(obs.kind, ObservationKind::CommandRun { .. }))
    {
        return Err(Denial::new(
            "final test claims require a command observation",
        ));
    }
    if summary_claims_test_success(summary)
        && !hydrated_evidence.iter().any(|obs| {
            matches!(obs.kind, ObservationKind::CommandRun { .. })
                && command_observation_is_passing_test_command(obs)
        })
    {
        return Err(Denial::new(
            "final test success claims require a successful test command observation",
        ));
    }
    if summary_claims_verification_success(summary)
        && !hydrated_evidence
            .iter()
            .any(|obs| command_observation_exit_code(obs) == Some(0))
    {
        return Err(Denial::new(
            "final successful verification claims require a successful command observation",
        ));
    }

    let file_claims = extract_file_claims(summary);
    for claim in file_claims {
        let backed_by_file_observation = hydrated_evidence.iter().any(|obs| {
            obs.kind
                .touched_files()
                .iter()
                .any(|observed| observed_path_matches_claim(observed, &claim))
        });
        if !backed_by_file_observation {
            return Err(Denial::new(format!(
                "final file claim requires fresh file or diff observation: {claim}"
            )));
        }
    }

    Ok(FinalGateReport {
        evidence: evidence.to_vec(),
        verification: verification.to_vec(),
    })
}

/// Validate a final answer by extracting cited observation ids from text.
///
/// # Errors
///
/// Returns [`Denial`] when extracted citations do not satisfy
/// [`validate_final_answer`].
pub fn validate_cited_final_answer(
    summary: &str,
    ledger: &RealityLedger,
) -> Result<FinalGateReport, Denial> {
    let evidence = extract_cited_obs_ids(summary);
    let verification = evidence
        .iter()
        .copied()
        .filter(|id| {
            ledger.get(*id).is_some_and(|obs| {
                matches!(obs.kind, ObservationKind::Verification { .. })
                    && obs.authority == Authority::Verifier
            })
        })
        .collect::<Vec<_>>();
    validate_final_answer(summary, &evidence, &verification, ledger)
}

#[must_use]
pub fn extract_cited_obs_ids(text: &str) -> Vec<ObsId> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for (start, _) in text.char_indices() {
        let end = start + UUID_LEN;
        if end > text.len() || !text.is_char_boundary(end) {
            continue;
        }
        let Ok(id) = text[start..end].parse::<ObsId>() else {
            continue;
        };
        if seen.insert(id) {
            ids.push(id);
        }
    }
    ids
}

fn summary_mentions_tests(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    lower.contains("cargo check")
        || lower.contains("cargo test")
        || lower.contains("cargo nextest")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| matches!(token, "test" | "tests" | "tested" | "testing"))
}

fn summary_claims_verification_success(summary: &str) -> bool {
    let tokens = summary
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();

    tokens.iter().enumerate().any(|(idx, token)| {
        matches!(
            token.as_str(),
            "pass"
                | "passed"
                | "passes"
                | "passing"
                | "succeed"
                | "succeeded"
                | "succeeds"
                | "successful"
                | "successfully"
                | "clean"
                | "green"
                | "ok"
                | "okay"
                | "verified"
        ) && !has_recent_negation(&tokens, idx)
    })
}

fn summary_claims_test_success(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    lower.contains("test") && summary_claims_verification_success(summary)
}

const fn command_observation_exit_code(observation: &crate::ledger::Observation) -> Option<i32> {
    let ObservationKind::CommandRun { exit_code, .. } = &observation.kind else {
        return None;
    };
    Some(*exit_code)
}

const fn is_final_evidence_observation(observation: &crate::ledger::Observation) -> bool {
    matches!(
        &observation.kind,
        ObservationKind::UserTask { .. }
            | ObservationKind::FileRead { .. }
            | ObservationKind::CommandRun { .. }
            | ObservationKind::DiffObserved { .. }
            | ObservationKind::ToolResult { .. }
    )
}

fn command_observation_is_passing_test_command(observation: &crate::ledger::Observation) -> bool {
    let ObservationKind::CommandRun {
        argv, exit_code, ..
    } = &observation.kind
    else {
        return false;
    };
    *exit_code == 0 && is_test_command_text(&argv.join(" ").to_ascii_lowercase())
}

fn is_test_command_text(command: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "cargo test",
        "cargo nextest",
        "npm test",
        "npm run test",
        "pnpm test",
        "pnpm run test",
        "yarn test",
        "yarn run test",
        "bun test",
        "pytest",
        "python -m pytest",
        "go test",
        "zig test",
        "swift test",
        "mvn test",
        "gradle test",
        "make test",
        "ctest",
    ];
    NEEDLES.iter().any(|needle| command.contains(needle))
}

fn has_recent_negation(tokens: &[String], idx: usize) -> bool {
    let start = idx.saturating_sub(3);
    tokens[start..idx].iter().any(|token| {
        matches!(
            token.as_str(),
            "not"
                | "no"
                | "never"
                | "without"
                | "fail"
                | "failed"
                | "fails"
                | "failing"
                | "failure"
        )
    })
}

fn extract_file_claims(summary: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut claims = Vec::new();

    for raw in summary.split_whitespace() {
        let Some(candidate) = normalize_claim_token(raw) else {
            continue;
        };
        if !looks_like_file_path(&candidate) {
            continue;
        }
        if seen.insert(candidate.clone()) {
            claims.push(candidate);
        }
    }

    claims
}

fn normalize_claim_token(raw: &str) -> Option<String> {
    let mut token = raw.trim_matches(|c: char| {
        c.is_ascii_whitespace()
            || matches!(
                c,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
            )
    });
    if let Some((label, _target)) = token.split_once("](") {
        token = label.trim_matches(|c: char| {
            c.is_ascii_whitespace()
                || matches!(
                    c,
                    '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
                )
        });
    }
    token = token.trim_end_matches('.');
    if token.is_empty() || token.contains("://") {
        return None;
    }

    while let Some((prefix, suffix)) = token.rsplit_once(':') {
        if !suffix.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        token = prefix;
    }

    token = token.trim_end_matches(':');
    let token = token.trim_start_matches("./");
    (!token.is_empty()).then(|| token.to_string())
}

fn looks_like_file_path(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if KNOWN_FILE_NAMES.contains(&lower.as_str()) {
        return true;
    }

    if FILE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        return true;
    }

    lower.contains('/')
        && lower
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains('.') || KNOWN_FILE_NAMES.contains(&last))
}

fn observed_path_matches_claim(observed: &str, claim: &str) -> bool {
    let observed = observed.trim_start_matches("./");
    let claim = claim.trim_start_matches("./");
    observed == claim
        || observed.ends_with(&format!("/{claim}"))
        || claim.ends_with(&format!("/{observed}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Authority, ObservationKind, RealityLedger};

    #[test]
    fn extract_cited_obs_ids_finds_unique_uuid_tokens() {
        let first = ObsId::new();
        let second = ObsId::new();
        let text = format!("Used [{first}] and obs-{second}; repeated {first}.");

        assert_eq!(extract_cited_obs_ids(&text), vec![first, second]);
    }

    #[test]
    fn cited_final_requires_cited_verification_observation() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run("/tmp", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!("Verified with cargo check using [{command}] and [{verification}].");

        let report = validate_cited_final_answer(&summary, &ledger).expect("valid final");
        assert_eq!(report.evidence, vec![command, verification]);
        assert_eq!(report.verification, vec![verification]);
    }

    #[test]
    fn cited_final_rejects_verifier_only_evidence() {
        let mut ledger = RealityLedger::new();
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: false,
                    command: Some("cargo check".to_string()),
                    findings: vec!["cargo check failed".to_string()],
                },
            )
            .expect("verification");
        let summary = format!("No changes were completed [{verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final answer requires non-verification evidence"
        );
    }

    #[test]
    fn cited_final_rejects_verification_kind_without_verifier_authority() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run("/tmp", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let forged_verification = ledger
            .append(
                Authority::Tool,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("forged verification");
        let summary =
            format!("Verified with cargo check using [{command}] and [{forged_verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final answer requires verification observation"
        );
    }

    #[test]
    fn final_rejects_explicit_verification_without_verifier_authority() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run("/tmp", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let forged_verification = ledger
            .append(
                Authority::Tool,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("forged verification");
        let summary = "Verified with cargo check.";

        let denial = validate_final_answer(
            summary,
            &[command, forged_verification],
            &[forged_verification],
            &ledger,
        )
        .expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final verification ids must reference verifier observations"
        );
    }

    #[test]
    fn final_success_claim_rejects_failed_verifier_observation() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run(
                "/tmp",
                vec!["cargo".into(), "check".into()],
                1,
                "",
                "failed",
            )
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: false,
                    command: Some("cargo check".to_string()),
                    findings: vec!["cargo check failed".to_string()],
                },
            )
            .expect("verification");
        let summary = format!("cargo check passed cleanly [{command}] [{verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final successful verification claims require a passing verifier observation"
        );
    }

    #[test]
    fn final_success_claim_rejects_failed_command_observation() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run(
                "/tmp",
                vec!["cargo".into(), "check".into()],
                1,
                "",
                "failed",
            )
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!("cargo check passed cleanly [{command}] [{verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final successful verification claims require a successful command observation"
        );
    }

    #[test]
    fn final_failed_verification_summary_accepts_failed_verifier_observation() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run(
                "/tmp",
                vec!["cargo".into(), "check".into()],
                1,
                "",
                "failed",
            )
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: false,
                    command: Some("cargo check".to_string()),
                    findings: vec!["cargo check failed".to_string()],
                },
            )
            .expect("verification");
        let summary = format!("cargo check failed [{command}] [{verification}].");

        validate_cited_final_answer(&summary, &ledger)
            .expect("honest failed verification summary is allowed");
    }

    #[test]
    fn final_test_success_claim_requires_test_command_observation() {
        let mut ledger = RealityLedger::new();
        let check = ledger
            .observe_command_run("/tmp", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!("Tests passed cleanly [{check}] [{verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final test success claims require a successful test command observation"
        );
    }

    #[test]
    fn final_test_success_claim_rejects_failed_test_command_observation() {
        let mut ledger = RealityLedger::new();
        let test = ledger
            .observe_command_run("/tmp", vec!["cargo".into(), "test".into()], 1, "", "failed")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo test".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!("Tests passed cleanly [{test}] [{verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("denied");

        assert_eq!(
            denial.reason(),
            "final test success claims require a successful test command observation"
        );
    }

    #[test]
    fn final_test_success_claim_accepts_test_command_observation() {
        let mut ledger = RealityLedger::new();
        let test = ledger
            .observe_command_run(
                "/tmp",
                vec![
                    "cargo".into(),
                    "test".into(),
                    "--test".into(),
                    "ledger_decision_e2e".into(),
                ],
                0,
                "",
                "",
            )
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo test --test ledger_decision_e2e".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!("Tests passed cleanly [{test}] [{verification}].");

        validate_cited_final_answer(&summary, &ledger).expect("test command grounds test success");
    }

    #[test]
    fn cited_final_rejects_uncited_summary() {
        let ledger = RealityLedger::new();
        let denial =
            validate_cited_final_answer("Verified with cargo check.", &ledger).expect_err("denied");
        assert_eq!(denial.reason(), "final answer requires evidence");
    }

    #[test]
    fn final_file_claim_requires_file_or_diff_evidence() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run("/repo", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!(
            "Updated src/pipeline.rs and verified with cargo check [{command}] [{verification}]."
        );

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("file denied");

        assert_eq!(
            denial.reason(),
            "final file claim requires fresh file or diff observation: src/pipeline.rs"
        );
    }

    #[test]
    fn final_file_claim_accepts_fresh_diff_evidence() {
        let mut ledger = RealityLedger::new();
        let diff = ledger
            .observe_diff(
                vec!["src/pipeline.rs".to_string()],
                "diff --git a/src/pipeline.rs b/src/pipeline.rs",
            )
            .expect("diff");
        let command = ledger
            .observe_command_run("/repo", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!(
            "Updated src/pipeline.rs:12 and verified with cargo check [{diff}] [{command}] [{verification}]."
        );

        validate_cited_final_answer(&summary, &ledger).expect("fresh diff grounds file claim");
    }

    #[test]
    fn final_latest_file_claim_does_not_count_as_test_claim() {
        let mut ledger = RealityLedger::new();
        let diff = ledger
            .observe_diff(
                vec!["src/providers/mod.rs".to_string()],
                "diff --git a/src/providers/mod.rs b/src/providers/mod.rs",
            )
            .expect("diff");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: false,
                    command: None,
                    findings: vec!["tests were not run".to_string()],
                },
            )
            .expect("verification");
        let summary = format!(
            "Updated the latest model list in src/providers/mod.rs [{diff}] [{verification}]."
        );

        validate_cited_final_answer(&summary, &ledger)
            .expect("latest must not be parsed as a test claim");
    }

    #[test]
    fn final_file_claim_accepts_markdown_link_with_fresh_diff_evidence() {
        let mut ledger = RealityLedger::new();
        let diff = ledger
            .observe_diff(
                vec!["src/final_gate.rs".to_string()],
                "diff --git a/src/final_gate.rs b/src/final_gate.rs",
            )
            .expect("diff");
        let command = ledger
            .observe_command_run("/repo", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary = format!(
            "Updated [src/final_gate.rs](/repo/src/final_gate.rs:120) and verified with cargo check [{diff}] [{command}] [{verification}]."
        );

        validate_cited_final_answer(&summary, &ledger)
            .expect("fresh diff grounds markdown file claim");
    }

    #[test]
    fn final_known_file_claim_with_trailing_colon_requires_evidence() {
        let mut ledger = RealityLedger::new();
        let command = ledger
            .observe_command_run("/repo", vec!["cargo".into(), "check".into()], 0, "", "")
            .expect("command");
        let verification = ledger
            .append(
                Authority::Verifier,
                ObservationKind::Verification {
                    passed: true,
                    command: Some("cargo check".to_string()),
                    findings: Vec::new(),
                },
            )
            .expect("verification");
        let summary =
            format!("Updated README.md: verified with cargo check [{command}] [{verification}].");

        let denial = validate_cited_final_answer(&summary, &ledger).expect_err("file denied");

        assert_eq!(
            denial.reason(),
            "final file claim requires fresh file or diff observation: README.md"
        );
    }
}
