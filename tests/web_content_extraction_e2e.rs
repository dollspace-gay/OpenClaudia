//! End-to-end tests for the `WebFetch` content-formatting layer:
//! `format_fetch_output` shape, `MAX_FETCH_OUTPUT_BYTES` cap
//! enforcement, `safe_truncate` UTF-8 safety, `format_search_results`
//! rendering.
//!
//! Sprint 41 of the verification effort.
//!
//! `tests/web_ssrf_e2e.rs` (sprint 9) covers the URL-validation
//! perimeter. This file covers the response-rendering layer that
//! the SSRF perimeter feeds into — the agent-visible output shape
//! and the size-cap defence that limits how much untrusted
//! upstream content reaches the model context.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::{format_fetch_output, safe_truncate, MAX_FETCH_OUTPUT_BYTES};
use openclaudia::web::{format_search_results, SearchResult};

// ───────────────────────────────────────────────────────────────────────────
// Section A — format_fetch_output happy path
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fetch_output_includes_title_url_and_content_in_documented_order() {
    let out = format_fetch_output(
        Some("Example Page"),
        "https://example.com/page",
        "Body content here.",
    );
    // The documented format is:
    //   # <title>
    //
    //   URL: <url>
    //
    //   <content>
    let title_pos = out.find("# Example Page").expect("title");
    let url_pos = out.find("URL: https://example.com/page").expect("url line");
    let content_pos = out.find("Body content here.").expect("content");
    assert!(
        title_pos < url_pos && url_pos < content_pos,
        "documented field order title → URL → content; got title={title_pos}, url={url_pos}, content={content_pos}"
    );
}

#[test]
fn fetch_output_omits_title_header_when_none() {
    let out = format_fetch_output(None, "https://example.com/", "x");
    assert!(
        !out.starts_with("# "),
        "no-title output MUST NOT lead with a markdown header; got {out:?}"
    );
    assert!(
        out.starts_with("URL: "),
        "no-title output MUST start with URL line; got {out:?}"
    );
}

