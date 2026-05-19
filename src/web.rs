//! Web tools for `OpenClaudia`
//!
//! Provides web access capabilities for agents:
//! - `web_fetch`: Fetch URL content via Jina Reader (free, handles JS/Cloudflare)
//! - `web_search`: Search the web via Tavily, Brave API, or `DuckDuckGo` (headless browser)
//! - `web_browser`: Full browser automation via headless Chrome (optional feature)

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

/// Process-wide shared `reqwest::Client` (crosslink #368).
///
/// Building a fresh `Client` on every call defeats reqwest's internal
/// connection pool and DNS cache, and — combined with a fresh tokio
/// `Runtime::new()` — leaks tokio worker threads on every web tool call.
/// One client, built once, reused everywhere. Tuned for the web-fetch
/// hot path: 90s idle pool, 10s connect timeout, TCP keepalive. The
/// per-request `timeout` overrides are still set at the call site.
pub(crate) static SHARED_HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .pool_idle_timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(10))
        .tcp_keepalive(Duration::from_mins(1))
        .build()
        .expect("shared reqwest client builds with default features")
});

/// Hostnames that always represent internal infrastructure, cloud metadata
/// endpoints, or cluster control planes. Block by name even before DNS
/// resolution — some of these resolve in odd ways across distros.
///
/// Alicloud metadata (`100.100.100.200`) and AWS IPv6 metadata
/// (`fd00:ec2::254`) are already caught by the typed CIDR checks in
/// `validate_resolved_ip`, but listing the literal IP string here adds a
/// belt-and-suspenders layer for environments where DNS returns those names
/// without resolving them.
const DANGEROUS_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "ip6-localhost",
    "ip6-loopback",
    // Cloud metadata endpoints
    "metadata",
    "metadata.google.internal",
    "metadata.goog",
    "metadata.aws",
    "metadata.tencentyun.com",
    "instance-data",
    "instance-data.ec2.internal",
    // Alicloud ECS metadata service — IP literal in shared address space (100.64/10)
    "100.100.100.200",
    // Kubernetes in-cluster endpoints
    "kubernetes",
    "kubernetes.default",
    "kubernetes.default.svc",
    "kubernetes.default.svc.cluster.local",
];

/// Parse a host string as an IP literal, including non-standard single-integer
/// forms (`http://2130706433/` = 127.0.0.1) that some resolvers accept and
/// that `url::Url` leaves as a hostname.
fn parse_host_as_ip(host: &str) -> Option<IpAddr> {
    // Standard IPv4/IPv6 textual form
    if let Ok(ip) = IpAddr::from_str(host) {
        return Some(ip);
    }
    // IPv6 in brackets
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        if let Ok(ip) = Ipv6Addr::from_str(inner) {
            return Some(IpAddr::V6(ip));
        }
    }
    // Decimal-integer IPv4 (2130706433 → 127.0.0.1)
    if !host.is_empty() && host.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = host.parse::<u32>() {
            return Some(IpAddr::V4(Ipv4Addr::from(n)));
        }
    }
    // Hex-integer IPv4 (0x7f000001 → 127.0.0.1)
    if let Some(hex) = host.strip_prefix("0x").or_else(|| host.strip_prefix("0X")) {
        if let Ok(n) = u32::from_str_radix(hex, 16) {
            return Some(IpAddr::V4(Ipv4Addr::from(n)));
        }
    }
    None
}

/// True when `v6` is on a range that should never be reachable from a public fetch.
fn is_ipv6_forbidden(v6: &Ipv6Addr) -> bool {
    // IPv4-mapped: unwrap and re-check as IPv4 (covers ::ffff:127.0.0.1 etc.)
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_ip_forbidden(&IpAddr::V4(v4));
    }
    let s = v6.segments();
    // Unique-local fc00::/7 — covers fd00:ec2::254 (AWS IPv6 metadata)
    if s[0] & 0xfe00 == 0xfc00 {
        return true;
    }
    // Link-local fe80::/10
    if s[0] & 0xffc0 == 0xfe80 {
        return true;
    }
    // 6to4 2002::/16 — deprecated tunneling; block wholesale.
    if s[0] == 0x2002 {
        return true;
    }
    // Teredo 2001:0000::/32 — tunneling.
    if s[0] == 0x2001 && s[1] == 0x0000 {
        return true;
    }
    false
}

