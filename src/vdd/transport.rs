//! HTTP transport for the VDD loop: adversary + builder request plumbing.

use reqwest::Client;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::{AppConfig, ProviderConfig, VddConfig};
use crate::providers::{get_adapter, ApiKey};
use crate::proxy::ChatCompletionRequest;
use crate::session::TokenUsage;

use crate::vdd::error::VddError;
use crate::vdd::helpers::truncate_output;
use crate::vdd::parsing::{extract_response_text, extract_token_usage};

/// Forward a request to a provider and return the raw reqwest response.
///
/// URL composition is entirely delegated to the adapter via `endpoint`
/// (the return value of `ProviderAdapter::chat_endpoint`), so provider-specific
/// path conventions (e.g. Google's `/v1beta/models/{model}:generateContent`)
/// are handled in the adapter, not here.
pub async fn forward_request(
    client: &Client,
    provider: &ProviderConfig,
    endpoint: &str,
    body: &Value,
    headers: Vec<(String, String)>,
) -> Result<reqwest::Response, reqwest::Error> {
    let base_url = provider
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');

    // endpoint already encodes the full provider-specific path, including
    // any model name or version segment (e.g. Google's v1beta path).
    let url = format!("{base_url}{endpoint}");

    // Validate the constructed URL before sending the request
    if let Err(e) = reqwest::Url::parse(&url) {
        warn!("VDD: Invalid provider URL '{}': {}", url, e);
    }

    debug!("VDD: Sending request to {}", url);

    let mut req = client.post(&url).json(body);
    for (key, value) in headers {
        req = req.header(key.as_str(), value.as_str());
    }
    for (key, value) in &provider.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    req.send().await
}

/// Send a request to the adversary provider. Returns (`response_text`, `token_usage`).
pub async fn send_to_adversary(
    client: &Client,
    config: &VddConfig,
    app_config: &AppConfig,
    request: &ChatCompletionRequest,
) -> Result<(String, TokenUsage), VddError> {
    let provider_config = app_config
        .providers
        .get(&config.adversary.provider)
        .ok_or_else(|| {
            VddError::ConfigError(format!(
                "Adversary provider '{}' not configured in providers section",
                config.adversary.provider
            ))
        })?;

    let api_key = config
        .adversary
        .api_key
        .as_ref()
        .or(provider_config.api_key.as_ref())
        .ok_or_else(|| {
            VddError::ConfigError(format!(
                "No API key for adversary provider '{}'",
                config.adversary.provider
            ))
        })?;

    let adapter = get_adapter(&config.adversary.provider);
    let transformed = adapter
        .transform_request(request)
        .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

    let headers = adapter.get_headers(api_key);
    let endpoint = adapter.chat_endpoint(&request.model);

    // Per-request timeout — guards against a hung adversary blocking
    // the whole VDD loop. See crosslink #496.
    let timeout_secs = config.adversary.request_timeout_seconds;
    let timeout = std::time::Duration::from_secs(timeout_secs);

    let response = tokio::time::timeout(
        timeout,
        forward_request(client, provider_config, &endpoint, &transformed, headers),
    )
    .await
    .map_err(|_| {
        VddError::AdversaryRequestFailed(format!(
            "adversary request timed out after {timeout_secs}s"
        ))
    })?
    .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

    // Same timeout wraps the body-read to prevent a slow-drip
    // payload from exceeding the total budget.
    let response_json: Value = tokio::time::timeout(timeout, response.json())
        .await
        .map_err(|_| {
            VddError::AdversaryRequestFailed(format!(
                "adversary response body read timed out after {timeout_secs}s"
            ))
        })?
        .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

    let text = extract_response_text(&response_json);
    let tokens = extract_token_usage(&response_json);

    // Always log at INFO level for debugging, truncated
    info!(
        response_length = text.len(),
        "VDD: Received adversary response ({} chars)",
        text.len()
    );

    if config.tracking.log_adversary_responses {
        // Log first 1000 chars to see what we're getting
        info!(
            "VDD: Adversary response preview: {}",
            truncate_output(&text, 1000)
        );
    }

    Ok((text, tokens))
}

/// Send a revision request back to the builder provider.
pub async fn send_to_builder(
    client: &Client,
    app_config: &AppConfig,
    request: &ChatCompletionRequest,
    provider_name: &str,
    api_key: Option<&ApiKey>,
) -> Result<(String, Value, TokenUsage), VddError> {
    let provider_config = app_config.providers.get(provider_name).ok_or_else(|| {
        VddError::BuilderRevisionFailed(format!(
            "Builder provider '{provider_name}' not configured"
        ))
    })?;

    let adapter = get_adapter(provider_name);
    let transformed = adapter
        .transform_request(request)
        .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

    let headers = api_key.map(|k| adapter.get_headers(k)).unwrap_or_default();
    let endpoint = adapter.chat_endpoint(&request.model);

    let response = forward_request(client, provider_config, &endpoint, &transformed, headers)
        .await
        .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

    let response_json: Value = response
        .json()
        .await
        .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

    let text = extract_response_text(&response_json);
    let tokens = extract_token_usage(&response_json);

    Ok((text, response_json, tokens))
}

/// Send a verification request through the builder's provider.
/// Reuses the same HTTP plumbing as `send_to_builder` but with a
/// simpler interface (no revision response needed).
pub async fn send_to_builder_for_verification(
    client: &Client,
    app_config: &AppConfig,
    request: &ChatCompletionRequest,
    provider_name: &str,
    api_key: Option<&ApiKey>,
) -> Result<(String, TokenUsage), VddError> {
    let provider_config = app_config.providers.get(provider_name).ok_or_else(|| {
        VddError::ConfigError(format!(
            "Builder provider '{provider_name}' not configured — \
             cannot run verification agent"
        ))
    })?;

    let adapter = get_adapter(provider_name);
    let transformed = adapter
        .transform_request(request)
        .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier transform: {e}")))?;

    let headers = api_key.map(|k| adapter.get_headers(k)).unwrap_or_default();
    let endpoint = adapter.chat_endpoint(&request.model);

    let response = forward_request(client, provider_config, &endpoint, &transformed, headers)
        .await
        .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier request: {e}")))?;

    let response_json: Value = response
        .json()
        .await
        .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier response: {e}")))?;

    let text = extract_response_text(&response_json);
    let tokens = extract_token_usage(&response_json);

    Ok((text, tokens))
}
