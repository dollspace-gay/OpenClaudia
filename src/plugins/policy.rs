//! Enterprise policy controls for marketplace + plugin acceptance.
//!
//! Port of Claude Code's `strictKnownMarketplaces`, `blockedMarketplaces`,
//! `skippedMarketplaces`, and `skippedPlugins` settings. Each list
//! gates a different stage of the install pipeline:
//!
//! - **strict_known_marketplaces**: when set (non-empty), ONLY the
//!   marketplace sources named here can be added. Anything else is
//!   rejected BEFORE the download happens, matching Claude Code's
//!   "check happens before touching the filesystem" guarantee.
//! - **blocked_marketplaces**: hard blocklist — any source listed here
//!   is rejected even when it would otherwise be allowed by
//!   `strict_known_marketplaces`. Takes precedence.
//! - **skipped_marketplaces**: user has declined to re-prompt on this
//!   one. Non-enforcing — the marketplace can still be added
//!   explicitly; this just suppresses automatic prompts.
//! - **skipped_plugins**: plugin IDs (in `plugin@marketplace` form)
//!   the user declined. Install path skips these silently.
//!
//! A missing `strict_known_marketplaces` (`None`) means "no allowlist
//! enforcement" — matches Claude Code's semantics where the field
//! being absent is distinct from being an empty array. An empty
//! allowlist means "nothing is allowed", which is a valid config for
//! locked-down environments.

use serde::{Deserialize, Serialize};

use super::marketplace::MarketplaceSource;

/// Enterprise policy snapshot pulled from the settings.json layering
/// chain. The `managed` flag notes whether the policy is load-bearing
/// for compliance (so violations log at `warn`) or a user-level
/// preference (log at `debug`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginPolicy {
    /// Exclusive allowlist. `None` → no allowlist enforcement.
    /// `Some(empty)` → nothing is allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_known_marketplaces: Option<Vec<MarketplaceSource>>,
    /// Hard blocklist — takes precedence over the allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_marketplaces: Vec<MarketplaceSource>,
    /// User has declined to re-prompt on these names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_marketplaces: Vec<String>,
    /// Plugin IDs the user declined (`plugin@marketplace`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_plugins: Vec<String>,
    /// True when the policy was loaded from the managed-settings
    /// layer (i.e. applied by an administrator, not the user).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
}

/// Why a marketplace was rejected. Kept as an enum rather than a
/// string so callers (CLI, TUI error messages, audit logs) can
/// format their own human-readable text without string-matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyRejection {
    /// A `blocked_marketplaces` entry matched — rejection is final
    /// even if the source would also appear in the allowlist.
    Blocked,
    /// `strict_known_marketplaces` is set and no entry matches —
    /// the source is unknown to the policy.
    NotInAllowlist,
}

/// Decide whether `source` may be added under `policy`. Blocklist
/// takes precedence over allowlist. Returns `Ok(())` when allowed.
///
/// # Errors
///
/// Returns [`PolicyRejection::Blocked`] or
/// [`PolicyRejection::NotInAllowlist`] on rejection.
pub fn check_marketplace_allowed(
    source: &MarketplaceSource,
    policy: &PluginPolicy,
) -> Result<(), PolicyRejection> {
    if policy
        .blocked_marketplaces
        .iter()
        .any(|blocked| sources_match(source, blocked))
    {
        return Err(PolicyRejection::Blocked);
    }
    if let Some(allow) = policy.strict_known_marketplaces.as_ref() {
        let hit = allow.iter().any(|allowed| sources_match(source, allowed));
        if !hit {
            return Err(PolicyRejection::NotInAllowlist);
        }
    }
    Ok(())
}

/// True when `plugin_id` (`plugin@marketplace`) is on the user's
/// skipped list. Install flows should silently skip these rather
/// than surfacing an error — the user already said no.
#[must_use]
pub fn is_plugin_skipped(plugin_id: &str, policy: &PluginPolicy) -> bool {
    policy.skipped_plugins.iter().any(|s| s == plugin_id)
}

/// True when `name` is on the user's skipped-marketplaces list. Used
/// by "would you like to add …?" prompts to stay quiet.
#[must_use]
pub fn is_marketplace_skipped(name: &str, policy: &PluginPolicy) -> bool {
    policy.skipped_marketplaces.iter().any(|s| s == name)
}

/// Compare two [`MarketplaceSource`]s by identity — i.e. "do these
/// refer to the same upstream?". Match rules per variant:
///
/// - `GitHub`: repo names must match case-insensitively; ref / path
///   only compared when both entries specify them (empty on the
///   policy side wildcards out).
/// - `Git`: URL must match verbatim (trailing `.git` trimmed to
///   canonicalize); ref / path same wildcarding rule.
/// - `Url`: URL must match verbatim.
/// - `File` / `Directory`: absolute paths must match verbatim.
/// - Mixed variants never match.
fn sources_match(candidate: &MarketplaceSource, rule: &MarketplaceSource) -> bool {
    match (candidate, rule) {
        (
            MarketplaceSource::GitHub {
                repo: r1,
                git_ref: ref1,
                path: p1,
            },
            MarketplaceSource::GitHub {
                repo: r2,
                git_ref: ref2,
                path: p2,
            },
        ) => {
            r1.eq_ignore_ascii_case(r2)
                && wild_match_opt(ref1, ref2)
                && wild_match_opt(p1, p2)
        }
        (
            MarketplaceSource::Git {
                url: u1,
                git_ref: ref1,
                path: p1,
            },
            MarketplaceSource::Git {
                url: u2,
                git_ref: ref2,
                path: p2,
            },
        ) => {
            canonical_git(u1) == canonical_git(u2)
                && wild_match_opt(ref1, ref2)
                && wild_match_opt(p1, p2)
        }
        (
            MarketplaceSource::Url { url: u1, .. },
            MarketplaceSource::Url { url: u2, .. },
        ) => u1 == u2,
        (
            MarketplaceSource::File { path: p1 },
            MarketplaceSource::File { path: p2 },
        ) => p1 == p2,
        (
            MarketplaceSource::Directory { path: p1 },
            MarketplaceSource::Directory { path: p2 },
        ) => p1 == p2,
        _ => false,
    }
}

