//! Integration tests for `pipeline.rs` — B1 (retry on 429/529/503) and
//! B3 (SSE tool-call accumulation).
//!
//! Uses `wiremock` to fake the upstream HTTP endpoint so no live network
//! traffic is needed. All tests pin **current** OC behaviour; see the
//! inline gap-issue comments where the behaviour diverges from spec #537.

use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── SSE fixture strings ───────────────────────────────────────────────────────

/// A minimal Anthropic-format SSE stream with one text block and no tool calls.
const SSE_TEXT_ONLY: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{",
    "\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"text\"}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
    "data: {\"type\":\"content_block_stop\"}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},",
    "\"usage\":{\"output_tokens\":5}}\n\n",
    "data: [DONE]\n\n",
);

// ── B3: SSE parse + tool-accumulation contract ────────────────────────────────

/// B3 — two `input_json_delta` chunks are concatenated by `AnthropicToolAccumulator`.
#[test]
fn b3_anthropic_tool_accumulator_concatenates_partial_json() {
    use openclaudia::tools::{AnthropicToolAccumulator, ToolCallAccumulator};

    let mut acc = AnthropicToolAccumulator::new();
    let mut _openai_acc = ToolCallAccumulator::new();

    // Simulate the event sequence that SSE_WITH_TOOL_USE produces
    let events: &[&str] = &[
        r#"{"type":"content_block_start","content_block":{"type":"tool_use","id":"call_abc","name":"bash"}}"#,
        r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#,
        r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
        r#"{"type":"content_block_stop"}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}"#,
    ];

    for raw in events {
        let json: Value = serde_json::from_str(raw).unwrap();
        acc.process_event(&json);
    }

    assert!(
        acc.has_tool_use(),
        "accumulator must recognise tool_use stop_reason"
    );

    let calls = acc.finalize_tool_calls();
    assert_eq!(calls.len(), 1, "exactly one tool call");
    assert_eq!(calls[0].function.name, "bash");

    // Partial JSON chunks must be concatenated verbatim (no deserialise yet)
    assert!(
        calls[0].function.arguments.contains("command"),
        "concatenated JSON must contain the key"
    );
    assert!(
        calls[0].function.arguments.contains("ls"),
        "concatenated JSON must contain the value"
    );
}

/// B3 — OpenAI-format `tool_calls` deltas are accumulated across chunks.
#[test]
fn b3_openai_tool_accumulator_partial_arguments() {
    use openclaudia::tools::ToolCallAccumulator;

    let mut acc = ToolCallAccumulator::new();

    // First delta: sets the ID and function name
    let delta1: Value = serde_json::json!({
        "tool_calls": [{
            "index": 0,
            "id": "call_xyz",
            "type": "function",
            "function": { "name": "read_file", "arguments": "{\"pat" }
        }]
    });
    acc.process_delta(&delta1);

    // Second delta: appends to arguments
    let delta2: Value = serde_json::json!({
        "tool_calls": [{
            "index": 0,
            "function": { "arguments": "h\":\"/tmp\"}" }
        }]
    });
    acc.process_delta(&delta2);

    assert!(acc.has_tool_calls());
    let calls = acc.finalize();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_xyz");
    assert_eq!(calls[0].function.name, "read_file");
    assert!(calls[0].function.arguments.contains("path"));
}

// ── B1: Retry loop — pin current (broken) state ──────────────────────────────

