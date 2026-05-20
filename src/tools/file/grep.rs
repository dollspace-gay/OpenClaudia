//! Native grep tool — crosslink #568.
//!
//! Parity target: CC `tools/GrepTool/GrepTool.ts`. The CC tool is backed by
//! ripgrep and supports a rich flag set (`-A` / `-B` / `-C`, file-type
//! filters, glob filters, output modes). This is a minimal parity
//! implementation: regex search across files under `path` with optional
//! ±N context lines.
//!
//! Args
//! ────
//! * `pattern` (required): regex string. Compiled with the `regex` crate.
//! * `path` (optional, default `.`): root directory to search.
//! * `context_lines` (optional, default `0`): integer ±N context window.
//! * `case_insensitive` (optional, default `false`): toggles `(?i)`.
//!
//! Output is `file:line:match` for every match, with optional
//! `file:line-context` lines for the surrounding window. Capped at
//! `MAX_MATCHES` so a runaway regex does not flood the agent.

use super::resolve_path;
use crate::tools::args::ToolArgs as _;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Hard cap on returned matches before truncation.
const MAX_MATCHES: usize = 200;

/// Hard cap on bytes read per file — keeps the grep tool from blocking
/// on a multi-GB log file.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Hard cap on directories descended, mirroring the glob walker.
const MAX_WALK_ENTRIES: usize = 50_000;

/// Vendor / generated directories the walker skips by default.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    ".svelte-kit",
    ".next",
    "dist",
    "build",
];

/// Execute the `grep` tool.
///
/// Returns `(stdout, is_error)`.
pub fn execute_grep(args: &HashMap<String, Value>) -> (String, bool) {
    let pattern = match args.arg_str("pattern") {
        Ok(p) => p.to_string(),
        Err(e) => return (e.to_string(), true),
    };

    let raw_path = args.arg_str_or("path", ".");
    let root = match resolve_path(raw_path) {
        Ok(p) => p,
        Err(e) => return (e, true),
    };

    let context: usize = usize::try_from(args.arg_u64_or("context_lines", 0)).unwrap_or(0);
    let case_insensitive = args.arg_bool_or("case_insensitive", false);

    let effective_pattern = if case_insensitive {
        format!("(?i){pattern}")
    } else {
        pattern.clone()
    };

    let regex = match Regex::new(&effective_pattern) {
        Ok(r) => r,
        Err(e) => return (format!("Invalid regex '{pattern}': {e}"), true),
    };

    // Collect every regular file under `root`, then grep each.
    let mut files: Vec<PathBuf> = Vec::new();
    let mut visited: usize = 0;
    collect_files(&root, &mut files, &mut visited);

    let mut output: Vec<String> = Vec::new();
    let mut total_matches: usize = 0;
    let mut truncated = false;

    for file in &files {
        if total_matches >= MAX_MATCHES {
            truncated = true;
            break;
        }
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        match grep_one(file, &regex, context) {
            Ok(hits) => {
                for hit in hits {
                    if total_matches >= MAX_MATCHES {
                        truncated = true;
                        break;
                    }
                    for ctx_line in hit.context_before {
                        output.push(format!("{rel}-{}-{ctx_line}", hit.line_no - 1));
                    }
                    output.push(format!("{rel}:{}:{}", hit.line_no, hit.line));
                    let mut after_no = hit.line_no;
                    for ctx_line in hit.context_after {
                        after_no += 1;
                        output.push(format!("{rel}-{after_no}-{ctx_line}"));
                    }
                    if context > 0 {
                        output.push("--".to_string());
                    }
                    total_matches += 1;
                }
            }
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "grep: file read failed");
            }
        }
    }

    let header = if truncated {
        format!(
            "Found {} matches (truncated at {MAX_MATCHES}) across {} files:",
            total_matches,
            files.len()
        )
    } else {
        format!(
            "Found {} matches across {} files:",
            total_matches,
            files.len()
        )
    };
    let body = output.join("\n");
    let out = if body.is_empty() {
        header
    } else {
        format!("{header}\n{body}")
    };
    (out, false)
}

struct Hit {
    line_no: usize,
    line: String,
    context_before: Vec<String>,
    context_after: Vec<String>,
}

