//! `MEMORY.md` entrypoint — the user-facing project-memory file that
//! gets injected into the system prompt.
//!
//! Port of Claude Code's `memdir/memdir.ts` entrypoint logic
//! (constants.ts: `MAX_ENTRYPOINT_LINES = 200`,
//! `MAX_ENTRYPOINT_BYTES = 25_000`; `truncateEntrypointContent`). The
//! load order here prefers `<cwd>/MEMORY.md` so repos shared with
//! Claude Code colleagues see the same file; falls through to
//! `<cwd>/.openclaudia/MEMORY.md` and then `~/.openclaudia/MEMORY.md`
//! for harness-specific entries.
//!
//! Truncation rules match Claude Code exactly:
//! 1. If the raw content is within BOTH limits, keep it as-is.
//! 2. Else truncate to `MAX_ENTRYPOINT_LINES` first (preserves
//!    whole entries rather than cutting mid-line).
//! 3. If the line-truncated content still exceeds
//!    `MAX_ENTRYPOINT_BYTES`, byte-truncate (UTF-8 safe).
//! 4. Append a one-line suffix noting truncation happened so the
//!    model knows some entries aren't visible.

use std::path::{Path, PathBuf};

/// Max rendered lines before truncation kicks in. Matches Claude
/// Code's `MAX_ENTRYPOINT_LINES` so MEMORY.md files shared with a
/// CC-using teammate render identically.
pub const MAX_ENTRYPOINT_LINES: usize = 200;

/// Max rendered bytes before truncation kicks in. Matches Claude
/// Code's `MAX_ENTRYPOINT_BYTES`. Applied AFTER line truncation —
/// the byte limit only trims when the line-truncated text still
/// exceeds it.
pub const MAX_ENTRYPOINT_BYTES: usize = 25_000;

/// The loaded entrypoint file. `content` is already truncated (if
/// truncation happened); the `truncation` field tells callers
/// whether the raw file was larger so they can surface a hint.
#[derive(Debug, Clone)]
pub struct EntrypointFile {
    /// Absolute path the content was read from.
    pub path: PathBuf,
    /// Content after truncation. May equal the raw file content.
    pub content: String,
    /// Which limits (if any) trimmed the content.
    pub truncation: EntrypointTruncation,
}

/// Which limits trimmed the content during load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrypointTruncation {
    /// Raw content fit within both limits.
    None,
    /// Line count exceeded `MAX_ENTRYPOINT_LINES` and was trimmed;
    /// byte count stayed within `MAX_ENTRYPOINT_BYTES` after line
    /// truncation.
    Lines,
    /// Byte count exceeded `MAX_ENTRYPOINT_BYTES` after line
    /// truncation — content was further byte-trimmed.
    Bytes,
    /// Both limits triggered (common case for a very long file).
    LinesAndBytes,
}

impl EntrypointFile {
    /// True when the caller should surface a "memory was truncated"
    /// hint to the user / agent.
    #[must_use]
    pub const fn was_truncated(&self) -> bool {
        !matches!(self.truncation, EntrypointTruncation::None)
    }
}

/// Discover + load a MEMORY.md entrypoint for the project rooted at
/// `cwd`. Search order (first hit wins):
///
/// 1. `<cwd>/MEMORY.md` — shared with Claude Code users.
/// 2. `<cwd>/.openclaudia/MEMORY.md` — OC-specific file in-repo.
/// 3. `<home>/.openclaudia/MEMORY.md` — user-global fallback.
///
/// Returns `None` when no candidate exists. Read / decode errors
/// log at `warn` and fall through to the next candidate — one
/// unreadable file doesn't silently swallow a valid one.
#[must_use]
pub fn load_entrypoint(cwd: &Path) -> Option<EntrypointFile> {
    let candidates = discovery_candidates(cwd);
    for path in candidates {
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let (content, truncation) = truncate_content(&raw);
                return Some(EntrypointFile {
                    path,
                    content,
                    truncation,
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "could not read MEMORY.md candidate — trying next"
                );
                continue;
            }
        }
    }
    None
}

