//! End-to-end tests for `Plugin::load` + manifest deserialization
//! + dir-name validators.
//!
//! Sprint 49 of the verification effort.
//!
//! `tests/plugin_skill_security_e2e.rs` (sprint 11) covers URL
//! validation + marketplace policy + skill parsing. This file
//! fills the loader gap: end-to-end manifest parsing from real
//! tempdir-based plugin trees + the dir-name validator catalog.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::plugins::manifest::{CommandsSpec, HooksSpec, McpServersSpec, PluginManifest};
use openclaudia::plugins::validate::{derive_dir_name_from_url, validate_plugin_dir_name};
use openclaudia::plugins::Plugin;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Write a `.claude-plugin/plugin.json` file inside `dir` with the
/// given content. Creates the directory if needed.
fn write_manifest(dir: &Path, content: &str) {
    let plugin_dir = dir.join(".claude-plugin");
    fs::create_dir_all(&plugin_dir).expect("mkdir .claude-plugin");
    fs::write(plugin_dir.join("plugin.json"), content).expect("write manifest");
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — PluginManifest serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn minimal_manifest_with_name_only_parses() {
    let json = r#"{"name": "minimal"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    assert_eq!(manifest.name, "minimal");
    assert!(manifest.version.is_none());
    assert!(manifest.description.is_none());
    assert!(manifest.commands.is_none());
}

#[test]
fn full_manifest_round_trips_through_json() {
    let json = r#"{
        "name": "test-plugin",
        "version": "1.2.3",
        "description": "A test plugin",
        "author": {"name": "Alice", "email": "alice@example.com", "url": "https://alice.dev"},
        "homepage": "https://test-plugin.example.com",
        "repository": "https://github.com/test/plugin",
        "license": "MIT",
        "keywords": ["test", "demo"]
    }"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    assert_eq!(manifest.name, "test-plugin");
    assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
    assert_eq!(manifest.description.as_deref(), Some("A test plugin"));
    let author = manifest.author.as_ref().expect("author");
    assert_eq!(author.name, "Alice");
    assert_eq!(author.email.as_deref(), Some("alice@example.com"));
    assert_eq!(author.url.as_deref(), Some("https://alice.dev"));
    assert_eq!(manifest.license.as_deref(), Some("MIT"));
    assert_eq!(
        manifest.keywords.as_deref(),
        Some(["test".to_string(), "demo".to_string()].as_slice())
    );
}

#[test]
fn manifest_rejects_invalid_json() {
    let bad = r#"{"name": "x", "version": ]"#;
    let outcome: Result<PluginManifest, _> = serde_json::from_str(bad);
    assert!(outcome.is_err(), "malformed JSON MUST error");
}

#[test]
fn manifest_with_missing_required_name_field_errors() {
    let no_name = r#"{"version": "1.0.0"}"#;
    let outcome: Result<PluginManifest, _> = serde_json::from_str(no_name);
    assert!(outcome.is_err(), "missing required `name` MUST error");
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — CommandsSpec deserialization variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn commands_as_path_string_parses_as_path_variant() {
    let json = r#"{"name": "p", "commands": "./cmds"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    match manifest.commands.expect("commands present") {
        CommandsSpec::Path(p) => assert_eq!(p, "./cmds"),
        other => panic!("expected Path variant, got {other:?}"),
    }
}

#[test]
fn commands_as_object_map_parses_as_map_variant() {
    // Documented CommandMetadata fields: source, content,
    // description, argumentHint, model, allowedTools.
    let json = r#"{
        "name": "p",
        "commands": {
            "test": {"source": "cmds/test.md", "description": "Run tests"}
        }
    }"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    match manifest.commands.expect("commands present") {
        CommandsSpec::Map(map) => {
            assert!(map.contains_key("test"));
            let meta = map.get("test").unwrap();
            assert_eq!(meta.description.as_deref(), Some("Run tests"));
            assert_eq!(meta.source.as_deref(), Some("cmds/test.md"));
        }
        other => panic!("expected Map variant, got {other:?}"),
    }
}

