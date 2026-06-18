//! One-shot print mode for non-interactive use.
//!
//! This path intentionally does not reuse the legacy REPL loop: it sends one
//! prompt, prints assistant text to stdout, and exits. Request shaping still
//! goes through provider adapters so provider-specific envelopes stay aligned
//! with the proxy and REPL paths.

use futures::StreamExt;
use openclaudia::providers::ProviderAdapter;
use reqwest::header::CONTENT_TYPE;
use std::io::Write as _;

use crate::{resolve_chat_auth, resolve_model_name, ChatAuth};

/// Arguments for [`cmd_print`].
pub struct PrintOptions {
    pub model_override: Option<String>,
    pub target_override: Option<String>,
    pub prompt: String,
}

struct PrintSseState {
    anthropic_accumulator: openclaudia::tools::AnthropicToolAccumulator,
    tool_accumulator: openclaudia::tools::ToolCallAccumulator,
    in_thinking_block: bool,
}

impl PrintSseState {
    const fn new() -> Self {
        Self {
            anthropic_accumulator: openclaudia::tools::AnthropicToolAccumulator::new(),
            tool_accumulator: openclaudia::tools::ToolCallAccumulator::new(),
            in_thinking_block: false,
        }
    }
}

fn load_print_config(
    model_override: Option<&str>,
    target_override: Option<&str>,
) -> anyhow::Result<openclaudia::config::AppConfig> {
    let mut config = openclaudia::config::load_config().map_err(|e| {
        if openclaudia::config::config_file_exists() {
            eprintln!("Failed to parse configuration: {e}");
            anyhow::anyhow!("invalid configuration: {e}")
        } else {
            eprintln!("No configuration found. Run 'openclaudia init' first.");
            anyhow::anyhow!("no configuration found")
        }
    })?;

    if let Some(target) = target_override {
        config.proxy.target = target.to_string();
    } else if let Some(model) = model_override {
        let detected = openclaudia::proxy::determine_provider(model, &config);
        if detected != config.proxy.target {
            config.proxy.target = detected;
        }
    }

    Ok(config)
}

fn build_print_request(
    adapter: &dyn ProviderAdapter,
    model: &str,
    prompt: String,
    thinking: &openclaudia::config::ThinkingConfig,
    claude_code_token: Option<&str>,
) -> Result<serde_json::Value, String> {
    let request = openclaudia::proxy::ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![openclaudia::proxy::ChatMessage {
            role: "user".to_string(),
            content: openclaudia::proxy::MessageContent::Text(prompt),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            extra: std::collections::HashMap::new(),
        }],
        temperature: None,
        max_tokens: Some(openclaudia::DEFAULT_MAX_TOKENS),
        stream: Some(adapter.name() != "google"),
        tools: None,
        tool_choice: None,
        extra: std::collections::HashMap::new(),
    };

    let mut body = adapter
        .transform_request_with_thinking(&request, thinking)
        .map_err(|e| format!("request transform error: {e}"))?;
    if claude_code_token.is_some() {
        openclaudia::claude_credentials::inject_system_prompt(&mut body);
    }
    Ok(body)
}

fn resolve_print_endpoint(
    model: &str,
    provider: &openclaudia::config::ProviderConfig,
    adapter: &dyn ProviderAdapter,
    claude_code_token: Option<&str>,
) -> String {
    if claude_code_token.is_some() {
        return openclaudia::claude_credentials::get_oauth_endpoint(model);
    }

    let path = if adapter.name() == "google" {
        adapter.chat_endpoint(model)
    } else {
        adapter
            .stream_endpoint(model)
            .unwrap_or_else(|| adapter.chat_endpoint(model))
    };
    format!(
        "{}{}",
        openclaudia::proxy::normalize_base_url(&provider.base_url),
        path
    )
}

fn sse_data_from_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') {
        return None;
    }
    trimmed.strip_prefix("data:").map(str::trim_start)
}

