//! Enterprise policy controls for marketplace + plugin acceptance.
//!
//! Port of Claude Code's `strictKnownMarketplaces`, `blockedMarketplaces`,
//! `skippedMarketplaces`, and `skippedPlugins` settings. Each list
//! gates a different stage of the install pipeline:
//!
//! - **`strict_known_marketplaces`**: when set (non-empty), ONLY the
//!   marketplace sources named here can be added. Anything else is
//!   rejected BEFORE the download happens, matching Claude Code's
//!   "check happens before touching the filesystem" guarantee.
//! - **`blocked_marketplaces`**: hard blocklist — any source listed here
//!   is rejected even when it would otherwise be allowed by
//!   `strict_known_marketplaces`. Takes precedence.
//! - **`skipped_marketplaces`**: user has declined to re-prompt on this
//!   one. Non-enforcing — the marketplace can still be added
//!   explicitly; this just suppresses automatic prompts.
//! - **`skipped_plugins`**: plugin IDs (in `plugin@marketplace` form)
//!   the user declined. Install path skips these silently.
//!
//! A missing `strict_known_marketplaces` (`None`) means "no allowlist
//! enforcement" — matches Claude Code's semantics where the field
//! being absent is distinct from being an empty array. An empty
//! allowlist means "nothing is allowed", which is a valid config for
//! locked-down environments.

use serde::{Deserialize, Serialize};

use super::marketplace::MarketplaceSource;
use super::validate::PublicKey;

/// Actions that the policy can mandate on every plugin install.
///
/// Each variant represents an independent gate; the manager applies all
/// non-`None` actions in sequence before allowing an install to proceed.
#[derive(Debug, Clone)]
pub enum PolicyAction {
    /// Require a valid ed25519 signature over the manifest bytes. The install
    /// is rejected unless the plugin's `signature` field is present **and**
    /// verifies against at least one key in `trusted_keys`.
    ///
    /// Corresponds to crosslink #249 / #521 mandated-refactor point 2
    /// (signed-manifest scheme).
    RequireSignature {
        /// The set of trusted signer public keys (32-byte ed25519 verifying
        /// keys). An empty vec is treated as "no key is trusted" and will
        /// always reject.
        trusted_keys: Vec<PublicKey>,
    },
}

/// Enterprise policy snapshot pulled from the settings.json layering chain.
///
/// The `managed` flag notes whether the policy is load-bearing for compliance
/// (so violations log at `warn`) or a user-level preference (log at `debug`).
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
    /// Additional install-time actions enforced by this policy.
    ///
    /// Not serialized — `PolicyAction` variants carry non-serializable types
    /// (raw key bytes). Callers populate this field programmatically from the
    /// trust-key store after loading the base policy from JSON.
    #[serde(skip)]
    pub actions: Vec<PolicyAction>,
}

/// Why a marketplace was rejected. Kept as an enum rather than a
/// string so callers (CLI, TUI error messages, audit logs) can
/// format their own human-readable text without string-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
                && wild_match_opt(ref1.as_ref(), ref2.as_ref())
                && wild_match_opt(p1.as_ref(), p2.as_ref())
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
                && wild_match_opt(ref1.as_ref(), ref2.as_ref())
                && wild_match_opt(p1.as_ref(), p2.as_ref())
        }
        (MarketplaceSource::Url { url: u1, .. }, MarketplaceSource::Url { url: u2, .. }) => {
            u1 == u2
        }
        (MarketplaceSource::File { path: p1 }, MarketplaceSource::File { path: p2 }) => p1 == p2,
        (MarketplaceSource::Directory { path: p1 }, MarketplaceSource::Directory { path: p2 }) => {
            p1 == p2
        }
        _ => false,
    }
}

/// Matches two optional fields: `None` on the rule side wildcards
/// (any candidate matches); otherwise both must be `Some` and equal.
fn wild_match_opt(candidate: Option<&String>, rule: Option<&String>) -> bool {
    match (candidate, rule) {
        (_, None) => true,
        (Some(c), Some(r)) => c == r,
        (None, Some(_)) => false,
    }
}

