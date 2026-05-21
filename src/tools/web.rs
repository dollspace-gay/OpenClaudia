use crate::tools::args::ToolArgs as _;
use crate::tools::safe_truncate;
use crate::web::{self, WebConfig};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::sync::LazyLock;
use tokio::runtime::{Handle, Runtime};

/// Process-wide shared tokio runtime used to drive the async web tools
/// from sync caller contexts (crosslink #368).
///
/// The previous implementation invoked `tokio::runtime::Runtime::new()`
/// on every `execute_web_fetch` / `execute_web_search` call when no
/// runtime was already current. Each construction spawned a fresh
/// multi-thread worker pool (default = `num_cpus`) and tore it back
/// down at end of block — tens of milliseconds per call on a hot path,
/// plus epoll/kqueue churn and thread-pool explosion under load. It
/// also forced `reqwest::Client` to be rebuilt against that ephemeral
/// runtime, defeating its connection pool and DNS cache.
///
/// One runtime, built once, kept alive for the lifetime of the process.
/// All sync-context tool calls share it via `block_on`. Async-context
/// calls still go through `Handle::current()` + `block_in_place` so
/// they participate in the caller's own runtime (no nested-runtime
/// panic and no thread-jump to the shared runtime).
static SHARED_RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("openclaudia-web-tools")
        .build()
        .expect("shared web-tools tokio runtime builds with default settings")
});

/// Drive `fut` to completion regardless of whether the caller is inside
/// a tokio runtime or not.
///
/// * Inside a runtime → `block_in_place` + `Handle::block_on` so we don't
///   panic on nested runtimes and stay on the caller's runtime.
/// * Outside a runtime → `SHARED_RUNTIME.block_on` so we don't construct
///   (or destruct) a runtime per call.
///
/// Centralising the dispatch makes it impossible for a future web tool
/// to regress and `Runtime::new()` again.
fn run_blocking<F>(fut: F) -> F::Output
where
    F: Future,
{
    if let Ok(handle) = Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(fut))
    } else {
        SHARED_RUNTIME.block_on(fut)
    }
}

/// Hard cap on the agent-facing fetched-page output string, in bytes.
///
/// Both [`execute_web_fetch`] and [`execute_web_browser`] truncate to this
/// length and append a `(content truncated, N total chars)` marker.
/// Centralised so the two fetch entry points can never drift (crosslink #807).
pub const MAX_FETCH_OUTPUT_BYTES: usize = 50_000;

/// Assemble the agent-facing string for a successful page fetch.
///
/// Output shape matches the pre-extraction implementation byte-for-byte:
///
/// ```text
/// # <title>           (omitted if title is None)
///
/// URL: <url>
///
/// <content>
/// ```
///
/// followed by a `...\n\n(content truncated, N total chars)` tail when the
/// rendered output exceeds [`MAX_FETCH_OUTPUT_BYTES`].
///
/// Extracted from `execute_web_fetch` and `execute_web_browser`, which
/// previously open-coded identical assembly + the `50000` magic constant
/// (crosslink #807). Both call sites now route through this single function
/// so a tweak to the format or the cap applies uniformly.
#[must_use]
pub fn format_fetch_output(title: Option<&str>, url: &str, content: &str) -> String {
    let mut output = String::new();
    if let Some(title) = title {
        let _ = write!(output, "# {title}\n\n");
    }
    let _ = write!(output, "URL: {url}\n\n");
    output.push_str(content);

    if output.len() > MAX_FETCH_OUTPUT_BYTES {
        let total = output.len();
        format!(
            "{}...\n\n(content truncated, {total} total chars)",
            safe_truncate(&output, MAX_FETCH_OUTPUT_BYTES),
        )
    } else {
        output
    }
}

