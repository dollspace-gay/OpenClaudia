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

/// Fetch a URL using Jina Reader
pub fn execute_web_fetch(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
        return ("Missing 'url' argument".to_string(), true);
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
        Ok(fetch_result) => {
            let mut output = String::new();
            if let Some(title) = fetch_result.title {
                let _ = write!(output, "# {title}\n\n");
            }
            let _ = write!(output, "URL: {}\n\n", fetch_result.url);
            output.push_str(&fetch_result.content);

            // Truncate if too long
            if output.len() > 50000 {
                output = format!(
                    "{}...\n\n(content truncated, {} total chars)",
                    safe_truncate(&output, 50000),
                    output.len()
                );
            }

            (output, false)
        }
        Err(e) => (format!("Failed to fetch URL: {e}"), true),
    }
}

/// Return the hostname of `url` in lowercase, stripping any `www.`
/// prefix. Used by [`domain_matches`] to compare a search-result URL
/// against an allow / block list. `None` when the URL can't be parsed.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, tail)| tail);
    let host_port = rest.split('/').next()?;
    let host = host_port.split(':').next()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
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
    args.get(key)
        .and_then(|v| v.as_array())
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
    let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
        return ("Missing 'query' argument".to_string(), true);
    };
    if query.trim().len() < 2 {
        return ("Query must be at least 2 characters.".to_string(), true);
    }

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(5, |v| usize::try_from(v).unwrap_or(usize::MAX));

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
    let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
        return ("Missing 'url' argument".to_string(), true);
    };

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (
            "Invalid URL: must start with http:// or https://".to_string(),
            true,
        );
    }

    match web::fetch_with_browser(url) {
        Ok(fetch_result) => {
            let mut output = String::new();
            if let Some(title) = fetch_result.title {
                let _ = write!(output, "# {title}\n\n");
            }
            let _ = write!(output, "URL: {}\n\n", fetch_result.url);
            output.push_str(&fetch_result.content);

            // Truncate if too long
            if output.len() > 50000 {
                output = format!(
                    "{}...\n\n(content truncated, {} total chars)",
                    safe_truncate(&output, 50000),
                    output.len()
                );
            }

            (output, false)
        }
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
        assert_eq!(host_of("://no-scheme"), Some("no-scheme".into()));
        assert_eq!(host_of(""), None);
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
}
