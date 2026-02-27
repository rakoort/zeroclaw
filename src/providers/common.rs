//! Common utilities shared across provider implementations.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};

/// Map an HTTP error from a provider API to a sanitized anyhow Error.
///
/// Delegates to [`super::sanitize_api_error`] for secret scrubbing and truncation.
pub fn map_api_error(provider: &str, status: u16, body: &str) -> anyhow::Error {
    let sanitized = super::sanitize_api_error(body);
    anyhow::anyhow!("{provider} API error ({status}): {sanitized}")
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
    fn standard_headers_include_content_type() {
        let headers = standard_headers();
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
    }
}