/// B1 — OC retries exactly 3 times on 429 then fails.
///
/// CURRENT CONTRACT (broken): max_retries is hard-coded to 3.
/// Gap #592 tracks raising this to match CC's default of 10.
///
/// The retry loop runs `for attempt in 0..=max_retries` (0,1,2,3).
/// On the final attempt (attempt == 3 == max_retries), the
/// `attempt < max_retries` guard is false so OC falls through to
/// `if !resp.status().is_success()` and returns `Err("API error 429: …")`.
/// Total requests = 4 (initial + 3 retries).
#[tokio::test]
async fn b1_retry_max_is_3_pin_gap_592() {
    let server = MockServer::start().await;

    // Return 429 forever — OC exhausts 3 retries then returns API error
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_string("Too Many Requests"),
        )
        .expect(4) // OC: initial + 3 retries = 4 total (gap #592: CC does 10)
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/messages", server.uri());
    let request_body = serde_json::json!({"model": "claude-sonnet-4-6", "messages": []});
    let (tx, _rx) = std::sync::mpsc::channel();

    let result = openclaudia::pipeline::run_turn(
        &client,
        &endpoint,
        &[],
        &request_body,
        "anthropic",
        None,
        None,
        None,
        None,
        tx,
    )
    .await;

    // After exhausting 3 retries, the final 429 falls through to the
    // non-success branch: Err("API error 429: …").
    // Gap #592: CC would try 10 times and surface a CannotRetryError.
    assert!(
        result.is_err(),
        "must fail after max retries (current limit 3, gap #592)"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("API error 429") || err.contains("Max retries"),
        "error must indicate 429 exhaustion, got: {err}"
    );
}

/// B1 — 503 is in OC's current retryable set (pin this).
#[tokio::test]
async fn b1_503_is_retried() {
    let server = MockServer::start().await;

    // First call: 503 — should be retried
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "0")
                .set_body_string("Service Unavailable"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second call: 200 with a minimal SSE body
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_TEXT_ONLY),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/messages", server.uri());
    let request_body = serde_json::json!({"model": "claude-sonnet-4-6", "messages": []});
    let (tx, _rx) = std::sync::mpsc::channel();

    let result = openclaudia::pipeline::run_turn(
        &client,
        &endpoint,
        &[],
        &request_body,
        "anthropic",
        None,
        None,
        None,
        None,
        tx,
    )
    .await;

    assert!(result.is_ok(), "503 must be retried and succeed on 2nd try");
}

/// B1 — `Retry-After` header is used verbatim when present.
///
/// OC parses `Retry-After` as `u64` seconds and sleeps exactly that long.
/// Gap #596 tracks that CC adds 0–25% jitter on top; OC does not.
/// Gap #595 tracks that CC emits a typed `api_retry` event; OC emits plain text.
///
/// This test uses `retry-after: 0` to avoid any actual sleep.
#[tokio::test]
async fn b1_retry_after_header_used_verbatim_no_jitter_gap_596() {
    let server = MockServer::start().await;

    // First call returns 429 with Retry-After: 0 (no jitter to wait for)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_string("rate limited"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Second call succeeds
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_TEXT_ONLY),
        )
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/messages", server.uri());
    let request_body = serde_json::json!({"model": "claude-sonnet-4-6", "messages": []});

    // Capture StreamText events to verify no structured retry event is emitted
    // Gap #595: CC emits {type:'system', subtype:'api_retry', retry_delay_ms}
    // OC emits plain AppEvent::StreamText (current broken contract)
    let (tx, rx) = std::sync::mpsc::channel();

    let result = openclaudia::pipeline::run_turn(
        &client,
        &endpoint,
        &[],
        &request_body,
        "anthropic",
        None,
        None,
        None,
        None,
        tx,
    )
    .await;

    assert!(result.is_ok(), "must succeed after one 429 retry");

    // Drain events and check that a retry message was sent as StreamText
    // (NOT as a structured event type — gap #595)
    let events: Vec<_> = rx.try_iter().collect();
    let retry_events: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                openclaudia::tui::events::AppEvent::StreamText(s) if s.contains("Retrying")
            )
        })
        .collect();
    assert!(
        !retry_events.is_empty(),
        "OC emits plain-text retry notice (gap #595: should be typed api_retry event)"
    );
}