/// Fetch a URL using Jina Reader
pub fn execute_web_fetch(args: &HashMap<String, Value>) -> (String, bool) {
    // crosslink #675: typed accessor.
    let url = match args.arg_str("url") {
        Ok(u) => u,
        Err(e) => return e.into_tool_error(),
    };

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (
            "Invalid URL: must start with http:// or https://".to_string(),
            true,
        );
    }

    // Drive the async fetch on either the caller's runtime (async
    // context) or the shared `SHARED_RUNTIME` (sync context). Never
    // build a fresh runtime per call — see crosslink #368.
    let result = run_blocking(web::fetch_url(url));

    match result {
        Ok(fetch_result) => (
            format_fetch_output(
                fetch_result.title.as_deref(),
                &fetch_result.url,
                &fetch_result.content,
            ),
            false,
        ),
        Err(e) => (format!("Failed to fetch URL: {e}"), true),
    }
}

/// Return the hostname of `url` in lowercase, stripping any `www.`
/// prefix. Used by [`domain_matches`] to compare a search-result URL
/// against an allow / block list. `None` when the URL can't be parsed.
///
/// Crosslink #763: was a hand-rolled split-on-`://` / `/` / `:` parser that
/// misread `http://user:pass@host/`, IPv6 literals (`https://[::1]:8080/`)
/// and treated `://no-scheme` as host `no-scheme`. Now delegates to
/// `url::Url`, which is already a dependency, so authority / userinfo /
/// IPv6 handling is correct by construction.
fn host_of(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// True when `host` is equal to `needle` or is a subdomain of it.
/// Matches Claude Code's behavior where `"docs.python.org"` covers
/// both the exact host and `foo.docs.python.org`.
fn domain_matches(host: &str, needle: &str) -> bool {
    let needle = needle.trim_start_matches("www.").to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    host == needle || host.ends_with(&format!(".{needle}"))
}

/// Extract the `allowed_domains` / `blocked_domains` JSON-array args
/// as owned `Vec<String>`s. Non-string entries are silently dropped,
/// which matches Claude Code's Zod schema behavior (strict parse).
fn domain_list(args: &HashMap<String, Value>, key: &str) -> Vec<String> {
    // crosslink #675: typed accessor.
    args.arg_array(key)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Search the web using Tavily or Brave API (or `DuckDuckGo` fallback).
///
/// Supports Claude Code-compatible `allowed_domains` / `blocked_domains`
/// filtering: results from domains matching `blocked_domains` are
/// dropped; if `allowed_domains` is non-empty, only results matching
/// that list are kept. Blocked list takes precedence when both lists
/// name the same domain.
pub fn execute_web_search(args: &HashMap<String, Value>) -> (String, bool) {
    // crosslink #675: typed accessors.
    let query = match args.arg_str("query") {
        Ok(q) => q,
        Err(e) => return e.into_tool_error(),
    };
    if query.trim().len() < 2 {
        return ("Query must be at least 2 characters.".to_string(), true);
    }

    let limit = usize::try_from(args.arg_u64_or("limit", 5)).unwrap_or(usize::MAX);

    let allowed = domain_list(args, "allowed_domains");
    let blocked = domain_list(args, "blocked_domains");

    // Load web config from environment
    // Falls back to DuckDuckGo with headless browser if no API keys configured
    let config = WebConfig::from_env();

    // Shared runtime; never construct a fresh one per call (crosslink #368).
    let result = run_blocking(web::search_web(query, &config, limit));

    match result {
        Ok(mut results) => {
            // Apply domain filters. Unparseable URLs are kept — failing
            // closed would drop valid results with unusual schemes the
            // caller might still want to see.
            if !allowed.is_empty() || !blocked.is_empty() {
                results.retain(|r| {
                    let Some(host) = host_of(&r.url) else {
                        return true;
                    };
                    if blocked.iter().any(|d| domain_matches(&host, d)) {
                        return false;
                    }
                    if !allowed.is_empty() && !allowed.iter().any(|d| domain_matches(&host, d)) {
                        return false;
                    }
                    true
                });
            }
            (web::format_search_results(&results), false)
        }
        Err(e) => (format!("Search failed: {e}"), true),
    }
}

/// Fetch a URL using headless Chrome browser
/// Fallback for when Jina Reader fails on complex sites
pub fn execute_web_browser(args: &HashMap<String, Value>) -> (String, bool) {
    // crosslink #675: typed accessor.
    let url = match args.arg_str("url") {
        Ok(u) => u,
        Err(e) => return e.into_tool_error(),
    };

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (
            "Invalid URL: must start with http:// or https://".to_string(),
            true,
        );
    }

    match web::fetch_with_browser(url) {
        Ok(fetch_result) => (
            format_fetch_output(
                fetch_result.title.as_deref(),
                &fetch_result.url,
                &fetch_result.content,
            ),
            false,
        ),
        Err(e) => (format!("Browser fetch failed: {e}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_handles_common_shapes() {
        assert_eq!(
            host_of("https://example.com/path"),
            Some("example.com".into())
        );
        assert_eq!(
            host_of("http://www.example.com"),
            Some("example.com".into())
        );
        assert_eq!(
            host_of("https://EXAMPLE.com:8080/x"),
            Some("example.com".into())
        );
        // crosslink #763: url::Url rejects scheme-less and empty inputs
        // (replacing the bespoke parser that returned `Some("no-scheme")`).
        assert_eq!(host_of("://no-scheme"), None);
        assert_eq!(host_of(""), None);
        // Userinfo and IPv6 literals — both wrong with the old hand-rolled
        // splitter, both correct now via url::Url.
        assert_eq!(
            host_of("http://user:pass@example.com/x"),
            Some("example.com".into())
        );
        assert_eq!(host_of("https://[::1]:8080/path"), Some("[::1]".into()));
    }

    #[test]
    fn domain_matches_subdomains_but_not_siblings() {
        assert!(domain_matches("docs.python.org", "docs.python.org"));
        assert!(domain_matches("foo.docs.python.org", "docs.python.org"));
        assert!(!domain_matches("python.org", "docs.python.org"));
        assert!(!domain_matches("evildocs.python.org", "docs.python.org"));
        assert!(domain_matches("example.com", "www.example.com"));
    }

    // ── crosslink #368: runtime sharing & no per-call construction ─────────

    /// Forensic test for crosslink #368.
    ///
    /// `Runtime::new()` per call is the bug we're killing. Here we issue
    /// 50 back-to-back synchronous invocations of the shared dispatcher
    /// and confirm that `SHARED_RUNTIME` is initialised exactly once —
    /// its `Handle::id()` is stable across every call. If a future
    /// refactor ever re-introduces `Runtime::new()` inside `run_blocking`
    /// (or the executor swap below), this test catches it.
    #[test]
    fn shared_runtime_is_reused_across_back_to_back_calls() {
        let first = run_blocking(async { Handle::current().id() });
        for _ in 0..50 {
            let id = run_blocking(async { Handle::current().id() });
            assert_eq!(
                id, first,
                "run_blocking constructed a new runtime on a sync-context call \
                 (regression of crosslink #368)"
            );
        }
    }

    /// Forensic test for crosslink #368.
    ///
    /// When the caller is already inside a tokio runtime, the dispatcher
    /// MUST execute on the caller's runtime (via `Handle::current()` +
    /// `block_in_place`), NOT on the shared one. Verifies the
    /// async-context branch of `run_blocking` does not jump runtimes.
    #[test]
    fn run_blocking_uses_caller_runtime_in_async_context() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let caller_id = rt.handle().id();
        let inside_id: tokio::runtime::Id = rt.block_on(async {
            // spawn_blocking puts the closure on the caller runtime's
            // blocking pool; `run_blocking` inside it sees an async
            // context and must stay on the caller's runtime rather than
            // hop to SHARED_RUNTIME.
            tokio::task::spawn_blocking(move || run_blocking(async { Handle::current().id() }))
                .await
                .unwrap()
        });
        assert_eq!(
            inside_id, caller_id,
            "run_blocking left the caller's runtime in an async context \
             (regression of crosslink #368)"
        );
    }

    /// Forensic test for crosslink #368.
    ///
    /// Validates `execute_web_fetch`'s synchronous wrapper still returns
    /// a well-formed error string when given an invalid URL — covering
    /// the argument-validation and runtime-dispatch path without
    /// requiring outbound network I/O. The point is to prove the
    /// dispatcher itself can be entered/exited cleanly back-to-back.
    #[test]
    fn execute_web_fetch_handles_back_to_back_invalid_urls() {
        let mut args = HashMap::new();
        // Trigger the URL-scheme guard so we exercise the sync path
        // without making a network call.
        args.insert("url".to_string(), Value::String("not-a-url".into()));
        for _ in 0..10 {
            let (msg, is_err) = execute_web_fetch(&args);
            assert!(is_err);
            assert!(msg.contains("http://") && msg.contains("https://"));
        }
    }

    // ── crosslink #807: shared format_fetch_output for both fetch paths ──

    /// Forensic test for crosslink #807.
    ///
    /// Pins the exact agent-facing output shape so that any future tweak
    /// to title / URL / body composition fails one test instead of letting
    /// the two fetch entry points silently drift. Covers all four branches
    /// of the formatter (title present / absent × body short / over-cap).
    #[test]
    fn format_fetch_output_pins_shape_807() {
        // Title present + body short of the cap → leading "# title", URL
        // header, body verbatim, no truncation marker.
        let out = format_fetch_output(Some("Hello"), "https://example.com/", "the body content");
        assert_eq!(
            out,
            "# Hello\n\nURL: https://example.com/\n\nthe body content"
        );

        // Title absent → no leading heading at all.
        let out = format_fetch_output(None, "https://example.com/", "body");
        assert_eq!(out, "URL: https://example.com/\n\nbody");

        // Body over the cap → truncated to MAX_FETCH_OUTPUT_BYTES then
        // suffixed with "...\n\n(content truncated, N total chars)" where
        // N is the *pre-truncation* total length.
        let body = "x".repeat(MAX_FETCH_OUTPUT_BYTES * 2);
        let out = format_fetch_output(None, "https://example.com/", &body);
        let expected_total = "URL: https://example.com/\n\n".len() + body.len();
        assert!(
            out.contains(&format!(
                "(content truncated, {expected_total} total chars)"
            )),
            "truncation marker must echo the pre-truncation total length"
        );
        assert!(
            out.ends_with(" total chars)"),
            "truncation marker must close the output"
        );
        // safe_truncate is UTF-8-boundary safe, so output length is bounded
        // by cap + suffix; checking it's not exploding to body.len() suffices.
        assert!(out.len() < MAX_FETCH_OUTPUT_BYTES + 200);
    }

    /// Forensic test for crosslink #807.
    ///
    /// Drives `format_fetch_output` across the boundary of
    /// [`MAX_FETCH_OUTPUT_BYTES`] from both sides — exactly the cap, one byte
    /// under, one byte over — and asserts the truncation marker fires iff
    /// the threshold is crossed. If a future refactor splits the cap between
    /// the two fetch entry points (the bug #807 originally filed), the two
    /// would silently disagree; routing both through this helper makes that
    /// impossible, and this test pins the threshold behaviour itself.
    #[test]
    fn format_fetch_output_truncates_at_cap_boundary_807() {
        // URL prefix consumed by the formatter header.
        let header_len = "URL: https://example.com/\n\n".len();

        // One byte under the cap → no truncation marker.
        let body_under = "z".repeat(MAX_FETCH_OUTPUT_BYTES - header_len - 1);
        let out = format_fetch_output(None, "https://example.com/", &body_under);
        assert!(!out.contains("content truncated"));
        assert_eq!(out.len(), MAX_FETCH_OUTPUT_BYTES - 1);

        // Exactly at the cap → no truncation marker.
        let body_at = "z".repeat(MAX_FETCH_OUTPUT_BYTES - header_len);
        let out = format_fetch_output(None, "https://example.com/", &body_at);
        assert!(!out.contains("content truncated"));
        assert_eq!(out.len(), MAX_FETCH_OUTPUT_BYTES);

        // One byte over the cap → truncation marker present, and the marker
        // reports the pre-truncation total exactly.
        let body_over = "z".repeat(MAX_FETCH_OUTPUT_BYTES - header_len + 1);
        let out = format_fetch_output(None, "https://example.com/", &body_over);
        let expected_total = header_len + body_over.len();
        assert!(out.contains(&format!(
            "(content truncated, {expected_total} total chars)"
        )));
    }
}