/// Build the absolute paths tried by [`load_entrypoint`], in order.
/// Exposed for tests that need to assert search precedence without
/// actually touching the filesystem.
fn discovery_candidates(cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(3);
    out.push(cwd.join("MEMORY.md"));
    out.push(cwd.join(".openclaudia").join("MEMORY.md"));
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".openclaudia").join("MEMORY.md"));
    }
    out
}

/// Apply both truncation rules to `raw`, in line-then-bytes order.
/// Returns `(truncated_content, which_rules_fired)`.
///
/// Public-within-crate so the Phase 2 session-notes writer can
/// reuse the byte-truncation helper without re-implementing the
/// UTF-8-safe slice.
pub(crate) fn truncate_content(raw: &str) -> (String, EntrypointTruncation) {
    let line_count = raw.lines().count();
    let byte_count = raw.len();

    if line_count <= MAX_ENTRYPOINT_LINES && byte_count <= MAX_ENTRYPOINT_BYTES {
        return (raw.to_string(), EntrypointTruncation::None);
    }

    let mut lines_triggered = false;
    let mut bytes_triggered = false;
    let mut truncated = raw.to_string();

    if line_count > MAX_ENTRYPOINT_LINES {
        lines_triggered = true;
        let kept: Vec<&str> = raw.lines().take(MAX_ENTRYPOINT_LINES).collect();
        truncated = kept.join("\n");
    }

    if truncated.len() > MAX_ENTRYPOINT_BYTES {
        bytes_triggered = true;
        truncated = utf8_safe_truncate(&truncated, MAX_ENTRYPOINT_BYTES);
    }

    // Claude Code appends a one-line suffix so the model knows some
    // entries aren't visible. Keep the text short + distinctive so
    // the model can grep for it in its own context.
    truncated.push_str("\n\n[truncated — MEMORY.md exceeded the display limits]");

    let which = match (lines_triggered, bytes_triggered) {
        (true, true) => EntrypointTruncation::LinesAndBytes,
        (true, false) => EntrypointTruncation::Lines,
        (false, true) => EntrypointTruncation::Bytes,
        // Unreachable: we only enter this block when at least one
        // limit was exceeded. Defensive match for readability.
        (false, false) => EntrypointTruncation::None,
    };
    (truncated, which)
}