/// Matches two optional fields: `None` on the rule side wildcards
/// (any candidate matches); otherwise both must be `Some` and equal.
fn wild_match_opt(candidate: &Option<String>, rule: &Option<String>) -> bool {
    match (candidate, rule) {
        (_, None) => true,
        (Some(c), Some(r)) => c == r,
        (None, Some(_)) => false,
    }
}

/// Drop a trailing `.git` so `https://…/foo` and `https://…/foo.git`
/// compare equal. No other canonicalization — we don't want to
/// collapse `http://` and `https://` since the scheme is security-
/// relevant for the policy.
fn canonical_git(url: &str) -> String {
    url.strip_suffix(".git").unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn git(url: &str) -> MarketplaceSource {
        MarketplaceSource::Git {
            url: url.to_string(),
            git_ref: None,
            path: None,
        }
    }

    #[test]
    fn missing_allowlist_permits_everything() {
        let policy = PluginPolicy::default();
        assert!(check_marketplace_allowed(&github("anthropic/foo"), &policy).is_ok());
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![]),
            ..PluginPolicy::default()
        };
        assert_eq!(
            check_marketplace_allowed(&github("anthropic/foo"), &policy),
            Err(PolicyRejection::NotInAllowlist),
        );
    }

    #[test]
    fn blocklist_takes_precedence_over_allowlist() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github("anthropic/foo")]),
            blocked_marketplaces: vec![github("anthropic/foo")],
            ..PluginPolicy::default()
        };
        assert_eq!(
            check_marketplace_allowed(&github("anthropic/foo"), &policy),
            Err(PolicyRejection::Blocked),
        );
    }

    #[test]
    fn github_repo_match_is_case_insensitive() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github("Anthropic/Foo")]),
            ..PluginPolicy::default()
        };
        assert!(check_marketplace_allowed(&github("anthropic/foo"), &policy).is_ok());
        assert!(check_marketplace_allowed(&github("ANTHROPIC/FOO"), &policy).is_ok());
    }

    #[test]
    fn mixed_variants_never_match() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github("x/y")]),
            ..PluginPolicy::default()
        };
        // Even though it's the "same" repo conceptually, a git URL
        // entry can't satisfy a GitHub allowlist entry. This keeps
        // policy intent (explicit GitHub-only) from being bypassed
        // by a caller who drops back to the raw Git variant.
        assert_eq!(
            check_marketplace_allowed(&git("https://github.com/x/y"), &policy),
            Err(PolicyRejection::NotInAllowlist),
        );
    }

    #[test]
    fn canonical_git_strips_trailing_dot_git() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![git("https://example.com/foo.git")]),
            ..PluginPolicy::default()
        };
        // Same upstream, different spelling.
        assert!(
            check_marketplace_allowed(&git("https://example.com/foo"), &policy).is_ok()
        );
    }

    #[test]
    fn ref_wildcards_when_rule_is_unset() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github("x/y")]),
            ..PluginPolicy::default()
        };
        // Rule omits `ref` → any candidate ref is allowed.
        assert!(
            check_marketplace_allowed(&github_with_ref("x/y", "v2"), &policy).is_ok()
        );
    }

    #[test]
    fn ref_must_match_when_rule_specifies_it() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github_with_ref("x/y", "main")]),
            ..PluginPolicy::default()
        };
        assert!(
            check_marketplace_allowed(&github_with_ref("x/y", "main"), &policy).is_ok()
        );
        assert_eq!(
            check_marketplace_allowed(&github_with_ref("x/y", "dev"), &policy),
            Err(PolicyRejection::NotInAllowlist),
        );
        // A candidate missing the ref can't satisfy a ref-specific
        // rule — otherwise the rule would be trivially bypassable.
        assert_eq!(
            check_marketplace_allowed(&github("x/y"), &policy),
            Err(PolicyRejection::NotInAllowlist),
        );
    }

    #[test]
    fn skipped_plugins_and_marketplaces_detected() {
        let policy = PluginPolicy {
            skipped_plugins: vec!["foo@bar".to_string()],
            skipped_marketplaces: vec!["noisy-mp".to_string()],
            ..PluginPolicy::default()
        };
        assert!(is_plugin_skipped("foo@bar", &policy));
        assert!(!is_plugin_skipped("foo@other", &policy));
        assert!(is_marketplace_skipped("noisy-mp", &policy));
        assert!(!is_marketplace_skipped("other", &policy));
    }
}
