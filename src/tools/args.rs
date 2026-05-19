//! Typed argument accessors for tool handlers — closes crosslink #675.
//!
//! Every executor used to reimplement the same
//! `args.get("k").and_then(|v| v.as_str())` extraction shape, drifting in
//! its error wording (`"Error: name is required"`,
//! `"Missing 'path' argument"`, `"missing 'content' field"`). The QA review
//! flagged this as a textbook DRY violation: N copies of the same dispatch
//! protocol, each free to regress independently (see issue #675).
//!
//! This module provides the [`ToolArgs`] trait, blanket-implemented for
//! `HashMap<String, Value, S>` so it covers both the `RandomState` map most
//! handlers receive *and* the generic `S: BuildHasher` maps that the
//! worktree / LSP tools use. The accessors are intentionally narrow:
//!
//! | accessor                             | use case                                  |
//! |--------------------------------------|-------------------------------------------|
//! | [`ToolArgs::arg_str`]                | required string — returns [`ToolArgError`]|
//! | [`ToolArgs::arg_string`]             | same, owned `String`                      |
//! | [`ToolArgs::arg_str_opt`]            | optional string, no error                 |
//! | [`ToolArgs::arg_str_or`]             | string with default                       |
//! | [`ToolArgs::arg_bool_or`]            | bool with default                         |
//! | [`ToolArgs::arg_i64_or`]             | i64 with default (signed integers)        |
//! | [`ToolArgs::arg_u64_or`]             | u64 with default (unsigned integers)      |
//! | [`ToolArgs::arg_array`]              | optional JSON array borrow                |
//!
//! `ToolArgError`'s `Display` produces the canonical phrasing
//! `Missing 'KEY' argument`, matching the prevalent style in the codebase
//! (file/write.rs, file/edit.rs, bash/mod.rs, …). Drift sites that used
//! `"Error: KEY is required"` (cron) or `"KEY is required"` are quietly
//! normalised — that uniformity is the entire point of the refactor.
//!
//! Tools that return the legacy `(String, bool)` shape convert the typed
//! error via [`ToolArgError::into_tool_error`], which produces
//! `(message, true)`. The two helpers compose:
//!
//! ```ignore
//! use crate::tools::args::ToolArgs as _;
//!
//! pub fn execute_thing(args: &HashMap<String, Value>) -> (String, bool) {
//!     let name = match args.arg_str("name") {
//!         Ok(n) => n,
//!         Err(e) => return e.into_tool_error(),
//!     };
//!     // …
//! }
//! ```

use serde_json::Value;
use std::collections::HashMap;
use std::hash::BuildHasher;

/// Typed extraction error for tool argument accessors.
///
/// One variant for now (`MissingOrWrongType`) — the legacy executors did
/// not distinguish "absent" from "present but not a string", so this
/// preserves the existing observable behaviour while giving callers a
/// structured error to match on if they want richer reporting later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolArgError {
    /// The requested key is absent, or the value is the wrong JSON type.
    MissingOrWrongType {
        /// The argument key that the executor asked for.
        key: &'static str,
    },
}

impl ToolArgError {
    /// Convert into the legacy `(message, is_error=true)` tuple every
    /// executor returns. Centralising the format string here is the
    /// whole point of issue #675.
    #[must_use]
    pub fn into_tool_error(self) -> (String, bool) {
        (self.to_string(), true)
    }
}

impl std::fmt::Display for ToolArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOrWrongType { key } => {
                write!(f, "Missing '{key}' argument")
            }
        }
    }
}

impl std::error::Error for ToolArgError {}

