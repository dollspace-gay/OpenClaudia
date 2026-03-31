use crate::tools::safe_truncate;
use crate::web::{self, WebConfig};
use serde_json::Value;
use std::collections::HashMap;
use tokio::runtime::Handle;

/// Fetch a URL using Jina Reader
pub(crate) fn execute_web_fetch(args: &HashMap<String, Value>) -> (String, bool) {
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return ("Missing 'url' argument".to_string(), true),
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
                Err(e) => return (format!("Failed to create runtime: {}", e), true),
            }
        }
    };

    match result {
        Ok(fetch_result) => {
            let mut output = String::new();
            if let Some(title) = fetch_result.title {
                output.push_str(&format!("# {}\n\n", title));
            }
            output.push_str(&format!("URL: {}\n\n", fetch_result.url));
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
        Err(e) => (format!("Failed to fetch URL: {}", e), true),
    }
}

/// Search the web using Tavily or Brave API
pub(crate) fn execute_web_search(args: &HashMap<String, Value>) -> (String, bool) {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ("Missing 'query' argument".to_string(), true),
    };

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

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
            Err(e) => return (format!("Failed to create runtime: {}", e), true),
        },
    };

    match result {
        Ok(results) => (web::format_search_results(&results), false),
        Err(e) => (format!("Search failed: {}", e), true),
    }
}

/// Fetch a URL using headless Chrome browser
/// Fallback for when Jina Reader fails on complex sites
pub(crate) fn execute_web_browser(args: &HashMap<String, Value>) -> (String, bool) {
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return ("Missing 'url' argument".to_string(), true),
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
                output.push_str(&format!("# {}\n\n", title));
            }
            output.push_str(&format!("URL: {}\n\n", fetch_result.url));
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
        Err(e) => (format!("Browser fetch failed: {}", e), true),
    }
}
