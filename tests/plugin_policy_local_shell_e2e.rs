//! End-to-end tests for `PluginPolicy` marketplace decision matrix
//! + `LocalShellTask` state machine.
//!
//! Sprint 52 of the verification effort.
//!
//! Sprint 11 (`plugin_skill_security_e2e.rs`) covered URL
//! validation + signature gates; this file covers the
//! orthogonal marketplace-allowlist + per-plugin skip-list
//! decisions plus the coordinator-side local-shell task state
//! transitions.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::coordinator::tasks::{tasks_for_agent, LocalShellTask, LocalShellTaskState};
use openclaudia::coordinator::teammate::TeammateId;
use openclaudia::plugins::marketplace::MarketplaceSource;
use openclaudia::plugins::policy::{
    check_marketplace_allowed, is_marketplace_skipped, is_plugin_skipped, PluginPolicy,
    PolicyRejection,
};

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn github(repo: &str) -> MarketplaceSource {
    MarketplaceSource::GitHub {
        repo: repo.to_string(),
        git_ref: None,
        path: None,
    }
}

fn github_with_ref(repo: &str, git_ref: &str) -> MarketplaceSource {
    MarketplaceSource::GitHub {
        repo: repo.to_string(),
        git_ref: Some(git_ref.to_string()),
        path: None,
    }
}

fn git_url(url: &str) -> MarketplaceSource {
    MarketplaceSource::Git {
        url: url.to_string(),
        git_ref: None,
        path: None,
    }
}

