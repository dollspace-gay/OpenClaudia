//! End-to-end tests for `config::ProxyConfig` field
//! defaults: port=8080, host=127.0.0.1, target=anthropic,
//! `max_response_bytes`=50 MiB. Plus YAML round-trip across
//! partial-field configs (each `#[serde(default = "...")]`
//! producer reachable from YAML omission).
//!
//! Sprint 175 of the verification effort. Sprint 49 covered
//! `AppConfig` YAML round-trip; this file pins each
//! `ProxyConfig` default and exposes the documented bind
//! semantics (loopback-only by default).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::config::ProxyConfig;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Default values for each field
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn default_port_is_8080() {
    // PINS DEFAULT: HTTP proxy listens on 8080 when no
    // override is given.
    let cfg = ProxyConfig::default();
    assert_eq!(cfg.port, 8080);
}

#[test]
fn default_host_is_loopback_127_0_0_1() {
    // PINS SECURITY: default bind is loopback-only.
    // External access requires explicit override.
    let cfg = ProxyConfig::default();
    assert_eq!(cfg.host, "127.0.0.1");
}

#[test]
fn default_target_is_anthropic() {
    let cfg = ProxyConfig::default();
    assert_eq!(cfg.target, "anthropic");
}

#[test]
fn default_max_response_bytes_is_50_mebibytes() {
    // PINS GUARD: 50 MiB cap against upstream DoS.
    let cfg = ProxyConfig::default();
    assert_eq!(cfg.max_response_bytes, 50 * 1024 * 1024);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — YAML deserialize defaults each field independently
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn empty_yaml_yields_all_defaults() {
    let cfg: ProxyConfig = serde_yaml::from_str("{}").expect("ok");
    assert_eq!(cfg.port, 8080);
    assert_eq!(cfg.host, "127.0.0.1");
    assert_eq!(cfg.target, "anthropic");
    assert_eq!(cfg.max_response_bytes, 50 * 1024 * 1024);
}

#[test]
fn yaml_port_override_preserves_other_defaults() {
    let cfg: ProxyConfig = serde_yaml::from_str("port: 9090").expect("ok");
    assert_eq!(cfg.port, 9090);
    // Other defaults stay.
    assert_eq!(cfg.host, "127.0.0.1");
    assert_eq!(cfg.target, "anthropic");
}

#[test]
fn yaml_host_override_preserves_other_defaults() {
    let cfg: ProxyConfig = serde_yaml::from_str("host: 0.0.0.0").expect("ok");
    assert_eq!(cfg.host, "0.0.0.0");
    assert_eq!(cfg.port, 8080);
    assert_eq!(cfg.target, "anthropic");
}

#[test]
fn yaml_target_override_preserves_other_defaults() {
    let cfg: ProxyConfig = serde_yaml::from_str("target: openai").expect("ok");
    assert_eq!(cfg.target, "openai");
    assert_eq!(cfg.port, 8080);
    assert_eq!(cfg.host, "127.0.0.1");
}

#[test]
fn yaml_max_response_bytes_override_preserves_other_defaults() {
    let cfg: ProxyConfig = serde_yaml::from_str("max_response_bytes: 1048576").expect("ok");
    assert_eq!(cfg.max_response_bytes, 1_048_576);
    assert_eq!(cfg.port, 8080);
}

#[test]
fn yaml_full_override_all_fields() {
    let yaml = "
port: 4242
host: ::1
target: google
max_response_bytes: 1024
";
    let cfg: ProxyConfig = serde_yaml::from_str(yaml).expect("ok");
    assert_eq!(cfg.port, 4242);
    assert_eq!(cfg.host, "::1");
    assert_eq!(cfg.target, "google");
    assert_eq!(cfg.max_response_bytes, 1024);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Type coercion + bounds
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn yaml_port_max_u16_accepted() {
    let cfg: ProxyConfig = serde_yaml::from_str("port: 65535").expect("ok");
    assert_eq!(cfg.port, 65535);
}

#[test]
fn yaml_port_zero_accepted_at_serde_layer() {
    // serde doesn't reject port 0; that's an OS-bind concern.
    let cfg: ProxyConfig = serde_yaml::from_str("port: 0").expect("ok");
    assert_eq!(cfg.port, 0);
}

#[test]
fn yaml_port_above_u16_max_rejected() {
    // 65536 doesn't fit u16.
    let outcome: Result<ProxyConfig, _> = serde_yaml::from_str("port: 65536");
    assert!(outcome.is_err(), "port out of u16 range MUST be rejected");
}

#[test]
fn yaml_port_negative_rejected() {
    let outcome: Result<ProxyConfig, _> = serde_yaml::from_str("port: -1");
    assert!(outcome.is_err());
}

#[test]
fn yaml_port_as_string_rejected() {
    let outcome: Result<ProxyConfig, _> = serde_yaml::from_str(r#"port: "8080""#);
    assert!(
        outcome.is_err(),
        "port as string MUST be rejected (strict typing)"
    );
}

#[test]
fn yaml_host_can_be_ipv6() {
    let cfg: ProxyConfig = serde_yaml::from_str("host: \"::1\"").expect("ok");
    assert_eq!(cfg.host, "::1");
}

#[test]
fn yaml_host_can_be_dns_name() {
    let cfg: ProxyConfig = serde_yaml::from_str("host: myhost.local").expect("ok");
    assert_eq!(cfg.host, "myhost.local");
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Clone preserves all fields
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn clone_preserves_all_4_fields() {
    let original = ProxyConfig {
        port: 1234,
        host: "192.168.1.1".to_string(),
        target: "qwen".to_string(),
        max_response_bytes: 999,
    };
    let cloned = original.clone();
    assert_eq!(cloned.port, original.port);
    assert_eq!(cloned.host, original.host);
    assert_eq!(cloned.target, original.target);
    assert_eq!(cloned.max_response_bytes, original.max_response_bytes);
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Unknown YAML fields are silently ignored (no #[serde(deny_unknown_fields)])
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_yaml_field_is_ignored_not_rejected() {
    // PINS FORWARD-COMPAT: serde defaults to "ignore unknown".
    let yaml = "port: 8080\nfuture_field: 42";
    let cfg: ProxyConfig = serde_yaml::from_str(yaml).expect("unknown field MUST NOT reject");
    assert_eq!(cfg.port, 8080);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Two-deserialize idempotency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn deserialize_same_yaml_twice_yields_identical_configs() {
    let yaml = "port: 4242\nhost: 0.0.0.0";
    let cfg1: ProxyConfig = serde_yaml::from_str(yaml).expect("ok");
    let cfg2: ProxyConfig = serde_yaml::from_str(yaml).expect("ok");
    assert_eq!(cfg1.port, cfg2.port);
    assert_eq!(cfg1.host, cfg2.host);
    assert_eq!(cfg1.target, cfg2.target);
}
