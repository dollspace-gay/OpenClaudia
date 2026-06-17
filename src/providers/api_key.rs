//! A redacting newtype for provider API keys.
//!
//! The sole purpose of [`ApiKey`] is to make it *structurally impossible* for
//! a raw secret to land in log output. Every place that formats an `ApiKey`
//! with `{:?}` or `{}` sees the redacted form. Call sites that need the raw
//! value (HTTP header construction) must reach for [`ApiKey::as_str`]
//! explicitly — an audit point that is easy to grep for.
//!
//! The validation performed by [`ApiKey::try_from_string`] (empty / control
//! char / non-ASCII rejection) closes the CRLF-injection vector into the
//! `Authorization` / `x-api-key` headers. It runs once on config load via
//! `serde::Deserialize` so a bad key in YAML surfaces a clear error at
//! startup rather than five layers deep inside a failed HTTP request.
//!
//! See crosslink #256.
//!
//! # Intentionally NOT implemented
//!
//! * [`Copy`]: keys should never be silently duplicated.
//! * [`std::hash::Hash`]: secrets are never valid keys in a `HashMap`/`HashSet`.
//! * `Deref<Target = str>` or `AsRef<str>`: callers must opt-in to raw
//!   access via [`ApiKey::as_str`]. The name is the audit trail.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use thiserror::Error;

/// Errors that can occur when constructing an [`ApiKey`] from a raw string.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApiKeyError {
    /// The value was empty or entirely whitespace.
    #[error("API key is empty or whitespace-only")]
    Empty,

    /// The value contained a byte outside the ASCII range. Such a value
    /// would also fail `reqwest::HeaderValue::from_str`; we reject earlier
    /// with a clearer error path.
    #[error("API key contains non-ASCII bytes (would fail header construction)")]
    NonAscii,

    /// The value contained an ASCII control character (`\r`, `\n`, `\0`,
    /// tab, …). This is the CRLF-injection guard — a malicious config or
    /// header value containing `\r\n` could otherwise smuggle additional
    /// HTTP headers into the outbound request.
    #[error("API key contains control character U+{codepoint:04X} (CRLF injection guard)")]
    ControlChar {
        /// The offending control character's Unicode scalar value.
        codepoint: u32,
    },

    /// The value exceeded [`MAX_API_KEY_LEN`] bytes. Legitimate API keys are
    /// well under this cap (Anthropic: ~108 chars, `OpenAI`: ~56); an
    /// 8 KiB header is an attack shape, not a real key. See crosslink #452.
    #[error("API key is {actual} bytes, exceeding the {max}-byte cap")]
    TooLong {
        /// Observed length of the rejected value.
        actual: usize,
        /// The cap that was exceeded.
        max: usize,
    },
}

/// Upper bound on the byte length of an accepted API key.
///
/// Anthropic, `OpenAI`, Google, Z.AI, Kimi/Moonshot, and `MiniMax` keys are all
/// well under 200 bytes; 512 gives the occasional long session/project-scoped
/// key enough room while refusing 8 KiB attack payloads. See crosslink #452.
pub const MAX_API_KEY_LEN: usize = 512;

/// A provider API key whose `Debug` and `Display` impls redact the middle of
/// the value.
///
/// Construct via [`ApiKey::try_from_string`] (or `serde::Deserialize`, which
/// delegates to it). Retrieve the raw value for HTTP calls via
/// [`ApiKey::as_str`].
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Borrow the raw key value for genuine cases that need it unredacted
    /// (notably round-trip persistence — see [`Self::expose_raw_for_serialization`]
    /// for the audit point used by `Serialize`).
    ///
    /// This is **unsafe** in the sense of "easy to misuse, not memory-unsafe":
    /// the return value MUST NOT be logged, traced, printed, or surfaced in
    /// error messages. The function name is deliberately verbose so that every
    /// call site shows up under `git grep expose_raw`.
    ///
    /// # Safety
    /// The caller asserts that the returned `&str` will not escape into any
    /// log, error, or otherwise human-readable sink. The compiler cannot prove
    /// this; reviewers must.
    #[must_use]
    pub unsafe fn expose_raw_for_serialization(&self) -> &str {
        &self.0
    }

    /// Attempt to construct an [`ApiKey`] from a raw string.
    ///
    /// # Errors
    ///
    /// Returns [`ApiKeyError::Empty`] for empty/whitespace-only input,
    /// [`ApiKeyError::NonAscii`] for non-ASCII input, and
    /// [`ApiKeyError::ControlChar`] for input containing any ASCII control
    /// character (covers `\r`, `\n`, `\0`, tabs, …).
    pub fn try_from_string(raw: String) -> Result<Self, ApiKeyError> {
        if raw.trim().is_empty() {
            return Err(ApiKeyError::Empty);
        }
        if raw.len() > MAX_API_KEY_LEN {
            return Err(ApiKeyError::TooLong {
                actual: raw.len(),
                max: MAX_API_KEY_LEN,
            });
        }
        if !raw.is_ascii() {
            return Err(ApiKeyError::NonAscii);
        }
        for c in raw.chars() {
            if c.is_ascii_control() {
                return Err(ApiKeyError::ControlChar {
                    codepoint: c as u32,
                });
            }
        }
        Ok(Self(raw))
    }

    /// Borrow the raw key value for use in HTTP header construction.
    ///
    /// This is the ONE audit point for unredacted access — every other path
    /// (Debug, Display, Serialize) redacts.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Produce a log-safe fingerprint of an API key.