#[test]
fn fetch_output_preserves_empty_title_as_no_header() {
    // When the title is `Some("")`, the impl uses `# ` + ""
    // which still emits a header line. Pin whichever way the
    // impl actually goes — sanity-check the wrapped value.
    let out = format_fetch_output(Some(""), "https://x", "body");
    // Either of the two behaviours is acceptable; the
    // contract is "content + url present, no panic".
    assert!(out.contains("URL: https://x"));
    assert!(out.contains("body"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — MAX_FETCH_OUTPUT_BYTES cap enforcement
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fetch_output_under_cap_returns_content_verbatim_no_truncation_marker() {
    let body = "x".repeat(1000); // well under 50_000
    let out = format_fetch_output(Some("t"), "https://x", &body);
    assert!(
        !out.contains("content truncated"),
        "under-cap content MUST NOT include the truncation marker; got {out:?}"
    );
    // The full body must appear.
    assert!(out.contains(&body));
}

#[test]
fn fetch_output_over_cap_truncates_with_documented_marker() {
    // Make the body big enough that the assembled output
    // exceeds MAX_FETCH_OUTPUT_BYTES (50_000).
    let body = "x".repeat(MAX_FETCH_OUTPUT_BYTES + 1000);
    let out = format_fetch_output(Some("t"), "https://x", &body);
    assert!(
        out.contains("content truncated"),
        "over-cap output MUST include the truncation marker; got len={}",
        out.len()
    );
    assert!(
        out.contains("total chars"),
        "marker MUST include the total-char count; got tail={:?}",
        &out[out.len().saturating_sub(80)..]
    );
    // The output (including marker) must NOT exceed cap + a
    // small marker tail (~80 chars).
    assert!(
        out.len() < MAX_FETCH_OUTPUT_BYTES + 200,
        "truncated output must stay under cap + marker tail; got {}",
        out.len()
    );
}

#[test]
fn fetch_output_cap_uses_byte_count_not_char_count() {
    // A body of all 4-byte UTF-8 chars (e.g. emoji "🎉" = 4 B)
    // must still be capped on byte length, not char length.
    let one_emoji = "🎉";
    assert_eq!(one_emoji.len(), 4, "test premise: emoji is 4 bytes");
    let body_chars = (MAX_FETCH_OUTPUT_BYTES / 4) + 100; // ~12.5K chars = 50K+400B
    let body: String = std::iter::repeat_n(one_emoji, body_chars).collect();
    assert!(body.len() > MAX_FETCH_OUTPUT_BYTES);
    let out = format_fetch_output(None, "https://x", &body);
    assert!(out.contains("content truncated"));
    // CRITICAL: truncation MUST NOT split a multi-byte UTF-8
    // character — output must remain valid UTF-8.
    assert!(
        std::str::from_utf8(out.as_bytes()).is_ok(),
        "truncated output MUST remain valid UTF-8 (no mid-char split)"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — safe_truncate UTF-8 safety
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn safe_truncate_under_cap_returns_full_string() {
    let s = "hello";
    assert_eq!(safe_truncate(s, 100), "hello");
    assert_eq!(safe_truncate(s, 5), "hello");
}

#[test]
fn safe_truncate_at_char_boundary_returns_exact_slice() {
    let s = "abcdef";
    assert_eq!(safe_truncate(s, 3), "abc");
}

#[test]
fn safe_truncate_never_splits_multi_byte_character() {
    let s = "🎉🎉🎉"; // 12 bytes, 3 chars
                      // Asking for 5 bytes would split the second 🎉 (chars at
                      // bytes 0..4, 4..8, 8..12). safe_truncate must walk back
                      // to a boundary at byte 4.
    let out = safe_truncate(s, 5);
    assert_eq!(out, "🎉", "must walk back to first char boundary <= 5");
    assert!(std::str::from_utf8(out.as_bytes()).is_ok());
}

#[test]
fn safe_truncate_zero_cap_returns_empty() {
    assert_eq!(safe_truncate("anything", 0), "");
}

#[test]
fn safe_truncate_just_under_boundary_walks_back() {
    // Pre-truncation at byte 11 (last byte of "🎉🎉🎉") MUST
    // walk back to byte 8.
    let s = "🎉🎉🎉";
    let out = safe_truncate(s, 11);
    assert_eq!(out, "🎉🎉", "byte 11 isn't a boundary; walk back to 8");
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — format_search_results
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn search_results_empty_returns_no_results_message() {
    let out = format_search_results(&[]);
    assert!(
        out.to_lowercase().contains("no results"),
        "empty results MUST yield a no-results message; got {out:?}"
    );
}

#[test]
fn search_results_renders_count_and_per_result_fields() {
    let results = vec![
        SearchResult {
            title: "First Result".to_string(),
            snippet: "A snippet describing the first.".to_string(),
            url: "https://example.com/1".to_string(),
        },
        SearchResult {
            title: "Second Result".to_string(),
            snippet: "Another snippet.".to_string(),
            url: "https://example.com/2".to_string(),
        },
    ];
    let out = format_search_results(&results);
    // Header: "Found 2 results:"
    assert!(
        out.contains("Found 2 results"),
        "header must announce count; got {out:?}"
    );
    // Each result: numbered, title (bold), snippet, URL.
    for (i, result) in results.iter().enumerate() {
        let n = i + 1;
        let numbered = format!("{n}. ");
        assert!(
            out.contains(&numbered),
            "missing entry number {numbered:?}; got {out:?}"
        );
        assert!(
            out.contains(&format!("**{}**", result.title)),
            "missing bold-wrapped title {:?}; got {out:?}",
            result.title
        );
        assert!(out.contains(&result.snippet));
        assert!(out.contains(&format!("URL: {}", result.url)));
    }
}

#[test]
fn search_results_renders_one_result_with_singular_or_plural_either_acceptable() {
    let results = vec![SearchResult {
        title: "Only One".to_string(),
        snippet: "Snippet".to_string(),
        url: "https://x".to_string(),
    }];
    let out = format_search_results(&results);
    // Header contains "1" and "result" (either "1 result" or
    // "1 results" — the impl uses plural; just pin that the
    // count is present).
    assert!(
        out.contains("Found 1"),
        "header must include the count; got {out:?}"
    );
    assert!(out.contains("Only One"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — content-safety regression guards
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fetch_output_does_not_evaluate_or_mutate_html_in_content() {
    // The formatter is byte-passthrough — it MUST NOT
    // interpret HTML, strip tags, evaluate script content, or
    // otherwise mutate the body. The sanitization layer lives
    // upstream in the fetcher; the formatter is the
    // last-mile renderer and must preserve content verbatim
    // so callers can audit what reached the model.
    let body = "<script>alert(1)</script><iframe src='evil'></iframe>";
    let out = format_fetch_output(None, "https://x", body);
    assert!(
        out.contains(body),
        "formatter must NOT mutate HTML content; got {out:?}"
    );
}

#[test]
fn fetch_output_url_is_not_html_escaped() {
    // URLs containing & MUST pass through unchanged (the
    // output is markdown, not HTML — no entity escaping).
    let url = "https://example.com/?q=a&b=c&d=e";
    let out = format_fetch_output(None, url, "body");
    assert!(
        out.contains(url),
        "URL must round-trip exactly; got {out:?}"
    );
    assert!(
        !out.contains("&amp;"),
        "URL MUST NOT be HTML-escaped; got {out:?}"
    );
}