/// Validate that a resolved [`IpAddr`] is safe to connect to (SSRF guard).
///
/// Returns `Ok(())` for routable public addresses; returns `Err` with a
/// human-readable explanation for any IANA-reserved, private, link-local,
/// cloud-metadata, or otherwise non-public range.
///
/// Covered ranges (IPv4):
/// - 0.0.0.0/8 (unspecified)
/// - 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 (RFC 1918 private)
/// - 100.64.0.0/10 (RFC 6598 carrier-grade NAT / shared address space;
///   covers Alicloud metadata 100.100.100.200)
/// - 127.0.0.0/8 (loopback)
/// - 169.254.0.0/16 (link-local / AWS + Azure + GCP metadata)
/// - 192.0.0.0/24 (IETF protocol assignments)
/// - 198.18.0.0/15 (RFC 2544 benchmarking)
/// - 203.0.113.0/24, 198.51.100.0/24, 192.0.2.0/24 (documentation)
/// - 224.0.0.0/4 (multicast)
/// - 240.0.0.0/4 (reserved for future use)
/// - 255.255.255.255/32 (broadcast)
///
/// Covered ranges (IPv6):
/// - `::1/128` (loopback)
/// - `::/128` (unspecified)
/// - `::ffff:0:0/96` (IPv4-mapped — re-checked as IPv4)
/// - `fc00::/7` (unique-local; covers `fd00:ec2::254` AWS IPv6 metadata)
/// - `fe80::/10` (link-local)
/// - `2002::/16` (6to4 tunnel)
/// - `2001::/32` (Teredo tunnel)
/// - `ff00::/8` (multicast)
///
/// # Errors
///
/// Returns `Err(String)` when `addr` falls in a reserved/internal range.
pub(crate) fn validate_resolved_ip(addr: IpAddr) -> Result<(), String> {
    if is_ip_forbidden(&addr) {
        Err(format!(
            "IP address {addr} is in a reserved/internal range and cannot be fetched"
        ))
    } else {
        Ok(())
    }
}

/// True if this IP is on any IANA-reserved, private, or otherwise non-public
/// range. Drives the SSRF guard.
fn is_ip_forbidden(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
            {
                return true;
            }
            let oct = v4.octets();
            // 100.64/10 shared address space (RFC 6598 / carrier-grade NAT)
            // Covers Alicloud metadata 100.100.100.200 (oct[1]=100, in 64..=127).
            if oct[0] == 100 && (64..=127).contains(&oct[1]) {
                return true;
            }
            // 192.0.0/24 IETF protocol assignments
            if oct[0] == 192 && oct[1] == 0 && oct[2] == 0 {
                return true;
            }
            // 198.18/15 benchmarking (RFC 2544)
            if oct[0] == 198 && (18..=19).contains(&oct[1]) {
                return true;
            }
            // 240.0.0.0/4 reserved for future use
            if oct[0] >= 240 {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || is_ipv6_forbidden(v6)
        }
    }
}

