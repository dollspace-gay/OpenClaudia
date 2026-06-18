use openclaudia::decision::{validate_decision, AgentDecision};
use openclaudia::ledger::{Authority, ObservationKind, RealityLedger};
use openclaudia::task_spec::TaskSpec;

#[test]
fn summary_observation_cannot_authorize_edit() {
    let mut ledger = RealityLedger::new();
    let summary = ledger
        .append(
            Authority::ModelSummary,
            ObservationKind::Summary {
                text: "src/lib.rs contains the needed function".to_string(),
                source_obs: Vec::new(),
            },
        )
        .expect("append summary");

    let decision = AgentDecision::Edit {
        reason: "change remembered code".to_string(),
        evidence: vec![summary],
        patch: "*** Begin Patch\n*** End Patch".to_string(),
    };

    let denial = validate_decision(&decision, &ledger).expect_err("summary must not ground edit");
    assert_eq!(denial.reason(), "summary is not authoritative evidence");
}

#[test]
fn edit_requires_non_stale_file_read() {
    let mut ledger = RealityLedger::new();
    let read = ledger
        .observe_file_read(
            "src/lib.rs",
            "pub fn old() {}\n",
            1,
            1,
            "1| pub fn old() {}",
        )
        .expect("read observation");

    let decision = AgentDecision::Edit {
        reason: "replace old function".to_string(),
        evidence: vec![read],
        patch: "*** Begin Patch\n*** Update File: src/lib.rs\n*** End Patch".to_string(),
    };
    validate_decision(&decision, &ledger).expect("fresh file read grounds edit");

    ledger
        .mark_file_observations_stale("src/lib.rs")
        .expect("mark stale");
    let denial = validate_decision(&decision, &ledger).expect_err("stale read denied");
    assert!(
        denial.reason().contains("stale observation"),
        "unexpected denial: {}",
        denial.reason()
    );
}

#[test]
fn diff_marks_previous_file_reads_stale() {
    let mut ledger = RealityLedger::new();
    let read = ledger
        .observe_file_read("src/providers/mod.rs", "old\n", 1, 1, "1| old")
        .expect("read observation");
    assert!(!ledger.is_stale(read));

    let diff = ledger
        .observe_diff(
            vec!["src/providers/mod.rs".to_string()],
            "diff --git a/src/providers/mod.rs b/src/providers/mod.rs\n",
        )
        .expect("diff observation");

    assert!(ledger.is_stale(read));
    assert!(!ledger.is_stale(diff));
}

#[test]
fn final_requires_verification_observation() {
    let mut ledger = RealityLedger::new();
    let task = ledger
        .observe_user_task("Make the binary commands functional.")
        .expect("task");
    let command = ledger
        .observe_command_run(
            "/repo",
            vec!["cargo".to_string(), "test".to_string()],
            0,
            "ok",
            "",
        )
        .expect("command");

    let without_verification = AgentDecision::Final {
        summary: "Ran tests and fixed the issue.".to_string(),
        evidence: vec![task, command],
        verification: Vec::new(),
    };
    let denial =
        validate_decision(&without_verification, &ledger).expect_err("verification required");
    assert_eq!(
        denial.reason(),
        "final answer requires verification observation"
    );

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
    let final_decision = AgentDecision::Final {
        summary: "Ran tests and fixed the issue.".to_string(),
        evidence: vec![task, command],
        verification: vec![verification],
    };
    validate_decision(&final_decision, &ledger).expect("verified final accepted");
}

#[test]
fn final_test_claim_requires_command_observation() {
    let mut ledger = RealityLedger::new();
    let task = ledger
        .observe_user_task("Audit the codebase.")
        .expect("task");
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

    let decision = AgentDecision::Final {
        summary: "Tests passed.".to_string(),
        evidence: vec![task],
        verification: vec![verification],
    };

    let denial = validate_decision(&decision, &ledger).expect_err("command observation required");
    assert_eq!(
        denial.reason(),
        "final test claims require a command observation"
    );
}

#[test]
fn run_command_requires_authoritative_evidence() {
    let mut ledger = RealityLedger::new();
    let summary = ledger
        .append(
            Authority::ModelSummary,
            ObservationKind::Summary {
                text: "Cargo tests are the next step".to_string(),
                source_obs: Vec::new(),
            },
        )
        .expect("summary");

    let denied = AgentDecision::RunCommand {
        reason: "verify summary".to_string(),
        evidence: vec![summary],
        argv: vec!["cargo".to_string(), "test".to_string()],
    };
    let denial = validate_decision(&denied, &ledger).expect_err("summary denied");
    assert_eq!(denial.reason(), "summary is not authoritative evidence");

    let task = ledger.observe_user_task("Run the tests.").expect("task");
    let allowed = AgentDecision::RunCommand {
        reason: "verify requested behavior".to_string(),
        evidence: vec![task],
        argv: vec!["cargo".to_string(), "test".to_string()],
    };
    validate_decision(&allowed, &ledger).expect("user task grounds command");
}

#[test]
fn sqlite_ledger_round_trips_observations_and_stale_state() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("reality-ledger.sqlite3");

    let read = {
        let mut ledger = RealityLedger::open(&path).expect("open ledger");
        let read = ledger
            .observe_file_read("src/main.rs", "fn main() {}\n", 1, 1, "1| fn main() {}")
            .expect("file read");
        ledger
            .observe_diff(
                vec!["src/main.rs".to_string()],
                "diff --git a/src/main.rs b/src/main.rs\n",
            )
            .expect("diff");
        assert!(ledger.is_stale(read));
        read
    };

    let ledger = RealityLedger::open(&path).expect("reopen ledger");
    assert_eq!(ledger.len(), 2);
    assert!(ledger.get(read).is_some());
    assert!(ledger.is_stale(read));
    let index = ledger.observation_index(16);
    assert_eq!(index.len(), 2);
    assert!(index.iter().any(|entry| entry.stale));
}

#[test]
fn task_spec_must_come_from_user_task_observation() {
    let mut ledger = RealityLedger::new();
    let task = ledger.observe_user_task("Do the audit.").expect("task");
    let spec = TaskSpec::from_user_observation(&ledger, task).expect("task spec");
    assert_eq!(spec.content, "Do the audit.");

    let command = ledger
        .observe_command_run(
            "/repo",
            vec!["git".to_string(), "status".to_string()],
            0,
            "",
            "",
        )
        .expect("command");
    let denial = TaskSpec::from_user_observation(&ledger, command).expect_err("not user task");
    assert_eq!(denial.reason(), "task spec must come from user authority");
}