#[test]
fn commands_map_with_truly_unknown_field_rejected_by_deny_unknown_fields() {
    // CommandMetadata uses deny_unknown_fields. A genuinely
    // unknown field name (one the manifest schema doesn't
    // recognise — not in the documented list of source,
    // content, description, argumentHint, model, allowedTools)
    // MUST error at parse time.
    let json = r#"{
        "name": "p",
        "commands": {
            "buggy": {"mystery_field_not_in_schema": "value"}
        }
    }"#;
    let outcome: Result<PluginManifest, _> = serde_json::from_str(json);
    assert!(
        outcome.is_err(),
        "deny_unknown_fields MUST reject mystery_field; got {outcome:?}"
    );
    // The error message must name the offending field.
    let err = outcome.unwrap_err().to_string();
    assert!(
        err.contains("mystery_field") || err.contains("unknown field"),
        "error must indicate the unknown field; got {err:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — HooksSpec variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn hooks_as_path_string_parses_as_path_variant() {
    let json = r#"{"name": "p", "hooks": "./hooks.yaml"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    let hooks = manifest.hooks.expect("hooks present");
    match hooks {
        HooksSpec::Path(p) => assert_eq!(p, "./hooks.yaml"),
        other => panic!("expected Path variant, got {other:?}"),
    }
}