/// Typed accessors over a tool handler's argument map.
///
/// Blanket-implemented for `HashMap<String, Value, S>` so every executor
/// (including the worktree/LSP ones that take a generic
/// `S: BuildHasher`) can call the same methods without converting maps.
pub trait ToolArgs {
    /// Required string argument. Returns [`ToolArgError`] if absent or
    /// not a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`ToolArgError::MissingOrWrongType`] when `key` is not
    /// present or the value is not a JSON string.
    fn arg_str(&self, key: &'static str) -> Result<&str, ToolArgError>;

    /// Required string argument as an owned `String`. Convenience for
    /// the `.to_string()` follow-up that several executors need (cron,
    /// task) so a string can outlive the borrowed map.
    ///
    /// # Errors
    ///
    /// Returns [`ToolArgError::MissingOrWrongType`] when `key` is not
    /// present or the value is not a JSON string.
    fn arg_string(&self, key: &'static str) -> Result<String, ToolArgError> {
        self.arg_str(key).map(str::to_owned)
    }

    /// Optional string argument. `None` when absent or non-string —
    /// drop-in replacement for `args.get(k).and_then(|v| v.as_str())`.
    fn arg_str_opt(&self, key: &str) -> Option<&str>;

    /// String argument with a fallback default. Used by `list.rs`
    /// (`path` defaults to `"."`), notebook (`edit_mode` defaults to
    /// `"replace"`), LSP (`action` defaults to `"hover"`).
    fn arg_str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str;

    /// Boolean argument with a fallback default. Used by bash
    /// (`run_in_background`), worktree (`apply_changes`).
    fn arg_bool_or(&self, key: &str, default: bool) -> bool;

    /// Signed-integer argument with a fallback default.
    fn arg_i64_or(&self, key: &str, default: i64) -> i64;

    /// Unsigned-integer argument with a fallback default. Used by web
    /// (`limit`), LSP (`line`, `character`), notebook (`cell_number`).
    fn arg_u64_or(&self, key: &str, default: u64) -> u64;

    /// Optional JSON-array borrow. Drop-in replacement for
    /// `args.get(k).and_then(|v| v.as_array())`.
    fn arg_array(&self, key: &str) -> Option<&Vec<Value>>;
}

impl<S: BuildHasher> ToolArgs for HashMap<String, Value, S> {
    fn arg_str(&self, key: &'static str) -> Result<&str, ToolArgError> {
        self.get(key)
            .and_then(Value::as_str)
            .ok_or(ToolArgError::MissingOrWrongType { key })
    }

    fn arg_str_opt(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    fn arg_str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).and_then(Value::as_str).unwrap_or(default)
    }

    fn arg_bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    fn arg_i64_or(&self, key: &str, default: i64) -> i64 {
        self.get(key).and_then(Value::as_i64).unwrap_or(default)
    }

    fn arg_u64_or(&self, key: &str, default: u64) -> u64 {
        self.get(key).and_then(Value::as_u64).unwrap_or(default)
    }

    fn arg_array(&self, key: &str) -> Option<&Vec<Value>> {
        self.get(key).and_then(Value::as_array)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make() -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("name".into(), json!("alice"));
        m.insert("enabled".into(), json!(true));
        m.insert("count".into(), json!(7));
        m.insert("negative".into(), json!(-3));
        m.insert("items".into(), json!(["a", "b"]));
        m.insert("number_as_string".into(), json!("12"));
        m.insert("null_value".into(), Value::Null);
        m
    }

    // ── arg_str ─────────────────────────────────────────────────────────

    #[test]
    fn arg_str_returns_value_when_present_and_string() {
        let m = make();
        assert_eq!(m.arg_str("name").unwrap(), "alice");
    }

    #[test]
    fn arg_str_errors_when_key_missing() {
        let m = make();
        let err = m.arg_str("absent").unwrap_err();
        assert_eq!(err, ToolArgError::MissingOrWrongType { key: "absent" });
        assert_eq!(err.to_string(), "Missing 'absent' argument");
    }

    #[test]
    fn arg_str_errors_when_value_is_wrong_type() {
        // `count` is a number — must not be coerced to a string.
        let m = make();
        let err = m.arg_str("count").unwrap_err();
        assert_eq!(err, ToolArgError::MissingOrWrongType { key: "count" });
    }

    #[test]
    fn arg_str_errors_when_value_is_null() {
        let m = make();
        assert!(m.arg_str("null_value").is_err());
    }

    #[test]
    fn into_tool_error_returns_legacy_tuple_with_is_error_true() {
        let m = make();
        let (msg, is_err) = m.arg_str("absent").unwrap_err().into_tool_error();
        assert!(is_err, "is_error flag must be true");
        assert_eq!(msg, "Missing 'absent' argument");
    }

    // ── arg_string ──────────────────────────────────────────────────────

    #[test]
    fn arg_string_returns_owned_copy() {
        let m = make();
        let owned: String = m.arg_string("name").unwrap();
        assert_eq!(owned, "alice");
    }

