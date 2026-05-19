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
///
/// Per-request timeout — crosslink #496 — wraps both the HTTP send and the
/// body read in `tokio::time::timeout` so a hung adversary cannot block the
/// VDD loop indefinitely. The timeout is configurable via
/// `vdd.adversary.request_timeout_seconds` (default 120 s).
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
    let provider_name = adapter.name().to_string();

    let response = tokio::time::timeout(
        timeout,
        forward_request(client, provider_config, &endpoint, &transformed, headers),
    )
    .await
    .map_err(|_| VddError::Timeout {
        provider: provider_name.clone(),
        elapsed_secs: timeout_secs,
    })?
    .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

    // Same timeout wraps the body-read to prevent a slow-drip
    // payload from exceeding the total budget.
    let response_json: Value = tokio::time::timeout(timeout, response.json())
        .await
        .map_err(|_| VddError::Timeout {
            provider: provider_name.clone(),
            elapsed_secs: timeout_secs,
        })?
        .map_err(|e| VddError::AdversaryRequestFailed(e.to_string()))?;

    // Crosslink #479: route extraction through the ProviderAdapter trait
    // so provider-specific response shapes (Gemini, Ollama, Anthropic) are
    // handled the same way they are on the main proxy path. The previous
    // free functions silently returned an empty string / zero tokens for
    // any provider whose response shape they did not hardcode.
    let text = adapter
        .extract_response_text(&response_json)
        .unwrap_or_default();
    let tokens = adapter
        .extract_token_usage(&response_json)
        .unwrap_or_default();

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
///
/// Symmetric per-request timeout for the builder (crosslink #496). The
/// builder revision call sits inside the same blocking-loop iteration as
/// the adversary call, so a hung builder would block the loop just as
/// badly. The timeout reuses the adversary's configured value for
/// simplicity — they're the same upper bound on how long any single
/// HTTP round-trip in the loop is allowed to take.
pub async fn send_to_builder(
    client: &Client,
    config: &VddConfig,
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

    let timeout_secs = config.adversary.request_timeout_seconds;
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let pname = provider_name.to_string();

    let response = tokio::time::timeout(
        timeout,
        forward_request(client, provider_config, &endpoint, &transformed, headers),
    )
    .await
    .map_err(|_| VddError::Timeout {
        provider: pname.clone(),
        elapsed_secs: timeout_secs,
    })?
    .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

    let response_json: Value = tokio::time::timeout(timeout, response.json())
        .await
        .map_err(|_| VddError::Timeout {
            provider: pname,
            elapsed_secs: timeout_secs,
        })?
        .map_err(|e| VddError::BuilderRevisionFailed(e.to_string()))?;

    // Crosslink #479: trait dispatch instead of hardcoded shape matching.
    let text = adapter
        .extract_response_text(&response_json)
        .unwrap_or_default();
    let tokens = adapter
        .extract_token_usage(&response_json)
        .unwrap_or_default();

    Ok((text, response_json, tokens))
}

