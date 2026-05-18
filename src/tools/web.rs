use crate::tools::safe_truncate;
use crate::web::{self, WebConfig};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use tokio::runtime::Handle;

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

    // Use tokio runtime to execute async function
    let result = match Handle::try_current() {
        Ok(handle) => {
            // We're in an async context, use block_in_place
            tokio::task::block_in_place(|| handle.block_on(web::fetch_url(url)))
        }
        Err(_) => {
            // Create a new runtime for sync context
            match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(web::fetch_url(url)),
                Err(e) => return (format!("Failed to create runtime: {e}"), true),
            }
        }
    };

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

    // Use tokio runtime to execute async function
    let result = match Handle::try_current() {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(web::search_web(query, &config, limit)))
        }
        Err(_) => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(web::search_web(query, &config, limit)),
            Err(e) => return (format!("Failed to create runtime: {e}"), true),
        },
    };

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
}