/// Validate that a URL is safe to fetch (prevents SSRF and restricts schemes).
///
/// Defense layers, in order:
///  1. Scheme allowlist (http/https only).
///  2. Hostname denylist: localhost, cloud metadata endpoints
///     (`metadata.google.internal`, `metadata.aws`, `instance-data`, etc.),
///     Kubernetes in-cluster endpoints.
///  3. IP-literal parsing: handles decimal-integer (`2130706433` = 127.0.0.1),
///     hex-integer (`0x7f000001`), bracketed IPv6, and standard dotted quads.
///     Any literal on an IANA reserved/private/link-local/multicast range
///     is rejected — full IPv4 + IPv6 matrix, not the prefix-string heuristic
///     the previous implementation used.
///  4. DNS resolution with `ToSocketAddrs`: hostnames are resolved and EVERY
///     resolved IP is checked against the same forbidden-range matrix via
///     [`validate_resolved_ip`].
///
/// Residual risk: a DNS-rebinding server that returns a public IP at
/// validate time and a private IP at `reqwest`'s dial time still bypasses
/// this. A custom `reqwest` resolver that re-checks at dial time is the
/// complete mitigation and is tracked as a follow-up to crosslink #335.
pub(crate) fn validate_url(url_str: &str) -> Result<(), String> {
    let parsed = Url::parse(url_str).map_err(|e| format!("Invalid URL: {e}"))?;

    // Scheme allowlist.
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("Unsupported URL scheme: {scheme}")),
    }

    let host = parsed.host_str().ok_or("URL has no host")?;
    let host_lower = host.to_ascii_lowercase();

    // Hostname denylist — these names should never be reachable at all.
    if DANGEROUS_HOSTNAMES.iter().any(|h| *h == host_lower) {
        return Err(format!(
            "URL host '{host}' is a known internal/metadata endpoint"
        ));
    }

    // If the host parses as an IP literal (standard, decimal, hex, IPv6-brackets),
    // check it directly — no DNS needed.
    if let Some(ip) = parse_host_as_ip(&host_lower) {
        return validate_resolved_ip(ip).map_err(|e| {
            format!("URL points to reserved/internal IP address (host was '{host}'): {e}")
        });
    }

    // Hostname → resolve and check each address.
    let port = parsed.port_or_known_default().unwrap_or(443);
    let socket_addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve host '{host}': {e}"))?
        .collect();
    if socket_addrs.is_empty() {
        return Err(format!("Host '{host}' did not resolve to any address"));
    }
    for sa in socket_addrs {
        validate_resolved_ip(sa.ip())
            .map_err(|e| format!("URL host '{host}' resolves to reserved/internal IP: {e}"))?;
    }

    Ok(())
}

/// Jina Reader base URL - converts any URL to clean markdown
const JINA_READER_URL: &str = "https://r.jina.ai/";

/// Tavily API endpoint
const TAVILY_API_URL: &str = "https://api.tavily.com/search";

/// Brave Search API endpoint
const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// `DuckDuckGo` HTML search endpoint (no API key required)
#[cfg(feature = "browser")]
const DUCKDUCKGO_HTML_URL: &str = "https://html.duckduckgo.com/html/";

/// Web configuration for API keys
#[derive(Debug, Clone, Default)]
pub struct WebConfig {
    pub tavily_api_key: Option<String>,
    pub brave_api_key: Option<String>,
}

impl WebConfig {
    /// Load web config from environment variables
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            tavily_api_key: std::env::var("TAVILY_API_KEY").ok(),
            brave_api_key: std::env::var("BRAVE_API_KEY").ok(),
        }
    }
}

/// Result from `web_fetch`
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub content: String,
    pub title: Option<String>,
    pub url: String,
}

/// Search result item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Fetch a URL using Jina Reader
///
/// Jina Reader handles:
/// - JavaScript rendering
/// - Cloudflare bypass
/// - Clean markdown output
///
/// # Errors
///
/// Returns an error string if the URL is invalid or the fetch fails.
pub async fn fetch_url(url: &str) -> Result<FetchResult, String> {
    validate_url(url)?;

    // Use Jina Reader to fetch and convert to markdown.
    // Reuses the process-wide `SHARED_HTTP_CLIENT` (crosslink #368) so the
    // connection pool and DNS cache survive across calls.
    let jina_url = format!("{JINA_READER_URL}{url}");

    let response = SHARED_HTTP_CLIENT
        .get(&jina_url)
        .timeout(Duration::from_secs(30))
        .header("Accept", "text/markdown")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch URL: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {} - {}", response.status(), url));
    }

    let content = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {e}"))?;

    // Extract title from markdown if present (first # heading)
    let title = content
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").to_string());

    Ok(FetchResult {
        content,
        title,
        url: url.to_string(),
    })
}