/// Send a verification request through the builder's provider.
/// Reuses the same HTTP plumbing as `send_to_builder` but with a
/// simpler interface (no revision response needed).
pub async fn send_to_builder_for_verification(
    client: &Client,
    config: &VddConfig,
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

    let timeout_secs = config.adversary.request_timeout_seconds;
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let pname = provider_name.to_string();

    let response = tokio::time::timeout(
        timeout,
        forward_request(client, provider_config, &endpoint, &transformed, headers),
    )
    .await
    .map_err(|_| VddError::Timeout {
        provider: pname.clone(),
        elapsed_secs: timeout_secs,
    })?
    .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier request: {e}")))?;

    let response_json: Value = tokio::time::timeout(timeout, response.json())
        .await
        .map_err(|_| VddError::Timeout {
            provider: pname,
            elapsed_secs: timeout_secs,
        })?
        .map_err(|e| VddError::AdversaryRequestFailed(format!("verifier response: {e}")))?;

    // Crosslink #479: trait dispatch instead of hardcoded shape matching.
    let text = adapter
        .extract_response_text(&response_json)
        .unwrap_or_default();
    let tokens = adapter
        .extract_token_usage(&response_json)
        .unwrap_or_default();

    Ok((text, tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        GuardrailsConfig, HooksConfig, KeybindingsConfig, PermissionsConfig, ProviderConfig,
        ProxyConfig, SessionConfig, ThinkingConfig, VddAdversaryConfig, VddConfig,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    fn cfg_with_timeout(secs: u64) -> VddConfig {
        VddConfig {
            enabled: true,
            adversary: VddAdversaryConfig {
                provider: "openai".to_string(),
                model: None,
                api_key: None,
                temperature: 0.3,
                max_tokens: 256,
                request_timeout_seconds: secs,
            },
            ..Default::default()
        }
    }

    fn app_cfg_with_provider(provider: &str, base_url: &str) -> AppConfig {
        let mut providers = HashMap::new();
        providers.insert(
            provider.to_string(),
            ProviderConfig {
                base_url: base_url.to_string(),
                api_key: Some(
                    crate::providers::ApiKey::try_from_string("test-key".to_string()).unwrap(),
                ),
                model: None,
                headers: HashMap::new(),
                thinking: ThinkingConfig::default(),
            },
        );
        AppConfig {
            proxy: ProxyConfig::default(),
            providers,
            hooks: HooksConfig::default(),
            session: SessionConfig::default(),
            keybindings: KeybindingsConfig::default(),
            vdd: VddConfig::default(),
            guardrails: GuardrailsConfig::default(),
            permissions: PermissionsConfig::default(),
            managed_settings_path: None,
        }
    }

    fn dummy_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: HashMap::new(),
        }
    }

    // ── Crosslink #496: VDD HTTP timeout ──────────────────────────────────
    //
    // A slow / hung adversary upstream cannot block the VDD loop
    // indefinitely. `send_to_adversary` wraps both the HTTP send and the
    // body-read in `tokio::time::timeout`; on expiry it returns
    // `VddError::Timeout { provider, elapsed_secs }`.

    /// The configured timeout value is propagated from
    /// `VddConfig.adversary.request_timeout_seconds` into the actual
    /// timeout the transport applies. We can't observe the duration
    /// directly, but we can pin that the typed config field is honoured
    /// by checking the timeout's serde default + override semantics.
    #[test]
    fn vdd_timeout_default_is_120_seconds() {
        let cfg = VddConfig::default();
        assert_eq!(cfg.adversary.request_timeout_seconds, 120);
    }

    #[test]
    fn vdd_timeout_override_is_respected_via_config() {
        let cfg = cfg_with_timeout(7);
        assert_eq!(cfg.adversary.request_timeout_seconds, 7);
    }

    /// Hit a reserved-IP "blackhole" address (`192.0.2.1` is TEST-NET-1
    /// per RFC 5737; routed-but-unreachable on every machine that
    /// honours the registry). The connect will hang past the 1 s
    /// timeout. Asserts that we get `VddError::Timeout` (the new
    /// variant) — not `AdversaryRequestFailed`.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn send_to_adversary_surfaces_timeout_variant_on_hang() {
        let cfg = cfg_with_timeout(1);
        // Use a domain that resolves but won't accept connections in 1s.
        // `192.0.2.1` is RFC 5737 test-net, guaranteed unrouted.
        let app_cfg = app_cfg_with_provider("openai", "http://192.0.2.1:81");
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let req = dummy_request();

        // Run the call and advance virtual time past the 1s budget.
        let handle =
            tokio::spawn(async move { send_to_adversary(&client, &cfg, &app_cfg, &req).await });
        // Drive paused-time forward past the configured timeout.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let result = handle.await.expect("join task");

        match result {
            Err(VddError::Timeout {
                provider,
                elapsed_secs,
            }) => {
                assert_eq!(provider, "openai");
                assert_eq!(elapsed_secs, 1);
            }
            Err(other) => panic!("expected VddError::Timeout, got {other:?}"),
            Ok(_) => panic!("expected timeout, got successful response"),
        }
    }

    /// The `VddError::Timeout` Display includes both the provider name
    /// and the elapsed seconds so the operator can see *which* upstream
    /// is hung and *how long* it has been waiting — required for
    /// triage. The previous code returned a stringly-typed
    /// `AdversaryRequestFailed("...timed out after {n}s")` which forces
    /// callers to substring-match to detect timeouts.
    #[test]
    fn vdd_timeout_error_display_has_provider_and_seconds() {
        let err = VddError::Timeout {
            provider: "google".to_string(),
            elapsed_secs: 42,
        };
        let display = err.to_string();
        assert!(display.contains("google"), "got: {display}");
        assert!(display.contains("42"), "got: {display}");
    }
}
