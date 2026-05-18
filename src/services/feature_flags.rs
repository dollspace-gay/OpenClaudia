//! Feature-flag source — boolean gates for opt-in code paths.
//!
//! Port of Claude Code's `getFeatureValue_CACHED_MAY_BE_STALE` +
//! GrowthBook-backed flag lookups, stripped down to the local case.
//! OC doesn't have a remote flag backend; every flag defaults to
//! `false` unless explicitly turned on via [`StaticFlags::set`] or the
//! `OPENCLAUDIA_FEATURE_<NAME>` environment variable.
//!
//! Flag names are expected to be `snake_case`. The env-var lookup
//! uppercases the name and prepends `OPENCLAUDIA_FEATURE_`, so
//! `ultrathink_enabled` reads `OPENCLAUDIA_FEATURE_ULTRATHINK_ENABLED`.

use std::collections::HashMap;

/// Trait for resolving feature flags. Separate from
/// [`crate::services::AnalyticsSink`] so a backend that wires one
/// doesn't need to implement the other.
pub trait FeatureFlagSource: Send + Sync {
    /// Resolve `name` → `true` / `false`. Unknown flags default to
    /// `false` — matches Claude Code's "opt-in" semantic for GB-backed
    /// flags.
    fn is_enabled(&self, name: &str) -> bool;
}

/// Default implementation backed by a `HashMap<String, bool>` +
/// environment-variable fallback. `set` writes are intentionally
/// build-time / startup-time — no lock overhead on the `is_enabled`
/// hot path at the cost of needing `&mut self` for mutation. Wrap in
/// `Arc<RwLock<>>` if you need concurrent updates.
#[derive(Debug, Default, Clone)]
pub struct StaticFlags {
    entries: HashMap<String, bool>,
}

impl StaticFlags {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed a flag. Called at startup / test setup.
    pub fn set(&mut self, name: &str, value: bool) {
        self.entries.insert(name.to_string(), value);
    }

    /// Chainable alias for `set` — convenient in test fixtures.
    #[must_use]
    pub fn with(mut self, name: &str, value: bool) -> Self {
        self.set(name, value);
        self
    }

    /// Resolve via env var: `OPENCLAUDIA_FEATURE_<UPPER>` where
    /// truthy = `1` / `true` / `on` / `yes` (case-insensitive).
    /// Any other value → `false`. Missing env var → `None`, letting
    /// the caller fall through to the map.
    fn env_override(name: &str) -> Option<bool> {
        let upper = name.to_ascii_uppercase();
        let key = format!("OPENCLAUDIA_FEATURE_{upper}");
        let raw = std::env::var(&key).ok()?;
        Some(matches!(
            raw.to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ))
    }
}

impl FeatureFlagSource for StaticFlags {
    fn is_enabled(&self, name: &str) -> bool {
        // Env var wins — operators / CI can override the compiled
        // defaults without a rebuild. Matches the precedence used
        // elsewhere (MAX_THINKING_TOKENS, CLAUDE_CODE_EFFORT_LEVEL).
        if let Some(env) = Self::env_override(name) {
            return env;
        }
        self.entries.get(name).copied().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `StaticFlags::env_override` reads an env var → tests that
    /// touch it must serialize. Shared across the module.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct EnvGuard {
        key: String,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var(&self.key, v),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }

    #[test]
    fn unknown_flag_defaults_false() {
        let _lock = env_lock();
        let flags = StaticFlags::new();
        assert!(!flags.is_enabled("missing"));
    }

    #[test]
    fn explicit_set_wins_over_default() {
        let _lock = env_lock();
        let flags = StaticFlags::new().with("fast_path", true);
        assert!(flags.is_enabled("fast_path"));
    }

    #[test]
    fn env_var_overrides_map_entry() {
        let _lock = env_lock();
        let flags = StaticFlags::new().with("beta_feature", false);
        // With map=false, no env override yet: should be false.
        assert!(!flags.is_enabled("beta_feature"));

        let _g = EnvGuard::set("OPENCLAUDIA_FEATURE_BETA_FEATURE", "1");
        assert!(flags.is_enabled("beta_feature"));
    }

