use serde::Deserialize;

/// Proxy server configuration
#[derive(Debug, Deserialize, Clone)]
pub struct ProxyConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_target")]
    pub target: String,
    /// Maximum bytes read from an upstream response body before aborting.
    ///
    /// Guards against memory-exhaustion `DoS` from malicious or buggy
    /// upstreams that stream gigabytes of data. Default: 50 MiB — enough
    /// for any legitimate LLM response including thinking + tool-use
    /// tokens, two orders of magnitude below a typical attack threshold.
    /// See crosslink #352.
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
}

pub const fn default_port() -> u16 {
    8080
}

/// Default bind address for the proxy server. Loopback by default —
/// users who need to bind 0.0.0.0 must do so explicitly via config.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Default upstream provider when none is configured.
pub const DEFAULT_TARGET: &str = "anthropic";

pub fn default_host() -> String {
    DEFAULT_HOST.to_string()
}

pub fn default_target() -> String {
    DEFAULT_TARGET.to_string()
}

pub const fn default_max_response_bytes() -> usize {
    50 * 1024 * 1024
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            target: default_target(),
            max_response_bytes: default_max_response_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        // This test verifies defaults work without any config files
        let config = ProxyConfig::default();
        assert_eq!(config.port, 8080);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.target, "anthropic");
        assert_eq!(config.max_response_bytes, 50 * 1024 * 1024);
    }

    #[test]
    fn test_proxy_config_default_values() {
        let config = ProxyConfig::default();
        assert_eq!(config.port, default_port());
        assert_eq!(config.host, default_host());
        assert_eq!(config.target, default_target());
        assert_eq!(config.max_response_bytes, default_max_response_bytes());
    }
}
