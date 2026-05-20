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
/// Returns `Ok(None)` when no candidate exists. A non-`NotFound` IO
/// error (permission denied, mid-read EIO, invalid UTF-8) is
/// propagated as `Err` rather than silently falling through to the
/// next candidate — silently loading the user-global MEMORY.md when
/// the project-local one was merely unreadable would inject the
/// wrong persona into the system prompt with no surfaced signal.
/// See crosslink #740.
///
/// # Errors
/// Returns an error when a candidate file exists but cannot be
/// read (e.g. EACCES, EIO, invalid UTF-8). `NotFound` is not an
/// error — it simply moves on to the next candidate.
pub fn load_entrypoint(cwd: &Path) -> anyhow::Result<Option<EntrypointFile>> {
    use anyhow::Context as _;

    let candidates = discovery_candidates(cwd);
    for path in candidates {
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let (content, truncation) = truncate_content(&raw);
                return Ok(Some(EntrypointFile {
                    path,
                    content,
                    truncation,
                }));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "MEMORY.md candidate {} exists but is unreadable",
                        path.display()
                    )
                });
            }
        }
    }
    Ok(None)
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
    let mut truncated = if line_count > MAX_ENTRYPOINT_LINES {
        lines_triggered = true;
        let kept: Vec<&str> = raw.lines().take(MAX_ENTRYPOINT_LINES).collect();
        kept.join("\n")
    } else {
        raw.to_string()
    };

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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        assert!(load_entrypoint(tmp.path()).unwrap().is_none());
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

        let loaded = load_entrypoint(tmp.path())
            .expect("io ok")
            .expect("root MEMORY.md hit");
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

        let loaded = load_entrypoint(tmp.path())
            .expect("io ok")
            .expect("subdir MEMORY.md hit");
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

        let loaded = load_entrypoint(tmp.path()).expect("io ok").expect("hit");
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

    // -----------------------------------------------------------------------
    // B3 — UTF-8 boundary tests: 2-byte sequences (spec §B3 "Missing tests")
    // -----------------------------------------------------------------------

    /// Pin B3: 2-byte codepoint (U+00E9, `é`) straddles the cut point.
    /// A naive slice at `max_bytes` would cut byte 1 of 2 and produce
    /// invalid UTF-8. `utf8_safe_truncate` must retreat to the char boundary.
    #[test]
    fn utf8_safe_truncate_two_byte_sequence_boundary() {
        // Build a string: 24_999 ASCII 'a' chars (1 byte each) followed by
        // `é` (U+00E9, 2 bytes). Total = 25_001 bytes.  A naive slice at
        // 25_000 would land between the two bytes of `é`.
        let mut s = "a".repeat(24_999);
        s.push('é'); // 2-byte codepoint at offset 24_999..25_001
        assert_eq!(s.len(), 25_001);

        let out = utf8_safe_truncate(&s, MAX_ENTRYPOINT_BYTES);

        // Result must be valid UTF-8 (implicit — it is a String).
        // Result must not exceed the cap.
        assert!(out.len() <= MAX_ENTRYPOINT_BYTES);
        // The multi-byte codepoint must be dropped whole (not split).
        assert!(!out.contains('é'));
        // All the ASCII prefix that fits must be present.
        assert_eq!(out.len(), 24_999);
    }

    /// Pin B3: truncate at exactly the byte after the FIRST byte of a
    /// 2-byte sequence — same retreat requirement, different alignment.
    #[test]
    fn utf8_safe_truncate_two_byte_sequence_internal_offset() {
        // Two `é` at positions 0..1 (bytes 0-1), then 2-3.
        // Ask to cut at byte 1 (inside the first codepoint).
        let s = "éé"; // 4 bytes total
        let out = utf8_safe_truncate(s, 1);
        // byte 1 is not a char boundary for `é`, so `end` retreats to 0.
        assert_eq!(out, "");
    }

    // -----------------------------------------------------------------------
    // B3 — UTF-8 boundary tests: 3-byte sequences (spec §B3 "Missing tests")
    // -----------------------------------------------------------------------

    /// Pin B3: 3-byte codepoint (U+3042, `あ`) straddles the cut point.
    /// `24_999` bytes of ASCII + one `あ` (3 bytes) = `25_002` bytes total.
    /// Naive slice at `25_000` lands byte 2 of 3; retreat must drop the whole
    /// codepoint.
    #[test]
    fn utf8_safe_truncate_three_byte_sequence_boundary() {
        let mut s = "a".repeat(24_999);
        s.push('あ'); // U+3042, 3 bytes at offset 24_999..25_002
        assert_eq!(s.len(), 25_002);

        let out = utf8_safe_truncate(&s, MAX_ENTRYPOINT_BYTES);

        assert!(out.len() <= MAX_ENTRYPOINT_BYTES);
        assert!(!out.contains('あ'));
        assert_eq!(out.len(), 24_999);
    }

    /// Pin B3: cut inside the SECOND byte of a 3-byte sequence.
    #[test]
    fn utf8_safe_truncate_three_byte_sequence_internal_offset() {
        // `あ` = 3 bytes (0xE3 0x81 0x82). Cut at byte 2 (between byte 1
        // and byte 2 of the codepoint) — must retreat to byte 0.
        let s = "あX"; // 4 bytes: [0xE3,0x81,0x82, 0x58]
        let out = utf8_safe_truncate(s, 2);
        assert_eq!(out, "");
    }

    // -----------------------------------------------------------------------
    // B2 — Divergence pin: CC trims before measuring; OC uses .lines() on
    // the raw string.  Whitespace-only files illustrate the gap.
    // -----------------------------------------------------------------------

    /// Pin B2 divergence (spec §B2 "OC gap"): a file consisting entirely of
    /// whitespace has a `.lines()` count of 0 in OC (Rust `.lines()` yields
    /// no items for all-whitespace input that has no `\n`, and for "\n" etc.
    /// it yields empty string slices, but the *count* stays 0 for pure spaces).
    /// After CC's `.trim()` the string is empty → 0 lines → no truncation.
    /// OC's result must also be no truncation.  Both agree here; the divergence
    /// is in the *caller* deciding whether to treat empty-content as absent.
    #[test]
    fn whitespace_only_file_no_truncation() {
        // OC's truncate_content on whitespace-only: lines().count() == 0,
        // byte count well under 25_000 → EntrypointTruncation::None.
        let raw = "   \n  \n\t\n   ";
        let (out, kind) = truncate_content(raw);
        assert_eq!(kind, EntrypointTruncation::None);
        // Content is returned verbatim (no suffix appended).
        assert_eq!(out, raw);
    }

    // -----------------------------------------------------------------------
    // B2 — Trailing-newline line count pin (spec §B2 edge cases)
    // -----------------------------------------------------------------------

    /// Pin B2: exactly 200 content lines followed by a single trailing `\n`.
    /// Rust `.lines()` does NOT yield a trailing empty string for a terminal
    /// newline, so `line_count` == 200 → no truncation (boundary is `> 200`).
    #[test]
    fn trailing_newline_at_exact_limit_is_not_truncated() {
        // 200 non-empty lines, then a trailing newline.
        let mut raw: String = (0..MAX_ENTRYPOINT_LINES)
            .map(|i| format!("- entry {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        raw.push('\n'); // trailing newline — Rust .lines() absorbs this

        let (_, kind) = truncate_content(&raw);
        // OC must NOT truncate: .lines() on "…\n" yields exactly 200 items.
        assert_eq!(kind, EntrypointTruncation::None);
    }

    /// Pin B2: 201 content lines, trailing newline.  Must trigger
    /// `EntrypointTruncation::Lines` and keep only the first 200.
    #[test]
    fn trailing_newline_one_over_limit_truncates() {
        let mut raw: String = (0..=MAX_ENTRYPOINT_LINES)
            .map(|i| format!("- entry {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        raw.push('\n');

        let (out, kind) = truncate_content(&raw);
        assert_eq!(kind, EntrypointTruncation::Lines);
        assert!(out.contains("truncated"));
        // Entry 200 (the 201st, 0-indexed) must be absent.
        assert!(!out.contains(&format!("- entry {MAX_ENTRYPOINT_LINES}")));
    }

    // -----------------------------------------------------------------------
    // B1 — HOME-unset: third candidate silently omitted (spec §B1)
    // -----------------------------------------------------------------------

    /// Pin B1: when HOME is unset, `discovery_candidates` returns only 2
    /// paths (cwd/MEMORY.md and cwd/.openclaudia/MEMORY.md); the function
    /// still succeeds if one of those exists.
    #[test]
    fn load_with_home_unset_still_finds_root_memory() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::remove_var("HOME");
        }

        std::fs::write(tmp.path().join("MEMORY.md"), "# home-unset test").unwrap();

        let loaded = load_entrypoint(tmp.path()).expect("io ok");
        // Must succeed even without HOME.
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().content.trim(), "# home-unset test");

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // B1 — Empty file returns Some (not None) (spec §B1 edge cases)
    // -----------------------------------------------------------------------

    /// Pin B1: an empty MEMORY.md returns `Some(EntrypointFile { content: "" })`
    /// — empty is NOT the same as missing.  Callers (not `load_entrypoint`) decide
    /// whether to treat empty content as absent (CC does via `.trim()` check).
    #[test]
    fn empty_file_returns_some_not_none() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        std::fs::write(tmp.path().join("MEMORY.md"), "").unwrap();

        let loaded = load_entrypoint(tmp.path()).expect("io ok");
        assert!(
            loaded.is_some(),
            "empty file must return Some, not None (B6 OC gap)"
        );
        assert_eq!(loaded.unwrap().content, "");

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // B1 — user-global fallback: ~/.openclaudia/MEMORY.md (spec §B1)
    // -----------------------------------------------------------------------

    /// Pin B1: when neither cwd/MEMORY.md nor cwd/.openclaudia/MEMORY.md exist,
    /// `load_entrypoint` falls through to HOME/.openclaudia/MEMORY.md.
    #[test]
    fn load_falls_back_to_home_openclaudia() {
        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        // Create only the home-level fallback, not cwd/* or cwd/.openclaudia/*.
        let home_oc = tmp.path().join(".openclaudia");
        std::fs::create_dir_all(&home_oc).unwrap();
        std::fs::write(home_oc.join("MEMORY.md"), "# global fallback").unwrap();

        // Use a separate cwd dir so it's clean.
        let cwd_tmp = TempDir::new().unwrap();
        let loaded = load_entrypoint(cwd_tmp.path()).expect("io ok");
        assert!(loaded.is_some(), "home-level fallback must be found");
        assert_eq!(loaded.unwrap().content.trim(), "# global fallback");

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // B3 — Newline-vs-char-boundary divergence pin (spec §B3 key divergence)
    // -----------------------------------------------------------------------

    /// Pin B3 divergence: OC uses char-boundary truncation (not last-`\n`
    /// truncation like CC). Construct a string where a `\n` falls BEFORE
    /// byte `25_000` but the char boundary falls AFTER that `\n`.  OC cuts at
    /// the char boundary (may include a partial line); CC cuts at the `\n`.
    /// This test pins OC's CURRENT behavior without asserting CC is wrong.
    #[test]
    fn byte_truncation_cuts_at_char_boundary_not_newline() {
        // 24_998 'a' chars (ASCII), then '\n' at byte 24_998, then 'あ'
        // (3 bytes: 24_999..25_002). Byte cap is 25_000.
        // - CC: lastIndexOf('\n', 25000) → 24_998 → cuts before `\n`.
        // - OC: char boundary walk from 25_000 → 24_999 is not a boundary
        //   (inside 'あ'), 24_998 is a boundary ('\n') → cuts at 24_998.
        // Both cut at 24_998 here (the `\n`). But the 3-byte char straddles
        // 24_999..25_001, so both agree on this exact placement.
        //
        // To SHOW the divergence we need the last `\n` strictly BEFORE the
        // last valid char boundary.  Use: (24_996 'a') + '\n' + 'a'
        // (24_998 bytes total up to here) + 'あ' (25_001 total).
        // Cap = 25_000: last `\n` at 24_997; char boundary walk from 25_000
        // retreats to 24_999 (inside 'あ') → 24_998 → also 'a' boundary.
        // So OC cuts at byte 24_998 (includes the 'a' after the `\n`);
        // CC cuts at byte 24_997 (before the trailing 'a').
        let mut s = "a".repeat(24_996);
        s.push('\n'); // byte 24_996
        s.push('a'); // byte 24_997
        s.push('あ'); // bytes 24_998..25_001
        assert_eq!(s.len(), 25_001);

        let out = utf8_safe_truncate(&s, MAX_ENTRYPOINT_BYTES);

        // OC must cut at a char boundary ≤ 25_000 and ≥ 0.
        assert!(out.len() <= MAX_ENTRYPOINT_BYTES);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        // OC includes the 'a' at byte 24_997 (char boundary at 24_998).
        // The CJK char 'あ' must be absent.
        assert!(!out.contains('あ'));
        // OC cuts at byte 24_998: the single 'a' after the newline IS present.
        // (CC would cut at 24_997 and exclude it.)  Pin OC's behavior:
        assert_eq!(out.len(), 24_998, "OC cuts at char boundary 24_998");
    }

    // -----------------------------------------------------------------------
    // #740 — non-NotFound IO errors must propagate, not silently fall through.
    // -----------------------------------------------------------------------

    /// Pin #740: a cwd MEMORY.md that exists but is unreadable
    /// (chmod 000) must surface as `Err`, not silently load the
    /// user-global fallback. Skipped on Windows (no POSIX permission
    /// bits) and when the test runs as root (chmod 000 is a no-op).
    #[cfg(unix)]
    #[test]
    fn unreadable_cwd_file_returns_err_instead_of_falling_through() {
        use std::os::unix::fs::PermissionsExt as _;

        if nix_is_root() {
            // chmod 000 doesn't lock root out — skip rather than report a false negative.
            return;
        }

        let _lock = env_lock();
        let tmp = TempDir::new().unwrap();
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        // The cwd file is unreadable, but a readable home-level fallback
        // exists. The pre-#740 implementation would silently load the
        // home file. The fixed implementation must Err so the caller can
        // surface the permission problem.
        let cwd_dir = TempDir::new().unwrap();
        let cwd_file = cwd_dir.path().join("MEMORY.md");
        std::fs::write(&cwd_file, "# project-local").unwrap();
        std::fs::set_permissions(&cwd_file, std::fs::Permissions::from_mode(0o000)).unwrap();

        let home_oc = tmp.path().join(".openclaudia");
        std::fs::create_dir_all(&home_oc).unwrap();
        std::fs::write(home_oc.join("MEMORY.md"), "# wrong fallback").unwrap();

        let result = load_entrypoint(cwd_dir.path());

        // Restore perms so TempDir can clean up.
        let _ = std::fs::set_permissions(&cwd_file, std::fs::Permissions::from_mode(0o644));

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        let err = result.expect_err(
            "#740: unreadable cwd MEMORY.md must propagate, not silently load the home file",
        );
        let msg = format!("{err:#}");
        assert!(
            msg.contains("MEMORY.md candidate") && msg.contains("unreadable"),
            "#740: error message must name the unreadable candidate; got {msg}"
        );
    }

    /// True when the current process is running as uid 0 (root).
    /// `chmod 000` does not deny access to root, so the
    /// permission-denied test above is a no-op there and must be
    /// skipped to avoid a false negative.
    #[cfg(unix)]
    fn nix_is_root() -> bool {
        // SAFETY: `libc::geteuid` is a pure thread-safe syscall wrapper with no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
}