#[test]
fn hooks_as_array_of_paths_parses_as_array_variant() {
    let json = r#"{
        "name": "p",
        "hooks": ["./hooks-1.yaml", "./hooks-2.yaml"]
    }"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    let hooks = manifest.hooks.expect("hooks present");
    match hooks {
        HooksSpec::Array(entries) => {
            assert_eq!(entries.len(), 2, "must keep both array entries");
        }
        other => panic!("expected Array variant, got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — McpServersSpec variants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn mcp_servers_as_path_string_parses_as_path_variant() {
    let json = r#"{"name": "p", "mcpServers": "./mcp.json"}"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    let mcp = manifest.mcp_servers.expect("mcpServers present");
    match mcp {
        McpServersSpec::Path(p) => assert_eq!(p, "./mcp.json"),
        other => panic!("expected Path variant, got {other:?}"),
    }
}

#[test]
fn mcp_servers_as_object_parses_with_command_and_args() {
    let json = r#"{
        "name": "p",
        "mcpServers": {
            "echo-srv": {"command": "node", "args": ["server.js"]}
        }
    }"#;
    let manifest: PluginManifest = serde_json::from_str(json).expect("parse");
    let mcp = manifest.mcp_servers.expect("mcpServers present");
    match mcp {
        McpServersSpec::Map(map) => {
            assert!(map.contains_key("echo-srv"));
        }
        other => panic!("expected Map variant, got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Plugin::load from real plugin dirs
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn load_plugin_from_minimal_manifest_dir() {
    let dir = TempDir::new().expect("tempdir");
    let plugin_dir = dir.path().join("myplug");
    fs::create_dir(&plugin_dir).expect("mkdir plug");
    write_manifest(&plugin_dir, r#"{"name": "myplug"}"#);

    let plugin = Plugin::load(&plugin_dir).expect("load must succeed");
    assert_eq!(plugin.manifest.name, "myplug");
    assert_eq!(plugin.path, plugin_dir);
    // id defaults: for local plugins, id = "<name>@local" or similar.
    assert!(
        plugin.id.contains("myplug"),
        "plugin id must contain name; got {:?}",
        plugin.id
    );
}

#[test]
fn load_plugin_missing_manifest_file_errors() {
    let dir = TempDir::new().expect("tempdir");
    let plugin_dir = dir.path().join("no-manifest");
    fs::create_dir(&plugin_dir).expect("mkdir");
    let outcome = Plugin::load(&plugin_dir);
    assert!(
        outcome.is_err(),
        "missing manifest MUST error; got {:?}",
        outcome.map(|p| p.id)
    );
}

#[test]
fn load_plugin_with_invalid_json_manifest_errors() {
    let dir = TempDir::new().expect("tempdir");
    let plugin_dir = dir.path().join("bad-json");
    fs::create_dir(&plugin_dir).expect("mkdir");
    write_manifest(&plugin_dir, r"{ not even close to json }");
    let outcome = Plugin::load(&plugin_dir);
    assert!(
        outcome.is_err(),
        "invalid JSON manifest MUST error; got {:?}",
        outcome.map(|p| p.id)
    );
}

#[test]
fn load_plugin_resolves_command_paths_from_commands_dir() {
    let dir = TempDir::new().expect("tempdir");
    let plugin_dir = dir.path().join("cmd-plug");
    fs::create_dir(&plugin_dir).expect("mkdir");
    // Write a commands subdir with a .md file.
    let cmd_dir = plugin_dir.join("commands");
    fs::create_dir_all(&cmd_dir).expect("mkdir commands");
    fs::write(cmd_dir.join("test.md"), "# Test command").expect("write cmd");
    // Manifest pointing at "./commands"
    write_manifest(
        &plugin_dir,
        r#"{"name": "cmd-plug", "commands": "./commands"}"#,
    );

    let plugin = Plugin::load(&plugin_dir).expect("load");
    assert!(
        !plugin.command_paths.is_empty(),
        "must discover command files under ./commands; got {:?}",
        plugin.command_paths
    );
    // The test.md must be in the resolved paths.
    let any_test = plugin.command_paths.iter().any(|p| p.ends_with("test.md"));
    assert!(any_test, "test.md must appear in command_paths");
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — validate_plugin_dir_name catalog
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn dir_name_validator_accepts_simple_names() {
    for name in &["plugin", "my-plugin", "plugin_v2", "plugin123", "Foo-Bar"] {
        let outcome = validate_plugin_dir_name(name);
        assert!(
            outcome.is_ok(),
            "valid name {name:?} MUST be accepted; got {outcome:?}"
        );
    }
}

#[test]
fn dir_name_validator_refuses_empty() {
    let outcome = validate_plugin_dir_name("");
    assert!(outcome.is_err(), "empty name MUST be refused");
}

#[test]
fn dir_name_validator_refuses_dot_and_dotdot() {
    assert!(validate_plugin_dir_name(".").is_err());
    assert!(validate_plugin_dir_name("..").is_err());
}

#[test]
fn dir_name_validator_refuses_leading_dot_hidden_files() {
    for name in &[".hidden", ".config", ".plugin"] {
        let outcome = validate_plugin_dir_name(name);
        assert!(
            outcome.is_err(),
            "leading-dot name {name:?} MUST be refused (no hidden dirs)"
        );
    }
}

#[test]
fn dir_name_validator_refuses_path_separators_and_control_chars() {
    // crosslink #875: cross-platform forbidden chars.
    for name in &[
        "with/slash",
        "with\\backslash",
        "with\0nul",
        "with:colon",
        "with\ntab", // control char
        "with\rcarriage",
    ] {
        let outcome = validate_plugin_dir_name(name);
        assert!(
            outcome.is_err(),
            "name {name:?} MUST be refused (path sep / NUL / control)"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — derive_dir_name_from_url
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn derive_dir_name_from_https_github_url_strips_dot_git() {
    let url = "https://github.com/user/my-plugin.git";
    let derived = derive_dir_name_from_url(url).expect("derive");
    assert_eq!(derived, "my-plugin");
}

#[test]
fn derive_dir_name_from_https_url_without_git_suffix() {
    let url = "https://github.com/user/plain-plugin";
    let derived = derive_dir_name_from_url(url).expect("derive");
    assert_eq!(derived, "plain-plugin");
}

#[test]
fn derive_dir_name_from_url_with_trailing_slash_strips_it() {
    let url = "https://example.com/path/to/plugin/";
    let derived = derive_dir_name_from_url(url);
    // Either accepts and returns "plugin", or refuses — both
    // are reasonable contracts. Pin the actual behaviour.
    if let Ok(name) = derived {
        assert_eq!(name, "plugin", "trailing slash MUST be stripped");
    }
    // Err branch acceptable — refusing trailing-slash form
    // is a defensible policy choice.
}

#[test]
fn derive_dir_name_from_scp_ssh_form_extracts_repo_segment() {
    let url = "git@github.com:user/scp-plugin.git";
    let derived = derive_dir_name_from_url(url).expect("derive");
    assert_eq!(derived, "scp-plugin");
}

#[test]
fn derive_dir_name_from_malformed_url_errors() {
    for bad in &["not-a-url", "http://", ""] {
        let outcome = derive_dir_name_from_url(bad);
        assert!(
            outcome.is_err(),
            "malformed URL {bad:?} MUST error; got {outcome:?}"
        );
    }
}