/// Canonicalize a git URL for identity comparison.
///
/// Crosslink #861: the prior implementation only stripped a trailing
/// `.git`. Trailing slashes, mixed-case hostnames, default ports, and
/// percent-encoding all defeated the allowlist match, pushing
/// administrators to widen their allowlists.
///
/// The current implementation parses with `url::Url`, lowercases the
/// host, drops the default port for the scheme, normalizes the path
/// (trim trailing `/`, trim trailing `.git`), and emits a stable
/// `scheme://host[:port]/path` form. The scheme is preserved (not
/// collapsed) — http vs. https is security-relevant.
///
/// Falls back to a string-level cleanup (trailing slash + `.git`)
/// when the URL fails to parse, so SCP-style URLs (`git@github.com:owner/repo`)
/// continue to compare correctly.
fn canonical_git(url: &str) -> String {
    if let Ok(parsed) = ::url::Url::parse(url) {
        let scheme = parsed.scheme().to_ascii_lowercase();
        let host = parsed.host_str().map(str::to_ascii_lowercase);
        // Default ports: 22 ssh, 80 http, 443 https, 9418 git. `port_or_known_default`
        // returns the explicit port if it differs from the default; otherwise None.
        let port = parsed.port();
        let port_suffix = match (port, scheme.as_str()) {
            (Some(443), "https")
            | (Some(80), "http")
            | (Some(22), "ssh")
            | (Some(9418), "git")
            | (None, _) => String::new(),
            (Some(p), _) => format!(":{p}"),
        };
        let mut path = parsed.path().to_string();
        // Collapse trailing slashes.
        while path.ends_with('/') && path.len() > 1 {
            path.pop();
        }
        // Strip a single trailing `.git`.
        if let Some(stripped) = path.strip_suffix(".git") {
            path = stripped.to_string();
        }
        if let Some(h) = host {
            return format!("{scheme}://{h}{port_suffix}{path}");
        }
    }
    // Fallback: trim trailing slash + `.git`.
    let trimmed = url.trim_end_matches('/');
    trimmed.strip_suffix(".git").unwrap_or(trimmed).to_string()
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
        assert!(check_marketplace_allowed(&git("https://example.com/foo"), &policy).is_ok());
    }

    /// Crosslink #861: trailing slash variants must compare equal.
    #[test]
    fn canonical_git_handles_trailing_slash() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![git("https://example.com/foo")]),
            ..PluginPolicy::default()
        };
        assert!(check_marketplace_allowed(&git("https://example.com/foo/"), &policy).is_ok());
        assert!(check_marketplace_allowed(&git("https://example.com/foo.git/"), &policy).is_ok());
    }

    /// Crosslink #861: host comparison is case-insensitive.
    #[test]
    fn canonical_git_handles_mixed_case_host() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![git("https://example.com/foo")]),
            ..PluginPolicy::default()
        };
        assert!(check_marketplace_allowed(&git("https://Example.com/foo"), &policy).is_ok());
        assert!(check_marketplace_allowed(&git("https://EXAMPLE.COM/foo"), &policy).is_ok());
    }

    /// Crosslink #861: default ports are dropped before comparison.
    #[test]
    fn canonical_git_drops_default_ports() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![git("https://example.com/foo")]),
            ..PluginPolicy::default()
        };
        assert!(check_marketplace_allowed(&git("https://example.com:443/foo"), &policy).is_ok());
    }

    /// Crosslink #861: an attacker URL embedding the allowed prefix in
    /// its path must NOT match — `attacker.com/example.com/foo` should
    /// remain distinct from `example.com/foo` after canonicalization.
    #[test]
    fn canonical_git_rejects_host_in_path_attack() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![git("https://example.com/foo")]),
            ..PluginPolicy::default()
        };
        assert_eq!(
            check_marketplace_allowed(&git("https://attacker.com/example.com/foo"), &policy),
            Err(PolicyRejection::NotInAllowlist),
        );
    }

    #[test]
    fn ref_wildcards_when_rule_is_unset() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github("x/y")]),
            ..PluginPolicy::default()
        };
        // Rule omits `ref` → any candidate ref is allowed.
        assert!(check_marketplace_allowed(&github_with_ref("x/y", "v2"), &policy).is_ok());
    }

    #[test]
    fn ref_must_match_when_rule_specifies_it() {
        let policy = PluginPolicy {
            strict_known_marketplaces: Some(vec![github_with_ref("x/y", "main")]),
            ..PluginPolicy::default()
        };
        assert!(check_marketplace_allowed(&github_with_ref("x/y", "main"), &policy).is_ok());
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
