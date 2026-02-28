use reqwest::multipart::Form;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

const MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_SECS: u64 = 5;

/// Errors from the Slack Web API.
#[derive(Debug)]
pub enum SlackApiError {
    /// HTTP 429 — caller should not retry; the client already retried internally.
    RateLimited { retry_after: Duration },
    /// `ok: false` with an auth-class error code.
    AuthError { error: String },
    /// `ok: false` with any other error code.
    ApiError { method: String, error: String },
    /// Transport / DNS / TLS failure.
    Network(reqwest::Error),
}

impl std::fmt::Display for SlackApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { retry_after } => {
                write!(f, "rate limited (retry after {}s)", retry_after.as_secs())
            }
            Self::AuthError { error } => write!(f, "auth error: {error}"),
            Self::ApiError { method, error } => write!(f, "{method}: {error}"),
            Self::Network(e) => write!(f, "network: {e}"),
        }
    }
}

impl std::error::Error for SlackApiError {}

const AUTH_ERRORS: &[&str] = &[
    "invalid_auth",
    "token_revoked",
    "not_authed",
    "account_inactive",
];

/// Shared Slack Web API client with retry-on-429 and envelope parsing.
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
    #[allow(dead_code)]
    app_token: String,
    base_url: String,
}

impl SlackClient {
    /// Production constructor — base URL defaults to `https://slack.com`.
    pub fn new(bot_token: String, app_token: String) -> Self {
        Self::new_with_base_url(bot_token, app_token, "https://slack.com".into())
    }

    /// Test constructor — caller supplies a wiremock base URL.
    pub fn new_with_base_url(bot_token: String, app_token: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            bot_token,
            app_token,
            base_url,
        }
    }

    /// POST `{base_url}/api/{method}` with JSON body and bearer auth.
    pub async fn api_post(
        &self,
        method: &str,
        body: &Value,
    ) -> Result<Value, SlackApiError> {
        let url = format!("{}/api/{}", self.base_url, method);
        let mut retries = 0u32;

        loop {
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.bot_token)
                .json(body)
                .send()
                .await
                .map_err(SlackApiError::Network)?;

            if resp.status() == 429 {
                if retries >= MAX_RETRIES {
                    let retry_after = parse_retry_after(&resp);
                    return Err(SlackApiError::RateLimited { retry_after });
                }
                let wait = parse_retry_after(&resp);
                debug!(method, ?wait, retries, "slack rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            let json: Value = resp.json().await.map_err(SlackApiError::Network)?;
            return parse_envelope(method, json);
        }
    }

    /// GET `{base_url}/api/{method}` with query params and bearer auth.
    pub async fn api_get(
        &self,
        method: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, SlackApiError> {
        let url = format!("{}/api/{}", self.base_url, method);
        let mut retries = 0u32;

        loop {
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&self.bot_token)
                .query(params)
                .send()
                .await
                .map_err(SlackApiError::Network)?;

            if resp.status() == 429 {
                if retries >= MAX_RETRIES {
                    let retry_after = parse_retry_after(&resp);
                    return Err(SlackApiError::RateLimited { retry_after });
                }
                let wait = parse_retry_after(&resp);
                debug!(method, ?wait, retries, "slack rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            let json: Value = resp.json().await.map_err(SlackApiError::Network)?;
            return parse_envelope(method, json);
        }
    }

    /// POST multipart form (for file uploads).
    pub async fn api_post_multipart(
        &self,
        method: &str,
        form: Form,
    ) -> Result<Value, SlackApiError> {
        let url = format!("{}/api/{}", self.base_url, method);
        // Multipart forms are consumed on send so we cannot retry transparently.
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.bot_token)
            .multipart(form)
            .send()
            .await
            .map_err(SlackApiError::Network)?;

        if resp.status() == 429 {
            let retry_after = parse_retry_after(&resp);
            warn!(method, ?retry_after, "slack multipart rate limited (no retry for multipart)");
            return Err(SlackApiError::RateLimited { retry_after });
        }

        let json: Value = resp.json().await.map_err(SlackApiError::Network)?;
        parse_envelope(method, json)
    }
}

/// Parse the `Retry-After` header (seconds) or fall back to default.
fn parse_retry_after(resp: &reqwest::Response) -> Duration {
    resp.headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(DEFAULT_RETRY_SECS))
}