fn extract_print_sse_text(json: &serde_json::Value, state: &mut PrintSseState) -> Option<String> {
    match openclaudia::pipeline::process_sse_event(
        json,
        state.in_thinking_block,
        &mut state.anthropic_accumulator,
        &mut state.tool_accumulator,
    ) {
        openclaudia::pipeline::SseAction::Text(text) => Some(text),
        openclaudia::pipeline::SseAction::ThinkingStart => {
            state.in_thinking_block = true;
            None
        }
        openclaudia::pipeline::SseAction::ThinkingEnd => {
            state.in_thinking_block = false;
            None
        }
        openclaudia::pipeline::SseAction::Thinking(_)
        | openclaudia::pipeline::SseAction::Reasoning(_)
        | openclaudia::pipeline::SseAction::None => None,
    }
}

fn extract_print_sse_line(line: &str, state: &mut PrintSseState) -> Option<String> {
    let data = sse_data_from_line(line)?;
    if data == "[DONE]" {
        return None;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
        return None;
    };
    extract_print_sse_text(&json, state)
}

fn response_is_json(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| {
            let lower = content_type.to_ascii_lowercase();
            lower.contains("application/json") || lower.contains("+json")
        })
}

async fn print_json_response(
    response: reqwest::Response,
    adapter: &dyn ProviderAdapter,
) -> anyhow::Result<()> {
    let body = response.json::<serde_json::Value>().await?;
    let text = adapter.extract_response_text(&body).ok_or_else(|| {
        anyhow::anyhow!("provider response did not contain printable assistant text")
    })?;
    println!("{text}");
    Ok(())
}

async fn print_sse_response(response: reqwest::Response) -> anyhow::Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut state = PrintSseState::new();

    while let Some(result) = stream.next().await {
        let chunk = result?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if buffer.len() > openclaudia::proxy::MAX_SSE_LINE_BYTES {
            anyhow::bail!(
                "SSE line exceeded {} bytes without newline",
                openclaudia::proxy::MAX_SSE_LINE_BYTES
            );
        }

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].to_string();
            buffer = buffer[line_end + 1..].to_string();
            if let Some(text) = extract_print_sse_line(&line, &mut state) {
                print!("{text}");
                std::io::stdout().flush()?;
            }
        }
    }

    if !buffer.trim().is_empty() {
        if let Some(text) = extract_print_sse_line(&buffer, &mut state) {
            print!("{text}");
            std::io::stdout().flush()?;
        }
    }

    println!();
    Ok(())
}