/// B1 — 408 is NOT retried (pin current behaviour; gap #597 tracks the fix).
#[tokio::test]
async fn b1_408_not_retried_pin_gap_597() {
    let server = MockServer::start().await;

    // 408 — OC currently does NOT retry this (gap #597: CC does)
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(408).set_body_string("Request Timeout"))
        .expect(1) // OC makes exactly 1 request — no retry on 408
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/messages", server.uri());
    let request_body = serde_json::json!({"model": "claude-sonnet-4-6", "messages": []});
    let (tx, _rx) = std::sync::mpsc::channel();

    let result = openclaudia::pipeline::run_turn(
        &client,
        &endpoint,
        &[],
        &request_body,
        "anthropic",
        None,
        None,
        None,
        None,
        tx,
    )
    .await;

    // OC returns API error immediately for 408 — no retry
    assert!(result.is_err(), "408 must not be retried (gap #597)");
    let err = result.unwrap_err();
    assert!(
        err.contains("API error 408"),
        "error must include status code, got: {err}"
    );
}

// ── B3: process_sse_event pure-function contract ──────────────────────────────

/// B3 — `process_sse_event` returns `SseAction::ThinkingStart` on thinking block.
#[test]
fn b3_process_sse_event_thinking_start() {
    use openclaudia::pipeline::{process_sse_event, SseAction};
    use openclaudia::tools::{AnthropicToolAccumulator, ToolCallAccumulator};

    let event: Value = serde_json::json!({
        "type": "content_block_start",
        "content_block": { "type": "thinking" }
    });
    let mut ant = AnthropicToolAccumulator::new();
    let mut oai = ToolCallAccumulator::new();

    let action = process_sse_event(&event, false, &mut ant, &mut oai);
    assert!(
        matches!(action, SseAction::ThinkingStart),
        "must return ThinkingStart for thinking content_block_start"
    );
}

/// B3 — `process_sse_event` returns `SseAction::ThinkingEnd` when in a thinking block.
#[test]
fn b3_process_sse_event_thinking_end() {
    use openclaudia::pipeline::{process_sse_event, SseAction};
    use openclaudia::tools::{AnthropicToolAccumulator, ToolCallAccumulator};

    let event: Value = serde_json::json!({"type": "content_block_stop"});
    let mut ant = AnthropicToolAccumulator::new();
    let mut oai = ToolCallAccumulator::new();

    // `in_thinking_block = true` is the gate
    let action = process_sse_event(&event, true, &mut ant, &mut oai);
    assert!(
        matches!(action, SseAction::ThinkingEnd),
        "content_block_stop while in thinking block must return ThinkingEnd"
    );
}

/// B3 — `process_sse_event` returns `SseAction::Thinking` for delta inside thinking block.
#[test]
fn b3_process_sse_event_thinking_delta() {
    use openclaudia::pipeline::{process_sse_event, SseAction};
    use openclaudia::tools::{AnthropicToolAccumulator, ToolCallAccumulator};

    let event: Value = serde_json::json!({
        "type": "content_block_delta",
        "delta": { "thinking": "pondering..." }
    });
    let mut ant = AnthropicToolAccumulator::new();
    let mut oai = ToolCallAccumulator::new();

    let action = process_sse_event(&event, true, &mut ant, &mut oai);
    match action {
        SseAction::Thinking(text) => assert_eq!(text, "pondering..."),
        other => panic!("expected SseAction::Thinking, got {other:?}"),
    }
}

/// B3 — OpenAI-format `choices[0].delta.content` returns `SseAction::Text`.
#[test]
fn b3_process_sse_event_openai_text_delta() {
    use openclaudia::pipeline::{process_sse_event, SseAction};
    use openclaudia::tools::{AnthropicToolAccumulator, ToolCallAccumulator};

    let event: Value = serde_json::json!({
        "choices": [{ "delta": { "content": "world" } }]
    });
    let mut ant = AnthropicToolAccumulator::new();
    let mut oai = ToolCallAccumulator::new();

    let action = process_sse_event(&event, false, &mut ant, &mut oai);
    match action {
        SseAction::Text(t) => assert_eq!(t, "world"),
        other => panic!("expected SseAction::Text, got {other:?}"),
    }
}