fn grep_one(path: &Path, regex: &Regex, context: usize) -> std::io::Result<Vec<Hit>> {
    let meta = fs::metadata(path)?;
    if meta.len() > MAX_FILE_BYTES {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)?;
    let lines: Vec<&str> = body.lines().collect();
    let mut hits = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if regex.is_match(line) {
            let line_no = idx + 1;
            let before_start = idx.saturating_sub(context);
            let after_end = (idx + context + 1).min(lines.len());
            let context_before: Vec<String> = lines[before_start..idx]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            let context_after: Vec<String> = lines[idx + 1..after_end]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            hits.push(Hit {
                line_no,
                line: (*line).to_string(),
                context_before,
                context_after,
            });
        }
    }
    Ok(hits)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, visited: &mut usize) {
    if *visited >= MAX_WALK_ENTRIES {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "grep: skipping unreadable dir");
            return;
        }
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        *visited += 1;
        if *visited >= MAX_WALK_ENTRIES {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if file_type.is_dir() {
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            subdirs.push(entry.path());
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    for sub in subdirs {
        collect_files(&sub, out, visited);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::File;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn write_file(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        File::create(&p)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
    }

    /// Pin: matches the requested regex and returns `file:line:match`
    /// for every hit, ignoring files that don't contain the pattern.
    #[test]
    fn grep_emits_file_line_match() {
        let dir = TempDir::new().unwrap();
        write_file(
            dir.path(),
            "a.rs",
            "fn main() {\n    println!(\"hello world\");\n}\n",
        );
        write_file(dir.path(), "b.rs", "fn other() {}\n");
        let mut args = HashMap::new();
        args.insert("pattern".to_string(), json!("hello"));
        args.insert(
            "path".to_string(),
            json!(dir.path().to_string_lossy().to_string()),
        );
        let (out, err) = execute_grep(&args);
        assert!(!err, "got error: {out}");
        assert!(out.contains("a.rs:2:"), "expected a.rs:2: in: {out}");
        assert!(out.contains("hello world"), "match body lost: {out}");
        assert!(!out.contains("b.rs:"), "b.rs leaked: {out}");
    }

    /// Pin: `context_lines` = 1 emits the surrounding line on each side.
    #[test]
    fn grep_emits_context_lines() {
        let dir = TempDir::new().unwrap();
        write_file(
            dir.path(),
            "ctx.txt",
            "line one\nline two MATCH here\nline three\n",
        );
        let mut args = HashMap::new();
        args.insert("pattern".to_string(), json!("MATCH"));
        args.insert(
            "path".to_string(),
            json!(dir.path().to_string_lossy().to_string()),
        );
        args.insert("context_lines".to_string(), json!(1));
        let (out, err) = execute_grep(&args);
        assert!(!err, "got error: {out}");
        // Before-context emitted with `-1-` delimiter style
        assert!(out.contains("ctx.txt-1-line one"), "before-ctx: {out}");
        assert!(out.contains("ctx.txt:2:"), "match line: {out}");
        assert!(out.contains("ctx.txt-3-line three"), "after-ctx: {out}");
    }

    /// Pin: an invalid regex surfaces a clean error instead of panicking.
    #[test]
    fn grep_invalid_regex_errors() {
        let dir = TempDir::new().unwrap();
        let mut args = HashMap::new();
        args.insert("pattern".to_string(), json!("[unterminated"));
        args.insert(
            "path".to_string(),
            json!(dir.path().to_string_lossy().to_string()),
        );
        let (out, err) = execute_grep(&args);
        assert!(err, "expected error, got: {out}");
        assert!(out.contains("Invalid regex"), "msg was: {out}");
    }

    /// Pin: missing pattern is a clean error.
    #[test]
    fn grep_missing_pattern_errors() {
        let args = HashMap::new();
        let (out, err) = execute_grep(&args);
        assert!(err, "missing pattern must be an error: {out}");
        assert!(out.contains("pattern"), "error must name the arg: {out}");
    }

    /// Pin: `case_insensitive=true` matches mixed-case occurrences.
    #[test]
    fn grep_case_insensitive_flag() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "x.txt", "HELLO world\n");
        let mut args = HashMap::new();
        args.insert("pattern".to_string(), json!("hello"));
        args.insert(
            "path".to_string(),
            json!(dir.path().to_string_lossy().to_string()),
        );
        args.insert("case_insensitive".to_string(), json!(true));
        let (out, err) = execute_grep(&args);
        assert!(!err);
        assert!(out.contains("x.txt:1:"), "case-insensitive miss: {out}");
    }
}
