//! End-to-end tests for `tools::remote_trigger::WebhookEndpoint`
//! shape — `url` + `headers` fields, `PartialEq`/`Eq` derive,
//! `Clone`, retrieval via `WebhookRegistry::get` + headers
//! propagation through register/replace.
//!
//! Sprint 216 of the verification effort.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::remote_trigger::{WebhookEndpoint, WebhookRegistry};
use std::collections::HashMap;

fn no_headers() -> HashMap<String, String> {
    HashMap::new()
}

fn one_header(k: &str, v: &str) -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert(k.to_string(), v.to_string());
    h
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — WebhookEndpoint Default construction shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn endpoint_constructible_with_explicit_fields() {
    let ep = WebhookEndpoint {
        url: "https://example.com/x".to_string(),
        headers: no_headers(),
    };
    assert_eq!(ep.url, "https://example.com/x");
    assert!(ep.headers.is_empty());
}

#[test]
fn endpoint_with_headers_preserves_kv_pairs() {
    let ep = WebhookEndpoint {
        url: "https://x.com/".to_string(),
        headers: one_header("Authorization", "Bearer xyz"),
    };
    assert_eq!(
        ep.headers.get("Authorization"),
        Some(&"Bearer xyz".to_string())
    );
    assert_eq!(ep.headers.len(), 1);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — register propagates fields into stored endpoint
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn register_propagates_url_into_endpoint() {
    let mut reg = WebhookRegistry::new();
    reg.register("notify", "https://example.com/hook", no_headers())
        .expect("register ok");
    let ep = reg.get("notify").expect("entry exists");
    assert_eq!(ep.url, "https://example.com/hook");
}

#[test]
fn register_propagates_headers_into_endpoint() {
    let mut reg = WebhookRegistry::new();
    let h = one_header("X-Secret", "value");
    reg.register("notify", "https://x.com/", h.clone())
        .expect("register ok");
    let ep = reg.get("notify").expect("entry exists");
    assert_eq!(ep.headers, h);
}

#[test]
fn register_with_multiple_headers_preserves_all() {
    let mut reg = WebhookRegistry::new();
    let mut h = HashMap::new();
    h.insert("X-A".to_string(), "1".to_string());
    h.insert("X-B".to_string(), "2".to_string());
    h.insert("X-C".to_string(), "3".to_string());
    reg.register("hook", "https://x.com/", h)
        .expect("register ok");
    let ep = reg.get("hook").expect("entry");
    assert_eq!(ep.headers.len(), 3);
    assert_eq!(ep.headers.get("X-A"), Some(&"1".to_string()));
    assert_eq!(ep.headers.get("X-B"), Some(&"2".to_string()));
    assert_eq!(ep.headers.get("X-C"), Some(&"3".to_string()));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — register URL upgrade semantics
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn register_scheme_less_input_upgraded_to_https() {
    // PINS DOC: scheme-less inputs get https:// prefix.
    let mut reg = WebhookRegistry::new();
    reg.register("notify", "example.com/hook", no_headers())
        .expect("register ok");
    let ep = reg.get("notify").expect("entry");
    assert!(
        ep.url.starts_with("https://"),
        "URL MUST be upgraded to https; got {:?}",
        ep.url
    );
}

#[test]
fn register_explicit_https_preserved() {
    let mut reg = WebhookRegistry::new();
    reg.register("notify", "https://api.example.com/v1/x", no_headers())
        .expect("register ok");
    let ep = reg.get("notify").expect("entry");
    assert!(ep.url.starts_with("https://"));
    assert!(ep.url.contains("api.example.com"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — replace overwrites entry
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn replace_overwrites_url_and_headers() {
    let mut reg = WebhookRegistry::new();
    reg.register("hook", "https://a.com/", one_header("X", "1"))
        .expect("register");
    reg.replace("hook", "https://b.com/", one_header("Y", "2"))
        .expect("replace ok");
    let ep = reg.get("hook").expect("entry");
    assert!(ep.url.contains("b.com"));
    assert!(ep.headers.contains_key("Y"));
    assert!(!ep.headers.contains_key("X"));
}

#[test]
fn replace_inserts_when_name_absent() {
    let mut reg = WebhookRegistry::new();
    // No prior register.
    reg.replace("new_hook", "https://x.com/", no_headers())
        .expect("replace inserts");
    assert!(reg.get("new_hook").is_some());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Endpoint PartialEq + Clone + Debug
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn endpoint_partial_eq_with_same_fields() {
    let a = WebhookEndpoint {
        url: "https://x.com/".to_string(),
        headers: one_header("X", "1"),
    };
    let b = WebhookEndpoint {
        url: "https://x.com/".to_string(),
        headers: one_header("X", "1"),
    };
    assert_eq!(a, b);
}

#[test]
fn endpoint_partial_eq_distinguishes_different_urls() {
    let a = WebhookEndpoint {
        url: "https://a.com/".to_string(),
        headers: no_headers(),
    };
    let b = WebhookEndpoint {
        url: "https://b.com/".to_string(),
        headers: no_headers(),
    };
    assert_ne!(a, b);
}

#[test]
fn endpoint_partial_eq_distinguishes_different_headers() {
    let a = WebhookEndpoint {
        url: "https://x.com/".to_string(),
        headers: one_header("X", "1"),
    };
    let b = WebhookEndpoint {
        url: "https://x.com/".to_string(),
        headers: one_header("X", "2"),
    };
    assert_ne!(a, b);
}

#[test]
fn endpoint_clone_preserves_url_and_headers() {
    let original = WebhookEndpoint {
        url: "https://marker.com/".to_string(),
        headers: one_header("X-Marker", "marker_216"),
    };
    let cloned = original.clone();
    assert_eq!(cloned, original);
    assert_eq!(cloned.url, "https://marker.com/");
    assert_eq!(
        cloned.headers.get("X-Marker"),
        Some(&"marker_216".to_string())
    );
}

#[test]
fn endpoint_debug_includes_url_field() {
    let ep = WebhookEndpoint {
        url: "https://debug.com/".to_string(),
        headers: no_headers(),
    };
    let d = format!("{ep:?}");
    assert!(d.contains("WebhookEndpoint"));
    assert!(d.contains("debug.com"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — get returns None for unknown
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn get_unknown_name_returns_none() {
    let reg = WebhookRegistry::new();
    assert!(reg.get("nonexistent_xyz").is_none());
}

#[test]
fn get_after_register_returns_some() {
    let mut reg = WebhookRegistry::new();
    reg.register("x", "https://x.com/", no_headers())
        .expect("ok");
    assert!(reg.get("x").is_some());
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Unicode + edge content
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn endpoint_with_unicode_header_value_preserved() {
    let mut reg = WebhookRegistry::new();
    reg.register("u", "https://x.com/", one_header("X-Note", "日本語の値"))
        .expect("ok");
    let ep = reg.get("u").expect("entry");
    assert_eq!(ep.headers.get("X-Note"), Some(&"日本語の値".to_string()));
}

#[test]
fn endpoint_with_empty_header_value_preserved() {
    let mut reg = WebhookRegistry::new();
    let h = one_header("X-Empty", "");
    reg.register("e", "https://x.com/", h).expect("ok");
    let ep = reg.get("e").expect("entry");
    assert_eq!(ep.headers.get("X-Empty"), Some(&String::new()));
}