    #[test]
    fn env_var_accepts_truthy_variants() {
        let _lock = env_lock();
        let flags = StaticFlags::new();
        for truthy in ["1", "true", "on", "yes", "TRUE", "Yes", "ON"] {
            let _g = EnvGuard::set("OPENCLAUDIA_FEATURE_GATE", truthy);
            assert!(
                flags.is_enabled("gate"),
                "expected '{truthy}' to count as truthy"
            );
        }
    }

    #[test]
    fn env_var_rejects_non_truthy_variants() {
        let _lock = env_lock();
        let flags = StaticFlags::new();
        for falsy in ["0", "false", "off", "no", "random"] {
            let _g = EnvGuard::set("OPENCLAUDIA_FEATURE_GATE", falsy);
            assert!(
                !flags.is_enabled("gate"),
                "expected '{falsy}' to count as falsy"
            );
        }
    }

    #[test]
    fn env_var_falls_through_to_map_when_unset() {
        let _lock = env_lock();
        // Ensure no ambient env var exists before the test runs.
        unsafe {
            std::env::remove_var("OPENCLAUDIA_FEATURE_MAP_ONLY");
        }
        let flags = StaticFlags::new().with("map_only", true);
        assert!(flags.is_enabled("map_only"));
    }

    // ── B2 spec pins (#536 §B2) ──────────────────────────────────────────────

    /// B2: env var key is `OPENCLAUDIA_FEATURE_` + uppercased flag name.
    /// `snake_case` names must be uppercased exactly. Pins the key-construction
    /// logic in `env_override` (`feature_flags.rs` lines 57-65).
    #[test]
    fn b2_env_var_key_is_prefix_plus_upper_name() {
        let _lock = env_lock();
        let _g = EnvGuard::set("OPENCLAUDIA_FEATURE_SNAKE_CASE_FLAG", "1");
        unsafe {
            std::env::remove_var("OPENCLAUDIA_FEATURE_snake_case_flag");
        }
        let flags = StaticFlags::new();
        assert!(
            flags.is_enabled("snake_case_flag"),
            "env key must be OPENCLAUDIA_FEATURE_SNAKE_CASE_FLAG"
        );
    }

    /// B2: empty-string env var evaluates to `false` (not a fall-through to
    /// the map). Spec §B2 edge-case: empty string is not in the truthy set
    /// `{1, true, on, yes}`, so `env_override` returns `Some(false)` which
    /// takes precedence over any map entry. Pins the CURRENT behaviour; if the
    /// impl is changed to treat `""` as absent, update this test and the spec.
    #[test]
    fn b2_empty_env_var_evaluates_to_false_not_fallthrough() {
        let _lock = env_lock();
        let _g = EnvGuard::set("OPENCLAUDIA_FEATURE_EMPTY_VAR_FLAG", "");
        // Map has `true`; env var is `""` (not truthy) → env wins with false.
        let flags = StaticFlags::new().with("empty_var_flag", true);
        assert!(
            !flags.is_enabled("empty_var_flag"),
            "empty env var must evaluate to false, not fall through to map=true"
        );
    }

    /// B2: `with()` builder is chainable and last write wins for the same key.
    #[test]
    fn b2_with_builder_is_chainable_and_last_write_wins() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var("OPENCLAUDIA_FEATURE_OVERWRITTEN");
        }
        let flags = StaticFlags::new()
            .with("overwritten", false)
            .with("overwritten", true);
        assert!(flags.is_enabled("overwritten"));
    }

    /// B2: falsy env var overrides a map `true` entry — the spec states "env
    /// var wins" unconditionally (§B2, resolution order step 1).
    #[test]
    fn b2_env_false_overrides_map_true() {
        let _lock = env_lock();
        let flags = StaticFlags::new().with("env_wins_flag", true);
        let _g = EnvGuard::set("OPENCLAUDIA_FEATURE_ENV_WINS_FLAG", "0");
        assert!(
            !flags.is_enabled("env_wins_flag"),
            "falsy env var must override map=true"
        );
    }

    /// B2: unknown flag with no env var always returns `false` — matches
    /// Claude Code's opt-in semantic (unknown → disabled by default).
    #[test]
    fn b2_unknown_flag_no_env_var_returns_false() {
        let _lock = env_lock();
        unsafe {
            std::env::remove_var("OPENCLAUDIA_FEATURE_TOTALLY_UNKNOWN_XYZ");
        }
        let flags = StaticFlags::new();
        assert!(!flags.is_enabled("totally_unknown_xyz"));
    }
}