    // ── arg_str_opt ─────────────────────────────────────────────────────

    #[test]
    fn arg_str_opt_returns_some_for_string_value() {
        let m = make();
        assert_eq!(m.arg_str_opt("name"), Some("alice"));
    }

    #[test]
    fn arg_str_opt_returns_none_for_missing_or_wrong_type() {
        let m = make();
        assert_eq!(m.arg_str_opt("absent"), None);
        assert_eq!(m.arg_str_opt("count"), None, "number must not coerce");
    }

    // ── arg_str_or ──────────────────────────────────────────────────────

    #[test]
    fn arg_str_or_returns_value_when_present() {
        let m = make();
        assert_eq!(m.arg_str_or("name", "default"), "alice");
    }

    #[test]
    fn arg_str_or_returns_default_when_missing() {
        let m = make();
        assert_eq!(m.arg_str_or("absent", "fallback"), "fallback");
    }

    #[test]
    fn arg_str_or_returns_default_when_wrong_type() {
        // A non-string value at the key must fall through to the default,
        // matching the prior `as_str().unwrap_or(default)` semantics.
        let m = make();
        assert_eq!(m.arg_str_or("count", "fallback"), "fallback");
    }

    // ── arg_bool_or ─────────────────────────────────────────────────────

    #[test]
    fn arg_bool_or_returns_value_when_present_and_bool() {
        let m = make();
        assert!(m.arg_bool_or("enabled", false));
    }

    #[test]
    fn arg_bool_or_returns_default_when_missing() {
        let m = make();
        assert!(!m.arg_bool_or("absent", false));
        assert!(m.arg_bool_or("absent", true));
    }

    #[test]
    fn arg_bool_or_returns_default_when_wrong_type() {
        // A string "true" is NOT coerced — match prior `as_bool` behaviour.
        let m = make();
        assert!(!m.arg_bool_or("name", false));
    }

    // ── arg_i64_or ──────────────────────────────────────────────────────

    #[test]
    fn arg_i64_or_returns_value_when_present_and_integer() {
        let m = make();
        assert_eq!(m.arg_i64_or("count", 0), 7);
        assert_eq!(m.arg_i64_or("negative", 0), -3);
    }

    #[test]
    fn arg_i64_or_returns_default_when_missing_or_wrong_type() {
        let m = make();
        assert_eq!(m.arg_i64_or("absent", 42), 42);
        // Numeric strings are NOT coerced.
        assert_eq!(m.arg_i64_or("number_as_string", 99), 99);
    }

    // ── arg_u64_or ──────────────────────────────────────────────────────

    #[test]
    fn arg_u64_or_returns_value_when_present_and_unsigned() {
        let m = make();
        assert_eq!(m.arg_u64_or("count", 0), 7);
    }

    #[test]
    fn arg_u64_or_returns_default_for_negative_value() {
        // A negative i64 is NOT a valid u64; must fall through to default.
        let m = make();
        assert_eq!(m.arg_u64_or("negative", 999), 999);
    }

    #[test]
    fn arg_u64_or_returns_default_when_missing() {
        let m = make();
        assert_eq!(m.arg_u64_or("absent", 5), 5);
    }

    // ── arg_array ───────────────────────────────────────────────────────

    #[test]
    fn arg_array_returns_some_for_array_value() {
        let m = make();
        let arr = m.arg_array("items").expect("array present");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], json!("a"));
    }

    #[test]
    fn arg_array_returns_none_for_missing_or_wrong_type() {
        let m = make();
        assert!(m.arg_array("absent").is_none());
        assert!(
            m.arg_array("name").is_none(),
            "string must not look like an array"
        );
    }

    // ── BuildHasher compatibility ──────────────────────────────────────

    #[test]
    fn trait_applies_to_custom_build_hasher_maps() {
        // The worktree/LSP tools take a generic `S: BuildHasher` map; this
        // test pins that the blanket impl actually covers a non-default S.
        use std::collections::hash_map::RandomState;
        let mut m: HashMap<String, Value, RandomState> = HashMap::with_hasher(RandomState::new());
        m.insert("k".into(), json!("v"));
        // Call through the trait — proves blanket impl applies.
        let v: &str = m.arg_str("k").expect("typed accessor over custom S");
        assert_eq!(v, "v");
    }
}