/// Run one-shot print mode.
///
/// # Errors
///
/// Returns an error when configuration/auth cannot be resolved, the provider
/// rejects the request, or the response stream cannot be decoded.
pub async fn cmd_print(options: PrintOptions) -> anyhow::Result<()> {
    crate::chdir_to_git_root();

    let config = load_print_config(
        options.model_override.as_deref(),
        options.target_override.as_deref(),
    )?;
    let provider = config.active_provider().ok_or_else(|| {
        anyhow::anyhow!(
            "no provider configured for target '{}'",
            config.proxy.target
        )
    })?;
    let Some(ChatAuth {
        api_key,
        claude_code_token,
    }) = resolve_chat_auth(&config.proxy.target, provider).await?
    else {
        anyhow::bail!(
            "could not resolve authentication for target '{}'",
            config.proxy.target
        );
    };
    let model = resolve_model_name(
        options.model_override,
        provider.model.clone(),
        &config.proxy.target,
    );
    let adapter = openclaudia::providers::get_adapter(&config.proxy.target)?;
    let request_body = build_print_request(
        adapter,
        &model,
        options.prompt,
        &provider.thinking,
        claude_code_token.as_deref(),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    let endpoint = resolve_print_endpoint(&model, provider, adapter, claude_code_token.as_deref());
    let extra_headers: Vec<(String, String)> = provider
        .headers
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let headers = openclaudia::pipeline::resolve_headers(
        &config.proxy.target,
        api_key.as_ref(),
        claude_code_token.as_deref(),
        &extra_headers,
    )?;

    let client = reqwest::Client::new();
    let mut request = client.post(endpoint).json(&request_body);
    for (key, value) in &headers {
        request = request.header(key.as_str(), value.as_str());
    }

    let response = request.send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| String::new());
        anyhow::bail!("API error {}: {body}", status.as_u16());
    }

    if response_is_json(&response) {
        print_json_response(response, adapter).await
    } else {
        print_sse_response(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn print_sse_extracts_openai_text_delta() {
        let mut state = PrintSseState::new();
        let json = json!({"choices": [{"delta": {"content": "hello"}}]});
        assert_eq!(
            extract_print_sse_text(&json, &mut state),
            Some("hello".to_string())
        );
    }

    #[test]
    fn print_sse_extracts_anthropic_text_delta() {
        let mut state = PrintSseState::new();
        let json = json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "world"}
        });
        assert_eq!(
            extract_print_sse_text(&json, &mut state),
            Some("world".to_string())
        );
    }

    #[test]
    fn print_sse_suppresses_thinking_deltas() {
        let mut state = PrintSseState::new();
        let start = json!({
            "type": "content_block_start",
            "content_block": {"type": "thinking"}
        });
        let delta = json!({
            "type": "content_block_delta",
            "delta": {"type": "thinking_delta", "thinking": "private"}
        });
        let stop = json!({"type": "content_block_stop"});
        assert_eq!(extract_print_sse_text(&start, &mut state), None);
        assert!(state.in_thinking_block);
        assert_eq!(extract_print_sse_text(&delta, &mut state), None);
        assert_eq!(extract_print_sse_text(&stop, &mut state), None);
        assert!(!state.in_thinking_block);
    }

    #[test]
    fn print_sse_suppresses_openai_reasoning_delta() {
        let mut state = PrintSseState::new();
        let json = json!({"choices": [{"delta": {"reasoning_content": "private"}}]});
        assert_eq!(extract_print_sse_text(&json, &mut state), None);
    }

    #[test]
    fn print_request_has_no_tools_and_streams_non_google() {
        let adapter = openclaudia::providers::get_adapter("openai").unwrap();
        let body = build_print_request(
            adapter,
            "gpt-5.5",
            "hi".to_string(),
            &openclaudia::config::ThinkingConfig::default(),
            None,
        )
        .unwrap();
        assert_eq!(body["stream"], true);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn print_request_applies_openai_reasoning_effort() {
        let adapter = openclaudia::providers::get_adapter("openai").unwrap();
        let thinking = openclaudia::config::ThinkingConfig {
            reasoning_effort: Some("xhigh".to_string()),
            ..Default::default()
        };

        let body =
            build_print_request(adapter, "gpt-5.5", "hi".to_string(), &thinking, None).unwrap();

        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn print_request_applies_google_thinking_budget() {
        let adapter = openclaudia::providers::get_adapter("google").unwrap();
        let thinking = openclaudia::config::ThinkingConfig {
            budget_tokens: Some(7777),
            ..openclaudia::config::ThinkingConfig::default()
        };

        let body = build_print_request(
            adapter,
            "gemini-3.5-flash",
            "hi".to_string(),
            &thinking,
            None,
        )
        .unwrap();

        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            7777
        );
    }

    #[test]
    fn print_endpoint_uses_google_json_endpoint() {
        let adapter = openclaudia::providers::get_adapter("google").unwrap();
        let provider = openclaudia::config::ProviderConfig {
            api_key: None,
            base_url: "https://generativelanguage.googleapis.com".to_string(),
            model: None,
            headers: std::collections::HashMap::new(),
            thinking: openclaudia::config::ThinkingConfig::default(),
        };
        let endpoint = resolve_print_endpoint("gemini-3.5-flash", &provider, adapter, None);
        assert!(endpoint.ends_with("/v1beta/models/gemini-3.5-flash:generateContent"));
        assert!(!endpoint.contains("streamGenerateContent"));
    }
}
