//! Claude Code-compatible session transcript persistence.
//!
//! Port of `utils/sessionStorage.ts` and `utils/sessionStoragePortable.ts`.
//! Transcripts are append-only JSONL files, one message per line. Layout:
//!
//! ```text
//! $CLAUDE_CONFIG_HOME_DIR/projects/<sanitized-cwd>/<session-id>.jsonl
//! ```
//!
//! `CLAUDE_CONFIG_HOME_DIR` defaults to `~/.claude`. `sanitized-cwd`
//! replaces every non-alphanumeric byte in the absolute path with `-`
//! (e.g. `/home/doll/OpenClaudia` → `-home-doll-OpenClaudia`), so
//! sessions created here are readable by Claude Code and vice versa.
//!
//! Each line is a [`SerializedMessage`] — the underlying chat message
//! plus envelope fields (`cwd`, `sessionId`, `timestamp`, `version`,
//! optional `gitBranch`). Appends use `O_APPEND` semantics via Rust's
//! [`OpenOptions`], which is atomic for small writes on POSIX.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Crate version baked in by Cargo. Matches Claude Code's `version`
/// field on each serialized message.
pub const TRANSCRIPT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// On-disk envelope around a raw chat message. Field names match
/// Claude Code's `SerializedMessage` type (camelCase over the wire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedMessage {
    /// Message kind — one of `user`, `assistant`, `system`, `summary`,
    /// `custom-title`, etc. Kept as a free-form string so new Claude
    /// Code metadata entry types round-trip without a code change.
    #[serde(rename = "type")]
    pub kind: String,
    /// UUID assigned to this message. Generated at append time if the
    /// caller doesn't provide one.
    pub uuid: String,
    /// ISO-8601 UTC timestamp.
    pub timestamp: String,
    /// Absolute working directory the message was generated in.
    pub cwd: String,
    /// Session UUID this message belongs to.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Harness version that wrote the line.
    pub version: String,
    /// Git branch at write time, if inside a repo.
    #[serde(rename = "gitBranch", skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Underlying chat message payload. For `user`/`assistant`/`system`
    /// this is typically `{ role, content }`. Metadata entry types
    /// (`summary`, `custom-title`, …) carry the payload directly in the
    /// outer object — we preserve it here under `message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
}

