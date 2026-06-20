//! Ultrathink keyword detection and thinking-budget resolution.
//!
//! Port of Claude Code's `utils/thinking.ts` + `utils/effort.ts` effort
//! model. The trigger words `ultrathink`, `think ultra hard`, and
//! `think ultrahard` in any user message bump the effective effort to
//! `high`, which in turn raises the Anthropic thinking budget to
//! [`ULTRATHINK_BUDGET_TOKENS`] (matches Claude Code's `_Q0.ULTRATHINK`).
//!
//! Environment overrides, in precedence order:
//! 1. `CLAUDE_CODE_EFFORT_LEVEL=low|medium|high|max|xhigh` forces that effort.
//!    `unset` or `auto` disables the effort parameter entirely.
//! 2. `MAX_THINKING_TOKENS=<n>` forces a specific Anthropic budget.
//!
//! Otherwise the resolved effort is: keyword-bump → caller default.
//!
//! Claude Code also has `max` (Opus 4.6 only), which we treat as an
//! alias of `high` on non-Opus-4.6 models to match the API's clamp.

use serde_json::Value;

/// Claude Code's `ULTRATHINK` token budget (from
/// `_Q0.ULTRATHINK = 31999` in the minified source). Applied to the
/// Anthropic `thinking.budget_tokens` field when effort is `high`/`max`.
pub const ULTRATHINK_BUDGET_TOKENS: u32 = 31999;

/// Phrases that bump effort to `high`. Case-insensitive; match-anywhere
/// for the multi-word forms, word-boundary for the single-word form.
///
/// Stored as lower-case ASCII bytes so the scan can compare each haystack
/// byte against the needle without allocating a lower-cased copy of the
/// (potentially multi-MiB) input — see #897 / #915.
const ULTRATHINK_PHRASES: &[&[u8]] = &[b"think ultra hard", b"think ultrahard"];

/// Lower-case ASCII bytes of `ultrathink`, used by [`find_whole_word_ci`].
const ULTRATHINK_NEEDLE: &[u8] = b"ultrathink";

/// Scan `text` for any of the ultrathink trigger keywords.
///
/// Performs an ASCII-case-insensitive scan over `text.as_bytes()` without
/// allocating — the previous implementation called `text.to_lowercase()`
/// on every invocation, which copies the whole buffer (`O(N)` allocation
/// per turn for every message, see #897 / #915). Non-ASCII bytes are
/// compared exactly; this matches the trigger words which are ASCII.
#[must_use]
pub fn has_ultrathink_keyword(text: &str) -> bool {
    let bytes = text.as_bytes();
    // `ultrathink` must be a whole word (not part of a longer ident).
    if find_whole_word_ci(bytes, ULTRATHINK_NEEDLE) {
        return true;
    }
    ULTRATHINK_PHRASES.iter().any(|p| contains_ci(bytes, p))
}

/// Return `true` if `haystack` (raw bytes) contains `needle` (lower-case
/// ASCII bytes) using ASCII-case-insensitive comparison. Allocation-free.
fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let last = haystack.len() - needle.len();
    (0..=last).any(|i| matches_ci_at(haystack, i, needle))
}

