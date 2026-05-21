//! End-to-end tests for `ToolInterceptor` streaming-buffer state
//! machine + `extract_tool_calls` / `extract_all_tool_calls` /
//! `strip_hallucinated_blocks` / `has_pending_tool_calls` /
//! `has_complete_block`.
//!
//! Sprint 69 of the verification effort. Pure-logic but
//! highly stateful surface; the position-cached marker scan
//! (crosslink #743), the buffer-cap defence (#343), and the
//! invoke-vs-shorthand format parsing are all worth pinning
//! at the integration boundary.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tool_intercept::ToolInterceptor;

// ───────────────────────────────────────────────────────────────────────────
// Section A — Fresh interceptor state
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn fresh_interceptor_has_empty_buffer() {
    let interceptor = ToolInterceptor::new();
    assert!(interceptor.get_buffer().is_empty());
}

#[test]
fn fresh_interceptor_default_matches_new() {
    let default = ToolInterceptor::default();
    let new = ToolInterceptor::new();
    assert_eq!(default.get_buffer(), new.get_buffer());
}

#[test]
fn fresh_interceptor_has_no_pending_or_complete_blocks() {
    let mut interceptor = ToolInterceptor::new();
    assert!(!interceptor.has_pending_tool_calls());
    assert!(!interceptor.has_complete_block());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — push + buffer accumulation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn push_appends_content_to_buffer() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("hello");
    assert_eq!(interceptor.get_buffer(), "hello");
    interceptor.push(" world");
    assert_eq!(interceptor.get_buffer(), "hello world");
}

#[test]
fn clear_resets_buffer_and_pending_state() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>ls</bash>");
    assert!(interceptor.has_complete_block());
    interceptor.clear();
    assert!(interceptor.get_buffer().is_empty());
    assert!(!interceptor.has_complete_block());
}

#[test]
fn push_empty_string_is_noop() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("abc");
    interceptor.push("");
    assert_eq!(interceptor.get_buffer(), "abc");
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — has_complete_block — invoke format
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn has_complete_block_true_for_closed_invoke() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push(r#"<invoke name="bash"><parameter name="command">ls</parameter></invoke>"#);
    assert!(interceptor.has_complete_block());
}

#[test]
fn has_complete_block_false_for_unclosed_invoke() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push(r#"<invoke name="bash"><parameter name="command">ls</parameter>"#);
    // No closing </invoke> yet → not complete.
    assert!(!interceptor.has_complete_block());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — has_complete_block — shorthand format
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn has_complete_block_true_for_each_shorthand_tool_pair() {
    for tool in &["bash", "read", "write", "edit", "glob", "grep"] {
        let mut interceptor = ToolInterceptor::new();
        let s = format!("<{tool}>content</{tool}>");
        interceptor.push(&s);
        assert!(
            interceptor.has_complete_block(),
            "shorthand <{tool}>...</{tool}> MUST be complete"
        );
    }
}

#[test]
fn has_complete_block_false_for_shorthand_open_without_close() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>ls -la");
    assert!(!interceptor.has_complete_block());
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — extract_tool_calls — invoke format
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extract_invoke_format_returns_tool_with_parsed_parameters() {
    let mut interceptor = ToolInterceptor::new();
    interceptor
        .push(r#"<invoke name="Bash"><parameter name="command">echo hi</parameter></invoke>"#);
    let (tools, before, _after) = interceptor.extract_tool_calls();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "Bash");
    assert_eq!(
        tools[0].parameters.get("command").map(String::as_str),
        Some("echo hi")
    );
    assert!(before.is_empty(), "no text before tool call");
}

#[test]
fn extract_invoke_preserves_text_before_tool_call() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push(
        r#"some thinking before <invoke name="Bash"><parameter name="command">ls</parameter></invoke>"#,
    );
    let (tools, before, _after) = interceptor.extract_tool_calls();
    assert_eq!(tools.len(), 1);
    assert!(
        before.contains("some thinking before"),
        "before MUST preserve prefix text; got {before:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — extract_tool_calls — shorthand format
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extract_shorthand_bash_returns_command_parameter() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>ls -la</bash>");
    let (tools, _before, _after) = interceptor.extract_tool_calls();
    assert_eq!(tools.len(), 1);
    // Shorthand <bash> resolves to canonical Bash with
    // command parameter.
    let cmd = tools[0]
        .parameters
        .values()
        .next()
        .expect("at least one parameter");
    assert!(
        cmd.contains("ls -la"),
        "parameter MUST carry command; got {cmd:?}"
    );
}

#[test]
fn extract_shorthand_with_no_tags_returns_empty_with_full_buffer() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("just plain text, no tags");
    let (tools, before, after) = interceptor.extract_tool_calls();
    assert!(tools.is_empty());
    assert_eq!(before, "just plain text, no tags");
    assert!(after.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — strip_hallucinated_blocks
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn strip_function_results_removes_hallucinated_output() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>ls</bash><function_results>fake output here</function_results>");
    interceptor.strip_hallucinated_blocks();
    let buf = interceptor.get_buffer();
    assert!(
        !buf.contains("fake output here"),
        "hallucinated <function_results> content MUST be stripped; got {buf:?}"
    );
    // The real <bash> shorthand survives.
    assert!(buf.contains("<bash>ls</bash>"));
}

#[test]
fn strip_function_results_handles_mismatched_close_tag() {
    // Models sometimes hallucinate </function_calls> instead
    // of </function_results>; pin the fallback path.
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>ls</bash><function_results>x y z</function_calls>tail content");
    interceptor.strip_hallucinated_blocks();
    let buf = interceptor.get_buffer();
    assert!(!buf.contains("x y z"));
    assert!(buf.contains("tail content"));
}

#[test]
fn strip_function_results_with_no_close_tag_drops_to_end_of_buffer() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>ls</bash><function_results>never closed payload");
    interceptor.strip_hallucinated_blocks();
    let buf = interceptor.get_buffer();
    assert!(
        !buf.contains("never closed payload"),
        "unclosed <function_results> MUST drop to end of buffer"
    );
    assert!(buf.contains("<bash>ls</bash>"));
}