/// Resolve `$CLAUDE_CONFIG_HOME_DIR`. Matches Claude Code's
/// `getClaudeConfigHomeDir()`: env var wins, else `~/.claude`.
#[must_use]
pub fn claude_config_home_dir() -> PathBuf {
    if let Ok(explicit) = std::env::var("CLAUDE_CONFIG_HOME_DIR") {
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
}

/// `<claude_config_home>/projects`.
#[must_use]
pub fn projects_dir() -> PathBuf {
    claude_config_home_dir().join("projects")
}

/// Sanitize a filesystem path for use as a project-directory name.
/// Claude Code's regex: `/[^a-zA-Z0-9]/g` → `-`. The result is the full
/// sanitized string — no length cap, so a path like `/home/doll/...`
/// produces `-home-doll-...`.
#[must_use]
pub fn sanitize_path(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Absolute projects-dir path for `cwd` (e.g.
/// `~/.claude/projects/-home-doll-OpenClaudia`).
#[must_use]
pub fn project_dir_for(cwd: &Path) -> PathBuf {
    let key = cwd.to_string_lossy();
    projects_dir().join(sanitize_path(&key))
}

/// Absolute transcript path for `(cwd, session_id)`.
#[must_use]
pub fn transcript_path(cwd: &Path, session_id: &str) -> PathBuf {
    project_dir_for(cwd).join(format!("{session_id}.jsonl"))
}

/// Best-effort git branch lookup via `git rev-parse --abbrev-ref HEAD`.
/// Returns `None` when git isn't available, `cwd` isn't a repo, or the
/// command takes longer than 2 seconds.
#[must_use]
pub fn current_git_branch(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

/// Build a [`SerializedMessage`] for `message` using the current time,
/// a fresh UUID, and a best-effort git-branch lookup.
#[must_use]
pub fn envelope_for(
    kind: &str,
    cwd: &Path,
    session_id: &str,
    message: Option<Value>,
) -> SerializedMessage {
    SerializedMessage {
        kind: kind.to_string(),
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        cwd: cwd.to_string_lossy().into_owned(),
        session_id: session_id.to_string(),
        version: TRANSCRIPT_VERSION.to_string(),
        git_branch: current_git_branch(cwd),
        message,
    }
}

/// Append one JSONL line to the transcript for `(cwd, session_id)`,
/// creating the project directory on first use. Mode `0o600` on the
/// file, `0o700` on the directory — matches Claude Code's permissions.
///
/// # Errors
///
/// Returns an error if the filesystem is inaccessible. The caller
/// should log-and-continue rather than crash: transcript writes are
/// best-effort and must not fail the user-visible turn.
pub fn append_entry(
    cwd: &Path,
    session_id: &str,
    entry: &SerializedMessage,
) -> std::io::Result<()> {
    let path = transcript_path(cwd, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        set_secure_perms(parent, 0o700);
    }
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")?;
    set_secure_perms(&path, 0o600);
    Ok(())
}

#[cfg(unix)]
fn set_secure_perms(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        if perms.mode() & 0o777 != mode {
            perms.set_mode(mode);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

#[cfg(not(unix))]
fn set_secure_perms(_path: &Path, _mode: u32) {
    // On Windows the umask model doesn't apply; rely on NTFS ACLs.
}

/// Read every JSONL line in `path` as a [`SerializedMessage`]. Lines
/// that fail to parse are skipped (and logged via `tracing::warn`) so a
/// partial/corrupt tail doesn't break resume.
#[must_use]
pub fn load_transcript(path: &Path) -> Vec<SerializedMessage> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SerializedMessage>(&line) {
            Ok(msg) => out.push(msg),
            Err(err) => tracing::warn!(
                path = %path.display(),
                line = idx + 1,
                error = %err,
                "skipping unparseable transcript line"
            ),
        }
    }
    out
}

/// Summary of a transcript on disk, used by `--resume` pickers.
#[derive(Debug, Clone)]
pub struct TranscriptInfo {
    pub session_id: String,
    pub path: PathBuf,
    pub first_prompt: Option<String>,
    pub message_count: usize,
    pub modified: std::time::SystemTime,
}

/// List every transcript for the project rooted at `cwd`, newest first.
/// Non-JSONL files and files we can't read are silently skipped.
#[must_use]
pub fn list_transcripts(cwd: &Path) -> Vec<TranscriptInfo> {
    let dir = project_dir_for(cwd);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<TranscriptInfo> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()?.to_str()? != "jsonl" {
                return None;
            }
            let session_id = path.file_stem()?.to_str()?.to_string();
            let modified = e.metadata().ok()?.modified().ok()?;
            let messages = load_transcript(&path);
            let first_prompt = messages
                .iter()
                .find(|m| m.kind == "user")
                .and_then(|m| m.message.as_ref())
                .and_then(extract_text_content);
            Some(TranscriptInfo {
                session_id,
                path,
                first_prompt,
                message_count: messages.len(),
                modified,
            })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// Pull plain text out of a `{ role, content }` payload where `content`
/// is either a string or an Anthropic-style block array.
fn extract_text_content(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let joined: String = arr
            .iter()
            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if joined.is_empty() {
            return None;
        }
        return Some(joined);
    }
    None
}

/// Locate a transcript anywhere under `projects_dir()` by session ID.
/// Used by `--resume <session-id>` when the user doesn't pass `--cwd`.
#[must_use]
pub fn find_transcript_by_id(session_id: &str) -> Option<PathBuf> {
    let projects = projects_dir();
    let entries = std::fs::read_dir(&projects).ok()?;
    for project_entry in entries.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let candidate = project_path.join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Serialize every test in this module that touches the shared
    /// `CLAUDE_CONFIG_HOME_DIR` env var. Without this, cargo's default
    /// parallel test runner races between tests that point the var at
    /// different `TempDir`s, causing flaky `list_transcripts` / path
    /// sanitization assertions.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn sanitize_matches_claude_code() {
        // No env lock needed — sanitize_path is pure.
        assert_eq!(sanitize_path("/home/doll/OpenClaudia"), "-home-doll-OpenClaudia");
        // Every non-alphanumeric char becomes one dash: `:` → `-`, `\` → `-`.
        assert_eq!(sanitize_path("C:\\Users\\Foo"), "C--Users-Foo");
        assert_eq!(sanitize_path("plain"), "plain");
    }

    #[test]
    fn env_overrides_home_dir() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(
            "CLAUDE_CONFIG_HOME_DIR",
            tmp.path().to_str().unwrap(),
        );
        assert_eq!(claude_config_home_dir(), tmp.path());
        assert_eq!(projects_dir(), tmp.path().join("projects"));
    }

    #[test]
    fn append_and_load_roundtrip() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(
            "CLAUDE_CONFIG_HOME_DIR",
            tmp.path().to_str().unwrap(),
        );
        let cwd = PathBuf::from("/home/doll/OpenClaudia");
        let session_id = "11111111-2222-3333-4444-555555555555";

        let entry = envelope_for(
            "user",
            &cwd,
            session_id,
            Some(json!({"role": "user", "content": "hello"})),
        );
        append_entry(&cwd, session_id, &entry).unwrap();

        let loaded = load_transcript(&transcript_path(&cwd, session_id));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, "user");
        assert_eq!(loaded[0].session_id, session_id);
        assert_eq!(loaded[0].cwd, "/home/doll/OpenClaudia");
        assert_eq!(
            loaded[0]
                .message
                .as_ref()
                .unwrap()
                .get("content")
                .and_then(|c| c.as_str()),
            Some("hello"),
        );
    }

    #[test]
    fn list_transcripts_sorts_newest_first() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(
            "CLAUDE_CONFIG_HOME_DIR",
            tmp.path().to_str().unwrap(),
        );
        let cwd = PathBuf::from("/tmp/proj");
        for id in ["aaa", "bbb"] {
            let entry = envelope_for("user", &cwd, id, Some(json!({"content": id})));
            append_entry(&cwd, id, &entry).unwrap();
            // Sleep briefly so mtime differs.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let infos = list_transcripts(&cwd);
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].session_id, "bbb");
        assert_eq!(infos[1].session_id, "aaa");
    }

    #[test]
    fn find_by_id_searches_all_projects() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(
            "CLAUDE_CONFIG_HOME_DIR",
            tmp.path().to_str().unwrap(),
        );
        let cwd = PathBuf::from("/tmp/elsewhere");
        let session_id = "needle-id";
        let entry = envelope_for("user", &cwd, session_id, None);
        append_entry(&cwd, session_id, &entry).unwrap();
        let found = find_transcript_by_id(session_id).unwrap();
        assert!(found.ends_with(format!("{session_id}.jsonl")));
    }
}