#[must_use]
pub fn redact_api_key(raw: &str) -> String {
    let len = raw.len();
    if len < 10 {
        return "<redacted>".to_string();
    }
    let head: String = raw.chars().take(4).collect();
    let tail: String = raw
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// Validate that an API-key string is structurally safe to hand to an HTTP
/// header builder.
///
/// # Errors
/// Returns a human-readable message when the key fails any of the checks.
pub fn validate_api_key(raw: &str) -> Result<(), String> {
    match ApiKey::try_from_string(raw.to_string()) {
        Ok(_) => Ok(()),
        Err(ApiKeyError::Empty) => Err("API key is empty or whitespace-only".to_string()),
        Err(ApiKeyError::NonAscii) => {
            Err("API key contains non-ASCII bytes (would fail header construction)".to_string())
        }
        Err(ApiKeyError::ControlChar { codepoint }) => Err(format!(
            "API key contains control character U+{codepoint:04X} (CRLF injection guard)"
        )),
        Err(ApiKeyError::TooLong { actual, max }) => Err(format!(
            "API key is {actual} bytes, exceeding the {max}-byte cap"
        )),
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApiKey({})", redact_api_key(&self.0))
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_api_key(&self.0))
    }
}

impl Serialize for ApiKey {
    /// Serialize as the redaction placeholder so the raw secret never
    /// lands in JSON / YAML / debug dumps via `serde_json::to_string`,
    /// `tracing` field formatters, or any other `Serialize`-driven sink.
    ///
    /// Genuine round-trip persistence (rare; the only legitimate site is
    /// the proxy's own config rewriter) must reach for
    /// [`ApiKey::expose_raw_for_serialization`] explicitly so call sites
    /// are greppable. See crosslink #944.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(REDACTED_PLACEHOLDER)
    }
}

/// The opaque marker emitted by `<ApiKey as Serialize>::serialize`.
///
/// Tests and downstream code can pattern-match on this constant when they
/// want to assert "the secret did NOT leak" without reasoning about the
/// redaction helper's truncation behaviour.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