/// Check the Slack envelope `{ "ok": bool, "error": "..." }` and return
/// the full JSON on success, or the appropriate error variant on failure.
fn parse_envelope(method: &str, json: Value) -> Result<Value, SlackApiError> {
    let ok = json.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        return Ok(json);
    }

    let error = json
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    if AUTH_ERRORS.contains(&error.as_str()) {
        Err(SlackApiError::AuthError { error })
    } else {
        Err(SlackApiError::ApiError {
            method: method.to_string(),
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn api_post_success_returns_parsed_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "ts": "123"})),
            )
            .mount(&server)
            .await;

        let client =
            SlackClient::new_with_base_url("xoxb-test".into(), String::new(), server.uri());
        let result = client
            .api_post("chat.postMessage", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["ts"], "123");
    }

    #[tokio::test]
    async fn api_post_auth_error_returns_slack_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth.test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": false, "error": "invalid_auth"})),
            )
            .mount(&server)
            .await;

        let client =
            SlackClient::new_with_base_url("xoxb-bad".into(), String::new(), server.uri());
        let result = client
            .api_post("auth.test", &serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(SlackApiError::AuthError { .. })));
    }

    #[tokio::test]
    async fn api_post_rate_limited_retries_and_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(429).append_header("Retry-After", "0"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true})),
            )
            .mount(&server)
            .await;

        let client =
            SlackClient::new_with_base_url("xoxb-test".into(), String::new(), server.uri());
        let result = client
            .api_post("chat.postMessage", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn api_post_includes_bearer_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth.test"))
            .and(header("Authorization", "Bearer xoxb-my-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true})),
            )
            .mount(&server)
            .await;

        let client =
            SlackClient::new_with_base_url("xoxb-my-token".into(), String::new(), server.uri());
        let result = client
            .api_post("auth.test", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn api_get_success_returns_parsed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/users.getPresence"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"ok": true, "presence": "active"}),
                ),
            )
            .mount(&server)
            .await;

        let client =
            SlackClient::new_with_base_url("xoxb-test".into(), String::new(), server.uri());
        let result = client
            .api_get("users.getPresence", &[("user", "U123")])
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["presence"], "active");
    }

    #[tokio::test]
    async fn api_post_channel_not_found_returns_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"ok": false, "error": "channel_not_found"}),
                ),
            )
            .mount(&server)
            .await;

        let client =
            SlackClient::new_with_base_url("xoxb-test".into(), String::new(), server.uri());
        let result = client
            .api_post("chat.postMessage", &serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(SlackApiError::ApiError { .. })));
    }

    #[test]
    fn parse_envelope_ok_true_returns_json() {
        let json = serde_json::json!({"ok": true, "data": 42});
        let result = parse_envelope("test.method", json.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), json);
    }

    #[test]
    fn parse_envelope_ok_false_auth_error() {
        for code in AUTH_ERRORS {
            let json = serde_json::json!({"ok": false, "error": code});
            let result = parse_envelope("test.method", json);
            assert!(
                matches!(&result, Err(SlackApiError::AuthError { .. })),
                "expected AuthError for {code}"
            );
        }
    }

    #[test]
    fn parse_envelope_ok_false_api_error() {
        let json = serde_json::json!({"ok": false, "error": "channel_not_found"});
        let result = parse_envelope("chat.postMessage", json);
        match result {
            Err(SlackApiError::ApiError { method, error }) => {
                assert_eq!(method, "chat.postMessage");
                assert_eq!(error, "channel_not_found");
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    #[test]
    fn slack_api_error_display() {
        let e = SlackApiError::AuthError {
            error: "invalid_auth".into(),
        };
        assert!(e.to_string().contains("invalid_auth"));

        let e = SlackApiError::ApiError {
            method: "chat.postMessage".into(),
            error: "channel_not_found".into(),
        };
        assert!(e.to_string().contains("channel_not_found"));

        let e = SlackApiError::RateLimited {
            retry_after: Duration::from_secs(30),
        };
        assert!(e.to_string().contains("30"));
    }
}