#[test]
fn strip_function_calls_wrapper_tags_keeps_inner_content() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<function_calls><invoke name=\"X\"></invoke></function_calls>");
    interceptor.strip_hallucinated_blocks();
    let buf = interceptor.get_buffer();
    assert!(
        !buf.contains("<function_calls>"),
        "wrapper tag MUST be removed; got {buf:?}"
    );
    assert!(
        !buf.contains("</function_calls>"),
        "closing wrapper MUST be removed; got {buf:?}"
    );
    // The inner content survives.
    assert!(buf.contains("<invoke"));
}

#[test]
fn strip_is_noop_when_no_hallucinated_blocks_present() {
    let mut interceptor = ToolInterceptor::new();
    let original = "<bash>ls</bash>real content";
    interceptor.push(original);
    interceptor.strip_hallucinated_blocks();
    assert_eq!(interceptor.get_buffer(), original);
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — extract_all_tool_calls
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extract_all_returns_every_tool_call_from_multi_tool_buffer() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>cmd1</bash><bash>cmd2</bash><bash>cmd3</bash>");
    let (tools, _texts) = interceptor.extract_all_tool_calls();
    assert_eq!(
        tools.len(),
        3,
        "MUST extract all 3 shorthand tools; got {}",
        tools.len()
    );
}

#[test]
fn extract_all_interleaves_text_and_tools() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("Step 1: <bash>ls</bash>Step 2: <bash>pwd</bash>");
    let (tools, texts) = interceptor.extract_all_tool_calls();
    assert_eq!(tools.len(), 2);
    // Text chunks between tools captured.
    assert!(!texts.is_empty(), "MUST capture interleaved text chunks");
    let joined = texts.join(" ");
    assert!(
        joined.contains("Step 1") || joined.contains("Step 2"),
        "interleaved text MUST include step labels; got {texts:?}"
    );
}

#[test]
fn extract_all_strips_hallucinated_results_before_extracting() {
    let mut interceptor = ToolInterceptor::new();
    interceptor
        .push("<bash>real1</bash><function_results>FAKE1</function_results><bash>real2</bash>");
    let (tools, texts) = interceptor.extract_all_tool_calls();
    assert_eq!(tools.len(), 2, "2 real tools MUST be extracted");
    let joined = texts.join("");
    assert!(
        !joined.contains("FAKE1"),
        "hallucinated result MUST NOT leak into texts; got {texts:?}"
    );
}

#[test]
fn extract_all_with_empty_buffer_returns_empty_vecs() {
    let mut interceptor = ToolInterceptor::new();
    let (tools, _texts) = interceptor.extract_all_tool_calls();
    assert!(tools.is_empty());
}

// ───────────────────────────────────────────────────────────────────────────
// Section I — Buffer cap defence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn push_beyond_buffer_cap_silently_drops_excess() {
    // Documented MAX_BUFFER_BYTES = 4 MiB; push 5 MiB and
    // verify the buffer stops growing at the cap.
    let mut interceptor = ToolInterceptor::new();
    let chunk_size = 1 << 20; // 1 MiB
    let chunk: String = "x".repeat(chunk_size);
    for _ in 0..5 {
        interceptor.push(&chunk);
    }
    let len = interceptor.get_buffer().len();
    assert!(
        len <= 4 * 1024 * 1024,
        "buffer MUST NOT exceed 4 MiB cap; got {len} bytes"
    );
}

#[test]
fn push_at_cap_drops_subsequent_pushes_entirely() {
    let mut interceptor = ToolInterceptor::new();
    let chunk: String = "x".repeat(4 * 1024 * 1024); // exactly cap
    interceptor.push(&chunk);
    let before = interceptor.get_buffer().len();
    interceptor.push("extra content");
    let after = interceptor.get_buffer().len();
    assert_eq!(
        before, after,
        "at-cap push MUST be a no-op; got grew from {before} to {after}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section J — has_pending_tool_calls
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn has_pending_tool_calls_true_for_open_invoke() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push(r#"<invoke name="X">"#);
    assert!(
        interceptor.has_pending_tool_calls(),
        "open invoke MUST be pending"
    );
}

#[test]
fn has_pending_tool_calls_true_for_open_shorthand() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("<bash>incomplete");
    assert!(interceptor.has_pending_tool_calls());
}

#[test]
fn has_pending_tool_calls_false_for_plain_text() {
    let mut interceptor = ToolInterceptor::new();
    interceptor.push("just some plain prose response with no tags");
    assert!(!interceptor.has_pending_tool_calls());
}