impl<'de> Deserialize<'de> for ApiKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::try_from_string(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_format_redacts() {
        let key = ApiKey::try_from_string("sk-ant-api03-SECRET_VALUE_HERE_XYZ_TAIL".to_string())
            .expect("valid key");
        let debug = format!("{key:?}");
        assert!(
            !debug.contains("SECRET_VALUE_HERE"),
            "leaked middle: {debug}"
        );
        assert!(!debug.contains("api03-SECRET"), "leaked middle: {debug}");
        assert!(debug.contains("sk-a"), "missing head fingerprint: {debug}");
        assert!(
            debug.contains("TAIL") || debug.contains("…"),
            "no redaction marker: {debug}"
        );
    }

    #[test]
    fn display_format_redacts() {
        let key = ApiKey::try_from_string("sk-ant-api03-SECRET_VALUE_HERE_XYZ_TAIL".to_string())
            .expect("valid key");
        let shown = format!("{key}");
        assert!(
            !shown.contains("SECRET_VALUE_HERE"),
            "leaked middle: {shown}"
        );
        assert!(!shown.contains("VALUE_HERE"), "leaked middle: {shown}");
        assert!(shown.contains('…'), "no ellipsis: {shown}");
    }

    #[test]
    fn try_from_rejects_crlf() {
        let err =
            ApiKey::try_from_string("sk-legit\r\nX-Injected-Header: evil".to_string()).unwrap_err();
        assert!(matches!(err, ApiKeyError::ControlChar { codepoint: 0x0D }));
    }

    #[test]
    fn try_from_rejects_nul() {
        let err = ApiKey::try_from_string("sk-legit\0".to_string()).unwrap_err();
        assert!(matches!(err, ApiKeyError::ControlChar { codepoint: 0x00 }));
    }

    #[test]
    fn try_from_rejects_empty() {
        assert_eq!(
            ApiKey::try_from_string(String::new()).unwrap_err(),
            ApiKeyError::Empty
        );
        assert_eq!(
            ApiKey::try_from_string("   ".to_string()).unwrap_err(),
            ApiKeyError::Empty
        );
    }

    #[test]
    fn try_from_rejects_non_ascii() {
        let err = ApiKey::try_from_string("sk-legit-émoji-🔥".to_string()).unwrap_err();
        assert_eq!(err, ApiKeyError::NonAscii);
    }

    #[test]
    fn serialize_redacts_and_does_not_round_trip() {
        // crosslink #944: `Serialize` now emits the redaction placeholder so
        // the raw key never leaks through a generic `to_string`. This breaks
        // the previous serde round-trip on purpose — callers that need the
        // raw value MUST go through `expose_raw_for_serialization` and
        // construct the wire form themselves.
        let key = ApiKey::try_from_string("sk-ant-api03-SECRET_VALUE_HERE".to_string())
            .expect("valid key");
        let json = serde_json::to_string(&key).expect("serialize");
        assert!(
            !json.contains("SECRET_VALUE_HERE"),
            "Serialize must not leak the raw key: {json}"
        );
        assert_eq!(json, format!("\"{REDACTED_PLACEHOLDER}\""));
        // Deserializing the redacted placeholder back yields *that string*
        // (which still passes validation), NOT the original key — by design,
        // since reversing redaction would defeat the purpose.
        let back: ApiKey = serde_json::from_str(&json).expect("deserialize placeholder");
        assert_ne!(back, key, "round-trip must not recover the original key");
        // SAFETY: test-only inspection of the raw value to prove the
        // redaction placeholder was the actual round-trip value.
        unsafe {
            assert_eq!(back.expose_raw_for_serialization(), REDACTED_PLACEHOLDER);
        }
    }

    #[test]
    fn expose_raw_returns_unredacted_value() {
        let raw = "sk-ant-api03-EXPOSE-TEST";
        let key = ApiKey::try_from_string(raw.to_string()).expect("valid key");
        // SAFETY: test asserts the unsafe accessor returns the same bytes
        // that the safe `as_str` returns — no logging or persistence happens.
        unsafe {
            assert_eq!(key.expose_raw_for_serialization(), raw);
        }
        assert_eq!(key.as_str(), raw);
    }

    #[test]
    fn serde_rejects_bad_key() {
        let json = "\"sk-legit\\r\\nX-Injected: evil\"";
        let result: Result<ApiKey, _> = serde_json::from_str(json);
        assert!(result.is_err(), "expected deserialize error for CRLF key");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("control character") || msg.contains("U+000D"),
            "error message should explain CRLF rejection: {msg}"
        );
    }

    #[test]
    fn serde_yaml_rejects_bad_key() {
        let yaml = "\"sk-legit\\r\\nX-Injected: evil\"";
        let result: Result<ApiKey, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "expected YAML deserialize error for CRLF key"
        );
    }

    #[test]
    fn as_str_returns_raw() {
        let raw = "sk-ant-api03-XXXXXXXXXX";
        let key = ApiKey::try_from_string(raw.to_string()).expect("valid key");
        assert_eq!(key.as_str(), raw);
    }

    #[test]
    fn short_key_redacts_fully() {
        let key = ApiKey::try_from_string("sk-short1".to_string()).expect("valid key");
        let debug = format!("{key:?}");
        assert!(
            debug.contains("<redacted>"),
            "short key not fully redacted: {debug}"
        );
        assert!(!debug.contains("sk-short1"));
    }

    #[test]
    fn validate_free_function_matches_try_from() {
        assert!(validate_api_key("sk-ant-api03-valid").is_ok());
        assert!(validate_api_key("").is_err());
        assert!(validate_api_key("sk-legit\r\n").is_err());
    }

    // --- Regression tests for crosslink #452 ---

    #[test]
    fn try_from_rejects_over_max_length() {
        // Anything beyond MAX_API_KEY_LEN is an attack shape, not a key.
        let long = "a".repeat(MAX_API_KEY_LEN + 1);
        let err = ApiKey::try_from_string(long).unwrap_err();
        assert!(
            matches!(err, ApiKeyError::TooLong { actual, max }
                if actual == MAX_API_KEY_LEN + 1 && max == MAX_API_KEY_LEN),
            "expected TooLong, got {err:?}"
        );
    }

    #[test]
    fn try_from_accepts_exactly_max_length() {
        let at_cap = "a".repeat(MAX_API_KEY_LEN);
        assert!(ApiKey::try_from_string(at_cap).is_ok());
    }

    #[test]
    fn try_from_accepts_realistic_anthropic_key_length() {
        // Representative Anthropic-style key is ~108 chars — must pass.
        let key = format!("sk-ant-api03-{}", "X".repeat(96));
        assert!(ApiKey::try_from_string(key).is_ok());
    }
}