/// Tavily API response structure
#[derive(Debug, Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

/// Brave Search API response structure
#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: String,
}

/// Search the web using `DuckDuckGo` (default) or configured API provider
///
/// # Errors
///
/// Returns an error string if all search backends fail.
pub async fn search_web(
    query: &str,
    config: &WebConfig,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    // Try DuckDuckGo first (free, no API key required)
    // Fall back to paid APIs only if DDG fails or browser feature disabled
    let ddg_error = match search_duckduckgo(query, limit) {
        Ok(results) => return Ok(results),
        Err(e) => {
            tracing::warn!("DuckDuckGo search failed: {}", e);
            e
        }
    };

    // Fall back to Tavily if configured
    if let Some(api_key) = &config.tavily_api_key {
        return search_tavily(query, api_key, limit).await;
    }

    // Fall back to Brave if configured
    if let Some(api_key) = &config.brave_api_key {
        return search_brave(query, api_key, limit).await;
    }

    Err(format!(
        "Web search failed. DuckDuckGo error: {ddg_error}. No fallback API keys configured (TAVILY_API_KEY or BRAVE_API_KEY)."
    ))
}

/// Search using Tavily API
async fn search_tavily(
    query: &str,
    api_key: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    #[derive(Serialize)]
    struct TavilyRequest<'a> {
        api_key: &'a str,
        query: &'a str,
        max_results: usize,
        include_answer: bool,
    }

    let request = TavilyRequest {
        api_key,
        query,
        max_results: limit,
        include_answer: false,
    };

    // Shared client + per-request timeout (crosslink #368).
    let response = SHARED_HTTP_CLIENT
        .post(TAVILY_API_URL)
        .timeout(Duration::from_secs(15))
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Tavily API request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Tavily API error {status}: {body}"));
    }

    let tavily_response: TavilyResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse Tavily response: {e}"))?;

    Ok(tavily_response
        .results
        .into_iter()
        .map(|r| SearchResult {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect())
}

/// Search using Brave Search API
async fn search_brave(
    query: &str,
    api_key: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    // Shared client + per-request timeout (crosslink #368).
    let response = SHARED_HTTP_CLIENT
        .get(BRAVE_SEARCH_URL)
        .timeout(Duration::from_secs(15))
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .query(&[("q", query), ("count", &limit.to_string())])
        .send()
        .await
        .map_err(|e| format!("Brave Search API request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Brave Search API error {status}: {body}"));
    }

    let brave_response: BraveResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse Brave response: {e}"))?;

    Ok(brave_response
        .web
        .map(|w| {
            w.results
                .into_iter()
                .map(|r| SearchResult {
                    title: r.title,
                    url: r.url,
                    snippet: r.description,
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Search `DuckDuckGo` using headless Chrome browser
///
/// No API key required - scrapes the HTML search results page
///
/// # Errors
///
/// Returns an error string if the browser cannot be launched or no results are found.
#[cfg(feature = "browser")]
pub fn search_duckduckgo(query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    use headless_chrome::{Browser, LaunchOptions};
    use scraper::{Html, Selector};

    let browser = Browser::new(
        LaunchOptions::default_builder()
            .headless(true)
            .build()
            .map_err(|e| format!("Failed to configure browser: {e}"))?,
    )
    .map_err(|e| format!("Failed to launch browser: {e}"))?;

    let tab = browser
        .new_tab()
        .map_err(|e| format!("Failed to create browser tab: {e}"))?;

    // Navigate to DuckDuckGo HTML search
    let search_url = format!("{}?q={}", DUCKDUCKGO_HTML_URL, urlencoding::encode(query));

    tab.navigate_to(&search_url)
        .map_err(|e| format!("Failed to navigate to DuckDuckGo: {e}"))?;

    tab.wait_until_navigated()
        .map_err(|e| format!("Navigation timeout: {e}"))?;

    // Wait for page to load
    std::thread::sleep(Duration::from_millis(500));

    // Get page HTML
    let html = tab
        .get_content()
        .map_err(|e| format!("Failed to get page content: {e}"))?;

    // Parse HTML and extract results
    let document = Html::parse_document(&html);

    // DDG HTML selectors
    let result_selector =
        Selector::parse(".result").map_err(|e| format!("Invalid selector: {e:?}"))?;
    let title_selector =
        Selector::parse(".result__a").map_err(|e| format!("Invalid selector: {e:?}"))?;
    let snippet_selector =
        Selector::parse(".result__snippet").map_err(|e| format!("Invalid selector: {e:?}"))?;

    let mut results = Vec::new();

    for result_element in document.select(&result_selector).take(limit) {
        // Get title and URL from the link
        if let Some(title_element) = result_element.select(&title_selector).next() {
            let title = title_element.text().collect::<String>().trim().to_string();

            // Get URL from href attribute - DDG wraps URLs in a redirect
            let url = title_element
                .value()
                .attr("href")
                .map(|href| {
                    // DDG HTML uses direct URLs or //duckduckgo.com/l/?uddg=<encoded_url>
                    if href.starts_with("//duckduckgo.com/l/") {
                        // Extract the actual URL from the redirect
                        href.find("uddg=").map_or_else(
                            || href.to_string(),
                            |uddg_start| {
                                let encoded = &href[uddg_start + 5..];
                                // Find end of URL (next & or end of string)
                                let end = encoded.find('&').unwrap_or(encoded.len());
                                urlencoding::decode(&encoded[..end])
                                    .map_or_else(|_| href.to_string(), std::borrow::Cow::into_owned)
                            },
                        )
                    } else if href.starts_with("http") {
                        href.to_string()
                    } else {
                        format!("https:{href}")
                    }
                })
                .unwrap_or_default();

            // Skip if no valid URL
            if url.is_empty() || !url.starts_with("http") {
                continue;
            }

            // SSRF guard (#610): validate every URL extracted from DDG HTML before
            // returning it to the agent.  A malicious or compromised DDG response
            // could embed private-IP / metadata URLs in result hrefs.
            if let Err(reason) = validate_url(&url) {
                tracing::debug!(
                    url = %url,
                    reason = %reason,
                    "DDG result URL dropped by SSRF guard"
                );
                continue;
            }

            // Get snippet
            let snippet = result_element
                .select(&snippet_selector)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            // Skip results without meaningful content
            if !title.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }
    }

    if results.is_empty() {
        return Err(
            "No search results found. DuckDuckGo may have changed their HTML structure."
                .to_string(),
        );
    }

    Ok(results)
}

#[cfg(not(feature = "browser"))]
pub fn search_duckduckgo(_query: &str, _limit: usize) -> Result<Vec<SearchResult>, String> {
    Err("DuckDuckGo search requires the browser feature. Rebuild with `cargo build --features browser` or set TAVILY_API_KEY/BRAVE_API_KEY.".to_string())
}

/// Fetch URL using headless Chrome browser
///
/// Use this when Jina Reader fails (e.g., complex authentication, specific Cloudflare challenges)
///
/// # Errors
///
/// Returns an error string if the URL is invalid or browser automation fails.
#[cfg(feature = "browser")]
pub fn fetch_with_browser(url: &str) -> Result<FetchResult, String> {
    use headless_chrome::{Browser, LaunchOptions};

    validate_url(url)?;

    let browser = Browser::new(
        LaunchOptions::default_builder()
            .headless(true)
            .build()
            .map_err(|e| format!("Failed to configure browser: {e}"))?,
    )
    .map_err(|e| format!("Failed to launch browser: {e}"))?;

    let tab = browser
        .new_tab()
        .map_err(|e| format!("Failed to create browser tab: {e}"))?;

    tab.navigate_to(url)
        .map_err(|e| format!("Failed to navigate to URL: {e}"))?;

    tab.wait_until_navigated()
        .map_err(|e| format!("Navigation timeout: {e}"))?;

    // Wait a bit for JavaScript to render
    std::thread::sleep(Duration::from_secs(2));

    // Get page content
    let content = tab
        .get_content()
        .map_err(|e| format!("Failed to get page content: {e}"))?;

    // Get title
    let title = tab.get_title().ok();

    Ok(FetchResult {
        content,
        title,
        url: url.to_string(),
    })
}

/// Fetch URL using headless Chrome browser (stub when browser feature is disabled)
///
/// # Errors
///
/// Always returns an error when the browser feature is not enabled.
#[cfg(not(feature = "browser"))]
pub fn fetch_with_browser(url: &str) -> Result<FetchResult, String> {
    validate_url(url)?;
    Err("Browser feature not enabled. Rebuild with `cargo build --features browser`".to_string())
}

/// Format search results for display to the agent
#[must_use]
pub fn format_search_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No results found.".to_string();
    }

    let mut output = format!("Found {} results:\n\n", results.len());

    for (i, result) in results.iter().enumerate() {
        let _ = write!(
            output,
            "{}. **{}**\n   {}\n   URL: {}\n\n",
            i + 1,
            result.title,
            result.snippet,
            result.url
        );
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_config_from_env() {
        // Just test that it doesn't panic
        let _config = WebConfig::from_env();
    }

    #[test]
    fn test_format_search_results() {
        let results = vec![SearchResult {
            title: "Test Result".to_string(),
            url: "https://example.com".to_string(),
            snippet: "This is a test result".to_string(),
        }];

        let formatted = format_search_results(&results);
        assert!(formatted.contains("Test Result"));
        assert!(formatted.contains("https://example.com"));
    }

    #[test]
    fn test_format_empty_results() {
        let results: Vec<SearchResult> = vec![];
        let formatted = format_search_results(&results);
        assert!(formatted.contains("No results found"));
    }

    #[test]
    fn test_validate_url_allows_http() {
        assert!(validate_url("http://example.com").is_ok());
    }

    #[test]
    fn test_validate_url_allows_https() {
        assert!(validate_url("https://example.com/path?q=1").is_ok());
    }

    #[test]
    fn test_validate_url_blocks_file_scheme() {
        let result = validate_url("file:///etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported URL scheme"));
    }

    #[test]
    fn test_validate_url_blocks_data_scheme() {
        let result = validate_url("data:text/html,<h1>hi</h1>");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported URL scheme"));
    }

    #[test]
    fn test_validate_url_blocks_ftp_scheme() {
        let result = validate_url("ftp://files.example.com/secret");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported URL scheme"));
    }

    #[test]
    fn test_validate_url_blocks_localhost() {
        let result = validate_url("http://localhost:8080/admin");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_127() {
        let result = validate_url("http://127.0.0.1:9090/");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_10_network() {
        let result = validate_url("http://10.0.0.1/internal");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_192_168() {
        let result = validate_url("http://192.168.1.1/router");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_172_16() {
        let result = validate_url("http://172.16.0.1/");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_169_254_link_local() {
        let result = validate_url("http://169.254.169.254/latest/meta-data/");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_zero_address() {
        let result = validate_url("http://0.0.0.0/");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_blocks_ipv6_loopback() {
        let result = validate_url("http://[::1]:8080/");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn test_validate_url_rejects_invalid_url() {
        let result = validate_url("not a url at all");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid URL"));
    }

    // --- SSRF regression tests (crosslink #335) ---

    #[test]
    fn ssrf_decimal_encoded_ipv4_loopback() {
        // 2130706433 decimal == 127.0.0.1
        let err = validate_url("http://2130706433/").unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("Invalid URL"),
            "decimal-encoded 127.0.0.1 not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_hex_encoded_ipv4_loopback() {
        let err = validate_url("http://0x7f000001/").unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("Invalid URL"),
            "hex-encoded 127.0.0.1 not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_gcp_metadata_hostname() {
        let err = validate_url("http://metadata.google.internal/").unwrap_err();
        assert!(
            err.contains("metadata endpoint"),
            "metadata.google.internal not denylisted: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_aws_metadata_hostname() {
        let err = validate_url("http://instance-data/").unwrap_err();
        assert!(
            err.contains("metadata endpoint"),
            "instance-data not denylisted: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_k8s_api_hostname() {
        let err = validate_url("http://kubernetes.default.svc/api/v1/secrets").unwrap_err();
        assert!(
            err.contains("metadata endpoint"),
            "kubernetes.default.svc not denylisted: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_ipv6_loopback_mapped() {
        let err = validate_url("http://[::ffff:127.0.0.1]/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "IPv4-mapped IPv6 loopback not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_ipv6_unique_local() {
        let err = validate_url("http://[fc00::1]/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "IPv6 unique-local not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_ipv6_link_local() {
        let err = validate_url("http://[fe80::1]/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "IPv6 link-local not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_shared_address_space() {
        let err = validate_url("http://100.64.0.1/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "100.64/10 CGNAT not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_benchmarking_range() {
        let err = validate_url("http://198.18.0.1/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "198.18/15 benchmarking not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_previous_prefix_bug_172_200_now_allowed() {
        // The previous impl wrongly blocked 172.200.x (public) via
        // `starts_with("172.2")`. Public 172.200.0.1 must NOT be rejected
        // by the host check.
        let result = validate_url("http://172.200.0.1/");
        if let Err(e) = result {
            assert!(
                !e.contains("reserved/internal"),
                "172.200.0.1 (public) wrongly classified as internal: {e}"
            );
        }
    }

    #[test]
    fn ssrf_blocks_172_all_private_slashes() {
        for middle in [16_u8, 20, 25, 28, 31] {
            let url = format!("http://172.{middle}.0.1/");
            let err = validate_url(&url).unwrap_err();
            assert!(
                err.contains("reserved/internal"),
                "172.{middle}.0.1 (private) not blocked: {err}"
            );
        }
    }

    #[test]
    fn ssrf_blocks_documentation_range() {
        let err = validate_url("http://203.0.113.1/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "203.0.113/24 (documentation) not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_blocks_future_reserved_240() {
        let err = validate_url("http://240.0.0.1/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "240.0.0.0/4 (reserved) not blocked: {err}"
        );
    }

    #[test]
    fn ssrf_allows_public_ipv4() {
        assert!(validate_url("http://8.8.8.8/").is_ok());
    }

    // ── Forensic bypass-vector tests (crosslink #290) ────────────────────────
    // Each test targets a specific encoding or protocol trick that the old
    // prefix-string guard could not detect.

    /// Bypass vector 1 — decimal-integer IPv4.
    /// `http://2130706433/` encodes 127.0.0.1 as a 32-bit decimal integer.
    /// Some HTTP stacks (curl, Python urllib) dereference this directly.
    /// `parse_host_as_ip` decodes it; `validate_resolved_ip` blocks it.
    #[test]
    fn bypass_decimal_encoded_loopback() {
        // 127 * 2^24 + 0 * 2^16 + 0 * 2^8 + 1 = 2130706433
        let err = validate_url("http://2130706433/secret").unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("Invalid URL"),
            "decimal-encoded 127.0.0.1 not blocked: {err}"
        );
    }

    /// Bypass vector 2 — hex-integer IPv4.
    /// `http://0x7f000001/` is the hex form of 127.0.0.1.
    #[test]
    fn bypass_hex_encoded_loopback() {
        let err = validate_url("http://0x7f000001/admin").unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("Invalid URL"),
            "hex-encoded 127.0.0.1 not blocked: {err}"
        );
    }

    /// Bypass vector 3 — IPv6 short-form loopback `[::1]`.
    /// The bracket-stripping in `parse_host_as_ip` must handle this form.
    #[test]
    fn bypass_ipv6_short_form_loopback() {
        let err = validate_url("http://[::1]:9090/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "IPv6 ::1 short-form loopback not blocked: {err}"
        );
    }

    /// Bypass vector 4 — IPv4-mapped IPv6 loopback `::ffff:127.0.0.1`.
    /// `is_ipv6_forbidden` unwraps via `to_ipv4_mapped()` and re-checks the
    /// inner IPv4 address against `is_ip_forbidden`.
    #[test]
    fn bypass_ipv6_mapped_loopback() {
        let err = validate_url("http://[::ffff:127.0.0.1]/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "::ffff:127.0.0.1 (IPv4-mapped loopback) not blocked: {err}"
        );
    }

    /// Bypass vector 5 — AWS EC2 instance metadata service (169.254.169.254).
    /// This is the well-known link-local address served on every AWS/Azure/GCP
    /// VM; `Ipv4Addr::is_link_local()` catches the entire 169.254/16 range.
    #[test]
    fn bypass_aws_metadata_ip() {
        let err = validate_url("http://169.254.169.254/latest/meta-data/iam/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "169.254.169.254 (AWS metadata) not blocked: {err}"
        );
    }

    /// Bypass vector 6 — Alicloud ECS metadata (100.100.100.200).
    /// Sits in RFC 6598 shared address space (100.64/10);
    /// `(64..=127).contains(&oct[1])` blocks it. Belt-and-suspenders: also
    /// listed literally in `DANGEROUS_HOSTNAMES`, so either message is valid.
    #[test]
    fn bypass_alicloud_metadata_ip() {
        let err = validate_url("http://100.100.100.200/latest/meta-data/").unwrap_err();
        assert!(
            err.contains("reserved/internal") || err.contains("metadata endpoint"),
            "100.100.100.200 (Alicloud metadata) not blocked: {err}"
        );
    }

    /// Bypass vector 7 — AWS IPv6 metadata endpoint (`fd00:ec2::254`).
    /// Lives in `fc00::/7` (unique-local); `is_ipv6_forbidden` blocks it via the
    /// `s[0] & 0xfe00 == 0xfc00` check.
    #[test]
    fn bypass_aws_ipv6_metadata_ip() {
        let err = validate_url("http://[fd00:ec2::254]/latest/meta-data/").unwrap_err();
        assert!(
            err.contains("reserved/internal"),
            "fd00:ec2::254 (AWS IPv6 metadata) not blocked: {err}"
        );
    }

    /// Positive case — a known public IP must NOT be rejected.
    /// Regression guard against the old `172.2*` prefix bug that blocked
    /// `172.200.x` (public address space).
    #[test]
    fn bypass_public_ipv4_is_allowed() {
        assert!(
            validate_url("http://172.200.0.1/public-resource").is_ok(),
            "172.200.0.1 (public) was wrongly rejected"
        );
    }

    /// Positive case — a public hostname must NOT be rejected.
    /// Guards against over-blocking in the hostname denylist or resolver path.
    #[test]
    fn bypass_public_hostname_is_allowed() {
        // example.com resolves to public addresses (93.184.216.34) and is
        // the canonical safe hostname for tests.
        assert!(
            validate_url("https://example.com/").is_ok(),
            "example.com (public hostname) was wrongly rejected"
        );
    }

    /// Direct unit test for `validate_resolved_ip` — private RFC 1918 address.
    #[test]
    fn validate_resolved_ip_rejects_rfc1918() {
        let private = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert!(
            validate_resolved_ip(private).is_err(),
            "192.168.1.100 (RFC1918) not rejected by validate_resolved_ip"
        );
    }

    /// Direct unit test for `validate_resolved_ip` — public address passes.
    #[test]
    fn validate_resolved_ip_allows_public() {
        let public = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(
            validate_resolved_ip(public).is_ok(),
            "8.8.8.8 (public) wrongly rejected by validate_resolved_ip"
        );
    }
}