/// Truncate `s` to at most `max_bytes` while keeping valid UTF-8.
/// Walks backward from `max_bytes` to the nearest char boundary so
/// we never split a multi-byte codepoint. Returns an owned String
/// rather than borrowing so callers can append.
fn utf8_safe_truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// The load_* tests flip the shared `HOME` env var to point the
    /// user-global fallback away from the developer's real home dir.
    /// Without a lock, cargo's parallel runner races between tests
    /// that each want a different HOME.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn short_content_is_not_truncated() {
        let raw = "# Project memory\n\nJust a few bullets.";
        let (out, kind) = truncate_content(raw);
        assert_eq!(out, raw);
        assert_eq!(kind, EntrypointTruncation::None);
    }

    #[test]
    fn exactly_at_line_limit_is_not_truncated() {
        let raw: String = (0..MAX_ENTRYPOINT_LINES)
            .map(|i| format!("- entry {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, kind) = truncate_content(&raw);
        assert_eq!(kind, EntrypointTruncation::None);
    }

    #[test]
    fn over_line_limit_keeps_first_n_lines() {
        // 400 short lines → 400 > 200, well under 25 KB.
        let raw: String = (0..400)
            .map(|i| format!("- e{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, kind) = truncate_content(&raw);
        assert_eq!(kind, EntrypointTruncation::Lines);
        // First line preserved, last line gone.
        assert!(out.contains("- e0\n"));
        assert!(!out.contains("- e399"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn over_byte_limit_is_utf8_safe() {
        // A single line over the byte limit → line truncation
        // doesn't help; byte truncation must not split a multi-byte
        // UTF-8 codepoint. Build a line made of 4-byte emoji so the
        // byte limit sits on a boundary we'd split naively.
        let big_line = "\u{1F600}".repeat(MAX_ENTRYPOINT_BYTES); // 4 × 25k ≈ 100k bytes
        let (out, kind) = truncate_content(&big_line);
        // Line count == 1 < MAX_ENTRYPOINT_LINES, so only bytes fire.
        assert_eq!(kind, EntrypointTruncation::Bytes);
        // Body (before suffix) must be valid UTF-8 and ≤ cap.
        let body_end = out.find("\n\n[truncated").unwrap_or(out.len());
        let body = &out[..body_end];
        assert!(body.len() <= MAX_ENTRYPOINT_BYTES);
        // Round-trip via String ensures no mid-codepoint split.
        assert!(std::str::from_utf8(body.as_bytes()).is_ok());
    }

    #[test]
    fn both_limits_trigger() {
        // Line truncation drops us to MAX_ENTRYPOINT_LINES (200)
        // lines, so each kept line must itself be long enough that
        // 200 × line_len still exceeds MAX_ENTRYPOINT_BYTES (25 000).
        // 200 × 200 = 40 000 > 25 000 — safely over.
        let raw: String = (0..400)
            .map(|i| format!("{i}: {}", "x".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, kind) = truncate_content(&raw);
        assert_eq!(kind, EntrypointTruncation::LinesAndBytes);
    }

    #[test]
    fn load_returns_none_when_no_candidate_exists() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        // Override HOME so the user-global fallback path doesn't
        // accidentally match a real file on the test machine.
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        assert!(load_entrypoint(tmp.path()).is_none());
        // Restore.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn load_prefers_root_memory_over_openclaudia_subdir() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        // Both files exist — root MEMORY.md must win.
        std::fs::write(tmp.path().join("MEMORY.md"), "# root").unwrap();
        std::fs::create_dir_all(tmp.path().join(".openclaudia")).unwrap();
        std::fs::write(tmp.path().join(".openclaudia/MEMORY.md"), "# openclaudia").unwrap();

        let loaded = load_entrypoint(tmp.path()).expect("root MEMORY.md hit");
        assert_eq!(loaded.content.trim(), "# root");
        assert!(loaded.path.ends_with("MEMORY.md"));
        assert!(!loaded.path.to_string_lossy().contains(".openclaudia"));

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn load_falls_back_to_openclaudia_subdir() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        std::fs::create_dir_all(tmp.path().join(".openclaudia")).unwrap();
        std::fs::write(tmp.path().join(".openclaudia/MEMORY.md"), "# from subdir").unwrap();

        let loaded = load_entrypoint(tmp.path()).expect("subdir MEMORY.md hit");
        assert_eq!(loaded.content.trim(), "# from subdir");
        assert!(loaded.path.to_string_lossy().contains(".openclaudia"));

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn load_truncates_oversized_file() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        let raw: String = (0..400)
            .map(|i| format!("- line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(tmp.path().join("MEMORY.md"), &raw).unwrap();

        let loaded = load_entrypoint(tmp.path()).expect("hit");
        assert!(loaded.was_truncated());
        assert_eq!(loaded.truncation, EntrypointTruncation::Lines);
        assert!(loaded.content.contains("truncated"));

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn utf8_safe_truncate_never_splits_codepoints() {
        // "Hello" in Japanese — each char is 3 bytes.
        let s = "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}"; // こんにちは, 15 bytes
                                                            // Ask to truncate at a byte offset that falls mid-codepoint (7).
        let out = utf8_safe_truncate(s, 7);
        // Must decode cleanly — if we split a codepoint this would
        // return a FromUtf8Error on the String conversion above.
        assert!(out.len() <= 7);
        // And the truncated content must be a prefix of the original.
        assert!(s.starts_with(&out));
    }
}