/// Return `true` if `haystack` contains `needle` (lower-case ASCII bytes)
/// bordered on both sides by non-alphanumeric/underscore bytes (or the
/// string end) — case-insensitive port of JavaScript's `\b<needle>\b`.
fn find_whole_word_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let last = haystack.len() - needle.len();
    for i in 0..=last {
        if !matches_ci_at(haystack, i, needle) {
            continue;
        }
        let before_ok = i == 0 || !is_word_byte(haystack[i - 1]);
        let after = i + needle.len();
        let after_ok = after == haystack.len() || !is_word_byte(haystack[after]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// ASCII-case-insensitive byte-wise equality of `haystack[at..at+needle.len()]`
/// to `needle` (which must already be lower-case ASCII).
fn matches_ci_at(haystack: &[u8], at: usize, needle: &[u8]) -> bool {
    needle
        .iter()
        .enumerate()
        .all(|(j, &n)| haystack[at + j].to_ascii_lowercase() == n)
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan all `user`-role messages in `messages` for an ultrathink
/// trigger. Claude Code uses `max(user_message_budgets)`; the effect is
/// the same — a single hit promotes the whole turn.
#[must_use]
pub fn has_ultrathink_in_messages(messages: &[Value]) -> bool {
    messages.iter().any(|m| {
        m.get("role").and_then(|r| r.as_str()) == Some("user")
            && m.get("content")
                .and_then(|c| c.as_str())
                .is_some_and(has_ultrathink_keyword)
    })
}

/// Parse `CLAUDE_CODE_EFFORT_LEVEL`. Returns:
/// - `Some(Some(level))` for a recognized level (`low`/`medium`/`high`/`max`/`xhigh`)
/// - `Some(None)` for `unset`/`auto` (disable effort parameter)
/// - `None` if the env var is absent or unparseable
#[must_use]
pub fn env_effort_override() -> Option<Option<String>> {
    let raw = std::env::var("CLAUDE_CODE_EFFORT_LEVEL").ok()?;
    let lower = raw.to_lowercase();
    match lower.as_str() {
        "unset" | "auto" => Some(None),
        "none" | "minimal" | "low" | "medium" | "high" | "max" => Some(Some(lower)),
        "xhigh" => Some(Some("max".to_string())),
        _ => None,
    }
}

/// Parse `MAX_THINKING_TOKENS` (Claude Code's pre-`effort` env var).
/// Returns `Some(n)` only for strictly positive integers.
#[must_use]
pub fn env_max_thinking_tokens() -> Option<u32> {
    std::env::var("MAX_THINKING_TOKENS")
        .ok()?
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
}

/// Resolve the effective effort for a single turn, following the
/// precedence chain: env → ultrathink keyword → caller default.
///
/// Returns `None` when effort should be omitted from the request (env
/// set to `unset`/`auto`).
#[must_use]
pub fn resolve_effort(base_effort: &str, messages: &[Value]) -> Option<String> {
    if let Some(env) = env_effort_override() {
        return env;
    }
    if has_ultrathink_in_messages(messages) {
        return Some("high".to_string());
    }
    Some(base_effort.to_string())
}

/// Resolve the Anthropic `thinking.budget_tokens` value for this turn.
/// Returns `None` when no thinking block should be attached.
///
/// - `MAX_THINKING_TOKENS` env var wins outright (matches Claude Code).
/// - `high`, `xhigh`, and `max` → [`ULTRATHINK_BUDGET_TOKENS`].
/// - `low`/`medium`/other → `None` (no thinking).
#[must_use]
pub fn anthropic_thinking_budget(effort: Option<&str>) -> Option<u32> {
    if let Some(n) = env_max_thinking_tokens() {
        return Some(n);
    }
    match effort? {
        "high" | "xhigh" | "max" => Some(ULTRATHINK_BUDGET_TOKENS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_plain_ultrathink() {
        assert!(has_ultrathink_keyword("please ultrathink this"));
        assert!(has_ultrathink_keyword("ULTRATHINK!"));
        assert!(has_ultrathink_keyword("ultrathink"));
    }

    #[test]
    fn rejects_ultrathink_embedded_in_word() {
        // `bultrathink` (matches the minified source's bultrathink string)
        // must NOT trigger — word boundary required.
        assert!(!has_ultrathink_keyword("bultrathink"));
        assert!(!has_ultrathink_keyword("myultrathinker"));
        assert!(!has_ultrathink_keyword("ultrathink_variant"));
    }

    #[test]
    fn detects_think_ultra_hard_variants() {
        assert!(has_ultrathink_keyword("think ultra hard about this"));
        assert!(has_ultrathink_keyword("THINK ULTRAHARD"));
    }

    /// #897 / #915: `has_ultrathink_keyword` must behave correctly on
    /// large inputs without depending on `to_lowercase()` allocation.
    /// We exercise a 256 KiB haystack containing the trigger near the
    /// end and assert both detection and the negative case at the same
    /// scale — if the allocation were re-introduced, the test would
    /// still pass functionally, but the scan now operates on the raw
    /// byte slice (see implementation).
    #[test]
    fn large_input_case_insensitive_match() {
        let mut hay = "x".repeat(256 * 1024);
        // negative: no trigger anywhere
        assert!(!has_ultrathink_keyword(&hay));
        // positive: append the trigger in mixed case
        hay.push_str(" UltraThink ");
        assert!(has_ultrathink_keyword(&hay));
        // negative: trigger embedded mid-identifier must still be rejected
        let embedded = "x".repeat(64 * 1024) + "myUltraThinker" + &"y".repeat(64 * 1024);
        assert!(!has_ultrathink_keyword(&embedded));
        // positive: multi-word variant deep inside large buffer
        let phrase = "z".repeat(128 * 1024) + " THINK ULTRA HARD " + &"q".repeat(128 * 1024);
        assert!(has_ultrathink_keyword(&phrase));
    }

    #[test]
    fn scans_user_messages_only() {
        let messages = vec![
            json!({"role": "system", "content": "ultrathink"}),
            json!({"role": "assistant", "content": "ultrathink"}),
            json!({"role": "user", "content": "hi"}),
        ];
        assert!(!has_ultrathink_in_messages(&messages));

        let messages2 = vec![json!({"role": "user", "content": "please ultrathink"})];
        assert!(has_ultrathink_in_messages(&messages2));
    }

    #[test]
    fn resolve_effort_honors_keyword() {
        // Temporarily drop any ambient env var for a deterministic test.
        // SAFETY: tests in this module run single-threaded per cargo-test;
        // the parallel runner doesn't mutate this specific env var elsewhere.
        let prev = std::env::var("CLAUDE_CODE_EFFORT_LEVEL").ok();
        unsafe {
            std::env::remove_var("CLAUDE_CODE_EFFORT_LEVEL");
        }
        let msgs = vec![json!({"role": "user", "content": "ultrathink this"})];
        assert_eq!(resolve_effort("medium", &msgs), Some("high".to_string()));
        let plain = vec![json!({"role": "user", "content": "hi"})];
        assert_eq!(resolve_effort("medium", &plain), Some("medium".to_string()));
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("CLAUDE_CODE_EFFORT_LEVEL", v);
            }
        }
    }

    #[test]
    fn anthropic_budget_high_is_ultrathink() {
        let prev = std::env::var("MAX_THINKING_TOKENS").ok();
        unsafe {
            std::env::remove_var("MAX_THINKING_TOKENS");
        }
        assert_eq!(
            anthropic_thinking_budget(Some("high")),
            Some(ULTRATHINK_BUDGET_TOKENS)
        );
        assert_eq!(
            anthropic_thinking_budget(Some("max")),
            Some(ULTRATHINK_BUDGET_TOKENS)
        );
        assert_eq!(
            anthropic_thinking_budget(Some("xhigh")),
            Some(ULTRATHINK_BUDGET_TOKENS)
        );
        assert_eq!(anthropic_thinking_budget(Some("medium")), None);
        assert_eq!(anthropic_thinking_budget(Some("low")), None);
        assert_eq!(anthropic_thinking_budget(None), None);
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("MAX_THINKING_TOKENS", v);
            }
        }
    }
}
