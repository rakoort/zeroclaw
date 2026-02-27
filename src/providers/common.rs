//! Common utilities shared across provider implementations.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};

/// Map an HTTP error from a provider API to a sanitized anyhow Error.
///
/// Delegates to [`super::sanitize_api_error`] for secret scrubbing and truncation.
pub fn map_api_error(provider: &str, status: u16, body: &str) -> anyhow::Error {
    let sanitized = super::sanitize_api_error(body);
    anyhow::anyhow!("{provider} API error ({status}): {sanitized}")
}

/// Trait for estimating token counts. Each provider can supply an
/// accurate implementation; the default uses a rough chars/4 heuristic.
pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, text: &str) -> usize;
}

/// Rough heuristic: ~4 characters per token. Suitable as a fallback
/// when no provider-specific estimator is available.
pub struct DefaultTokenEstimator;

impl TokenEstimator for DefaultTokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        text.len().div_ceil(4)
    }
}

/// Standard request headers for JSON provider APIs.
pub fn standard_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static("zeroclaw/1.0"));
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_api_error_includes_status_and_sanitized_body() {
        let err = map_api_error("anthropic", 429, "rate limited sk-secret123-abc");
        let msg = format!("{err}");
        assert!(msg.contains("429"));
        assert!(msg.contains("anthropic"));
        // secret must be scrubbed
        assert!(!msg.contains("sk-secret123-abc"));
    }

    #[test]
    fn standard_headers_include_required_headers() {
        let headers = standard_headers();
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
        assert_eq!(headers.get("accept").unwrap(), "application/json");
        assert_eq!(headers.get("user-agent").unwrap(), "zeroclaw/1.0");
    }

    #[test]
    fn default_estimator_approximates_chars_div_4() {
        let estimator = DefaultTokenEstimator;
        // 11 bytes / 4 = 2.75, rounded up = 3
        assert_eq!(estimator.estimate("hello world"), 3);
    }

    #[test]
    fn default_estimator_empty_string() {
        let estimator = DefaultTokenEstimator;
        assert_eq!(estimator.estimate(""), 0);
    }
}