fn url_source(url: &str) -> MarketplaceSource {
    MarketplaceSource::Url {
        url: url.to_string(),
        headers: None,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — empty / default policy admits everything
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn default_policy_admits_every_source() {
    let policy = PluginPolicy::default();
    for src in &[
        github("user/repo"),
        git_url("https://example.com/repo.git"),
        url_source("https://example.com/marketplace.json"),
        MarketplaceSource::File {
            path: "/tmp/x.json".to_string(),
        },
        MarketplaceSource::Directory {
            path: "/tmp/dir".to_string(),
        },
    ] {
        assert!(
            check_marketplace_allowed(src, &policy).is_ok(),
            "default policy MUST admit {src:?}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Blocklist takes precedence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn blocked_marketplace_is_rejected_with_blocked_variant() {
    let policy = PluginPolicy {
        blocked_marketplaces: vec![github("evil/repo")],
        ..PluginPolicy::default()
    };
    let outcome = check_marketplace_allowed(&github("evil/repo"), &policy);
    assert_eq!(outcome, Err(PolicyRejection::Blocked));
}

#[test]
fn blocked_takes_precedence_over_allowlist() {
    let policy = PluginPolicy {
        blocked_marketplaces: vec![github("evil/repo")],
        strict_known_marketplaces: Some(vec![github("evil/repo")]), // Also on allowlist!
        ..PluginPolicy::default()
    };
    // Even though the source is on BOTH lists, Blocked wins.
    let outcome = check_marketplace_allowed(&github("evil/repo"), &policy);
    assert_eq!(
        outcome,
        Err(PolicyRejection::Blocked),
        "blocklist MUST take precedence over allowlist"
    );
}

#[test]
fn blocklist_match_is_case_insensitive_for_github_repo() {
    let policy = PluginPolicy {
        blocked_marketplaces: vec![github("Evil/Repo")],
        ..PluginPolicy::default()
    };
    // Different case — must still match.
    let outcome = check_marketplace_allowed(&github("evil/repo"), &policy);
    assert_eq!(
        outcome,
        Err(PolicyRejection::Blocked),
        "GitHub repo match MUST be case-insensitive (per docstring)"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Allowlist enforcement
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn empty_allowlist_rejects_everything() {
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![]),
        ..PluginPolicy::default()
    };
    let outcome = check_marketplace_allowed(&github("user/repo"), &policy);
    assert_eq!(
        outcome,
        Err(PolicyRejection::NotInAllowlist),
        "empty allowlist MUST reject everything"
    );
}

#[test]
fn allowlist_admits_listed_source_only() {
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![github("alice/plugin")]),
        ..PluginPolicy::default()
    };
    // Listed source: admit.
    assert!(check_marketplace_allowed(&github("alice/plugin"), &policy).is_ok());
    // Unlisted source: NotInAllowlist.
    let outcome = check_marketplace_allowed(&github("bob/plugin"), &policy);
    assert_eq!(outcome, Err(PolicyRejection::NotInAllowlist));
}

#[test]
fn none_allowlist_disables_allowlist_check() {
    // `strict_known_marketplaces: None` means "no allowlist
    // enforcement" — distinct from empty.
    let policy = PluginPolicy {
        strict_known_marketplaces: None,
        ..PluginPolicy::default()
    };
    assert!(check_marketplace_allowed(&github("any/repo"), &policy).is_ok());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — wild_match_opt asymmetric semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn allowlist_with_no_ref_on_rule_admits_any_candidate_ref() {
    // Rule: github user/repo (no ref). Candidate: github
    // user/repo @ main. The wild-match semantic says None on
    // the rule side wildcards out any candidate.
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![github("user/repo")]),
        ..PluginPolicy::default()
    };
    let candidate = github_with_ref("user/repo", "main");
    assert!(
        check_marketplace_allowed(&candidate, &policy).is_ok(),
        "rule-side None ref MUST wildcard any candidate ref"
    );
}

#[test]
fn allowlist_with_specific_ref_on_rule_requires_candidate_match() {
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![github_with_ref("user/repo", "v1.0")]),
        ..PluginPolicy::default()
    };
    // Candidate with matching ref: admit.
    assert!(check_marketplace_allowed(&github_with_ref("user/repo", "v1.0"), &policy).is_ok());
    // Candidate with non-matching ref: reject.
    let outcome = check_marketplace_allowed(&github_with_ref("user/repo", "v2.0"), &policy);
    assert_eq!(outcome, Err(PolicyRejection::NotInAllowlist));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Git URL canonicalization
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn git_url_match_canonicalizes_trailing_dot_git() {
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![git_url("https://example.com/repo.git")]),
        ..PluginPolicy::default()
    };
    // Candidate without .git suffix — must still match.
    let candidate = git_url("https://example.com/repo");
    assert!(
        check_marketplace_allowed(&candidate, &policy).is_ok(),
        "git URL match MUST canonicalize trailing .git"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Cross-variant non-matching
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn different_source_variants_never_match_each_other() {
    let policy = PluginPolicy {
        blocked_marketplaces: vec![github("user/repo")],
        ..PluginPolicy::default()
    };
    // Git URL pointing at the same repo MUST NOT match a
    // GitHub blocklist entry — the variants are distinct
    // identities by policy.
    let candidate = git_url("https://github.com/user/repo.git");
    assert!(
        check_marketplace_allowed(&candidate, &policy).is_ok(),
        "Git URL MUST NOT match GitHub blocklist entry (cross-variant)"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — is_plugin_skipped / is_marketplace_skipped
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn is_plugin_skipped_returns_true_only_for_listed_ids() {
    let policy = PluginPolicy {
        skipped_plugins: vec!["evil@market".to_string(), "annoying@market".to_string()],
        ..PluginPolicy::default()
    };
    assert!(is_plugin_skipped("evil@market", &policy));
    assert!(is_plugin_skipped("annoying@market", &policy));
    assert!(!is_plugin_skipped("good@market", &policy));
    assert!(!is_plugin_skipped("", &policy));
}

#[test]
fn is_marketplace_skipped_returns_true_only_for_listed_names() {
    let policy = PluginPolicy {
        skipped_marketplaces: vec!["sketchy-marketplace".to_string()],
        ..PluginPolicy::default()
    };
    assert!(is_marketplace_skipped("sketchy-marketplace", &policy));
    assert!(!is_marketplace_skipped("trusted-marketplace", &policy));
}

#[test]
fn skipped_lookups_on_empty_policy_always_return_false() {
    let policy = PluginPolicy::default();
    assert!(!is_plugin_skipped("anything", &policy));
    assert!(!is_marketplace_skipped("anything", &policy));
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — LocalShellTask state machine
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn new_local_shell_task_starts_in_running_state() {
    let task = LocalShellTask::new("shell-1", "echo hi", None);
    assert!(matches!(task.state(), LocalShellTaskState::Running));
    assert_eq!(task.shell_id, "shell-1");
    assert_eq!(task.command, "echo hi");
    assert!(task.owner.is_none());
}

#[test]
fn mark_finished_transitions_from_running_to_finished() {
    let mut task = LocalShellTask::new("s", "cmd", None);
    task.mark_finished();
    assert!(matches!(task.state(), LocalShellTaskState::Finished));
}

#[test]
fn mark_finished_is_idempotent() {
    let mut task = LocalShellTask::new("s", "cmd", None);
    task.mark_finished();
    task.mark_finished(); // Second call must not panic / regress state.
    assert!(matches!(task.state(), LocalShellTaskState::Finished));
}

#[test]
fn finish_method_returns_ok_and_transitions_to_finished() {
    let mut task = LocalShellTask::new("s", "cmd", None);
    let outcome = task.finish();
    assert!(outcome.is_ok());
    assert!(matches!(task.state(), LocalShellTaskState::Finished));
}

#[test]
fn finish_from_already_finished_state_is_legal() {
    let mut task = LocalShellTask::new("s", "cmd", None);
    task.finish().expect("first finish");
    let outcome = task.finish();
    assert!(outcome.is_ok(), "Finished → Finished MUST be legal");
}

#[test]
fn local_shell_task_records_owner_teammate_id() {
    let owner = TeammateId::new();
    let task = LocalShellTask::new("s", "cmd", Some(owner.clone()));
    assert_eq!(task.owner.as_ref(), Some(&owner));
}

// ───────────────────────────────────────────────────────────────────────────
// Section I — tasks_for_agent filter
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn tasks_for_agent_filters_by_owner_id() {
    let alpha = TeammateId::new();
    let beta = TeammateId::new();

    let registry = vec![
        LocalShellTask::new("s1", "alpha-cmd-1", Some(alpha.clone())),
        LocalShellTask::new("s2", "beta-cmd", Some(beta.clone())),
        LocalShellTask::new("s3", "alpha-cmd-2", Some(alpha.clone())),
        LocalShellTask::new("s4", "unowned-cmd", None),
    ];

    let alpha_tasks = tasks_for_agent(&registry, &alpha);
    assert_eq!(alpha_tasks.len(), 2);
    let cmds: Vec<&str> = alpha_tasks.iter().map(|t| t.command.as_str()).collect();
    assert!(cmds.contains(&"alpha-cmd-1"));
    assert!(cmds.contains(&"alpha-cmd-2"));

    let beta_tasks = tasks_for_agent(&registry, &beta);
    assert_eq!(beta_tasks.len(), 1);
    assert_eq!(beta_tasks[0].command, "beta-cmd");
}

#[test]
fn tasks_for_agent_excludes_unowned_shells() {
    let alpha = TeammateId::new();
    let registry = vec![
        LocalShellTask::new("s-owned", "owned-cmd", Some(alpha.clone())),
        LocalShellTask::new("s-unowned", "unowned-cmd", None),
    ];
    let filtered = tasks_for_agent(&registry, &alpha);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].command, "owned-cmd");
}

#[test]
fn tasks_for_agent_empty_registry_yields_empty_result() {
    let alpha = TeammateId::new();
    let registry: Vec<LocalShellTask> = vec![];
    assert!(tasks_for_agent(&registry, &alpha).is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section J — PluginPolicy serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn plugin_policy_round_trips_through_json_preserving_all_fields() {
    let policy = PluginPolicy {
        strict_known_marketplaces: Some(vec![github("alice/plugin")]),
        blocked_marketplaces: vec![github("evil/repo")],
        skipped_marketplaces: vec!["sketchy".to_string()],
        skipped_plugins: vec!["bad@market".to_string()],
        managed: true,
        actions: Vec::new(),
    };
    let json = serde_json::to_string(&policy).expect("serialize");
    let back: PluginPolicy = serde_json::from_str(&json).expect("deserialize");
    assert!(back.strict_known_marketplaces.is_some());
    assert_eq!(back.blocked_marketplaces.len(), 1);
    assert_eq!(back.skipped_marketplaces, vec!["sketchy".to_string()]);
    assert_eq!(back.skipped_plugins, vec!["bad@market".to_string()]);
    assert!(back.managed);
}

#[test]
fn plugin_policy_default_serializes_with_only_required_fields() {
    let policy = PluginPolicy::default();
    let json = serde_json::to_string(&policy).expect("serialize");
    // Every collection has skip_serializing_if = "is_empty"
    // and every Option has skip_serializing_if = "is_none";
    // managed has skip_serializing_if = "Not::not". So a
    // default policy must serialize as an empty object.
    assert_eq!(json, "{}", "default policy MUST serialize to '{{}}'");
}
