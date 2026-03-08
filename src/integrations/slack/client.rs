use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

use crate::observability::traits::{Observer, ObserverEvent};

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
    observer: Arc<dyn Observer>,
}

impl SlackClient {
    /// Production constructor — base URL defaults to `https://slack.com`.
    pub fn new(bot_token: String, app_token: String, observer: Arc<dyn Observer>) -> Self {
        Self::new_with_base_url(bot_token, app_token, "https://slack.com".into(), observer)
    }

    /// Test constructor — caller supplies a wiremock base URL.
    pub fn new_with_base_url(
        bot_token: String,
        app_token: String,
        base_url: String,
        observer: Arc<dyn Observer>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            bot_token,
            app_token,
            base_url,
            observer,
        }
    }

    /// POST `{base_url}/api/{method}` with JSON body and bearer auth.
    pub async fn api_post(&self, method_name: &str, body: &Value) -> Result<Value, SlackApiError> {
        let url = format!("{}/api/{}", self.base_url, method_name);
        let mut retries = 0u32;
        let call_start = std::time::Instant::now();
        let mut total_rate_limit_wait_ms: u64 = 0;
        let mut last_status: Option<u16> = None;

        let result: Result<Value, SlackApiError> = loop {
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.bot_token)
                .json(body)
                .send()
                .await
                .map_err(SlackApiError::Network)?;

            last_status = Some(resp.status().as_u16());

            if resp.status() == 429 {
                if retries >= MAX_RETRIES {
                    let retry_after = parse_retry_after(&resp);
                    break Err(SlackApiError::RateLimited { retry_after });
                }
                let wait = parse_retry_after(&resp);
                total_rate_limit_wait_ms += wait.as_millis() as u64;
                debug!(method_name, ?wait, retries, "slack rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            let json: Value = resp.json().await.map_err(SlackApiError::Network)?;
            break parse_envelope(method_name, json);
        };

        let duration_ms = call_start.elapsed().as_millis() as u64;
        let success = result.is_ok();
        let error = result.as_ref().err().map(|e| e.to_string());
        let response_size_bytes = result
            .as_ref()
            .ok()
            .and_then(|v| serde_json::to_string(v).ok())
            .map(|s| s.len() as u64);

        self.observer.record_event(&ObserverEvent::IntegrationApiCall {
            integration: "slack".into(),
            method: method_name.to_string(),
            success,
            duration_ms,
            error,
            retries,
            status_code: last_status,
            response_size_bytes,
            rate_limit_wait_ms: if total_rate_limit_wait_ms > 0 {
                Some(total_rate_limit_wait_ms)
            } else {
                None
            },
        });

        result
    }

    /// GET `{base_url}/api/{method}` with query params and bearer auth.
    pub async fn api_get(
        &self,
        method_name: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, SlackApiError> {
        let url = format!("{}/api/{}", self.base_url, method_name);
        let mut retries = 0u32;
        let call_start = std::time::Instant::now();
        let mut total_rate_limit_wait_ms: u64 = 0;
        let mut last_status: Option<u16> = None;

        let result: Result<Value, SlackApiError> = loop {
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&self.bot_token)
                .query(params)
                .send()
                .await
                .map_err(SlackApiError::Network)?;

            last_status = Some(resp.status().as_u16());

            if resp.status() == 429 {
                if retries >= MAX_RETRIES {
                    let retry_after = parse_retry_after(&resp);
                    break Err(SlackApiError::RateLimited { retry_after });
                }
                let wait = parse_retry_after(&resp);
                total_rate_limit_wait_ms += wait.as_millis() as u64;
                debug!(method_name, ?wait, retries, "slack rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            let json: Value = resp.json().await.map_err(SlackApiError::Network)?;
            break parse_envelope(method_name, json);
        };

        let duration_ms = call_start.elapsed().as_millis() as u64;
        let success = result.is_ok();
        let error = result.as_ref().err().map(|e| e.to_string());
        let response_size_bytes = result
            .as_ref()
            .ok()
            .and_then(|v| serde_json::to_string(v).ok())
            .map(|s| s.len() as u64);

        self.observer.record_event(&ObserverEvent::IntegrationApiCall {
            integration: "slack".into(),
            method: method_name.to_string(),
            success,
            duration_ms,
            error,
            retries,
            status_code: last_status,
            response_size_bytes,
            rate_limit_wait_ms: if total_rate_limit_wait_ms > 0 {
                Some(total_rate_limit_wait_ms)
            } else {
                None
            },
        });

        result
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
    use crate::observability::noop::NoopObserver;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client(server: &MockServer) -> SlackClient {
        SlackClient::new_with_base_url(
            "xoxb-test".into(),
            String::new(),
            server.uri(),
            Arc::new(NoopObserver),
        )
    }

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

        let client = test_client(&server);
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

        let client = SlackClient::new_with_base_url(
            "xoxb-bad".into(),
            String::new(),
            server.uri(),
            Arc::new(NoopObserver),
        );
        let result = client.api_post("auth.test", &serde_json::json!({})).await;
        assert!(matches!(result, Err(SlackApiError::AuthError { .. })));
    }

    #[tokio::test]
    async fn api_post_rate_limited_retries_and_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(429).append_header("Retry-After", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = test_client(&server);
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = SlackClient::new_with_base_url(
            "xoxb-my-token".into(),
            String::new(),
            server.uri(),
            Arc::new(NoopObserver),
        );
        let result = client.api_post("auth.test", &serde_json::json!({})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn api_get_success_returns_parsed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/users.getPresence"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "presence": "active"})),
            )
            .mount(&server)
            .await;

        let client = test_client(&server);
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
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": false, "error": "channel_not_found"})),
            )
            .mount(&server)
            .await;

        let client = test_client(&server);
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

    #[tokio::test]
    async fn api_post_emits_integration_api_call_event_on_success() {
        use crate::observability::traits::ObserverMetric;
        use parking_lot::Mutex;

        #[derive(Default)]
        struct CapturingObserver {
            events: Mutex<Vec<String>>,
        }
        impl Observer for CapturingObserver {
            fn record_event(&self, event: &ObserverEvent) {
                if let ObserverEvent::IntegrationApiCall {
                    integration,
                    method,
                    success,
                    status_code,
                    ..
                } = event
                {
                    self.events.lock().push(format!(
                        "{integration}:{method}:success={success}:status={status_code:?}"
                    ));
                }
            }
            fn record_metric(&self, _: &ObserverMetric) {}
            fn name(&self) -> &str {
                "capturing"
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "ts": "123"})),
            )
            .mount(&server)
            .await;

        let observer = Arc::new(CapturingObserver::default());
        let client = SlackClient::new_with_base_url(
            "xoxb-test".into(),
            String::new(),
            server.uri(),
            observer.clone() as Arc<dyn Observer>,
        );

        let result = client
            .api_post("chat.postMessage", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());

        let events = observer.events.lock();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("slack:chat.postMessage:success=true"));
    }

    #[tokio::test]
    async fn api_get_emits_integration_api_call_event_on_success() {
        use crate::observability::traits::ObserverMetric;
        use parking_lot::Mutex;

        #[derive(Default)]
        struct CapturingObserver {
            events: Mutex<Vec<String>>,
        }
        impl Observer for CapturingObserver {
            fn record_event(&self, event: &ObserverEvent) {
                if let ObserverEvent::IntegrationApiCall {
                    integration,
                    method,
                    success,
                    ..
                } = event
                {
                    self.events.lock().push(format!(
                        "{integration}:{method}:success={success}"
                    ));
                }
            }
            fn record_metric(&self, _: &ObserverMetric) {}
            fn name(&self) -> &str {
                "capturing"
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/users.getPresence"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "presence": "active"})),
            )
            .mount(&server)
            .await;

        let observer = Arc::new(CapturingObserver::default());
        let client = SlackClient::new_with_base_url(
            "xoxb-test".into(),
            String::new(),
            server.uri(),
            observer.clone() as Arc<dyn Observer>,
        );

        let result = client
            .api_get("users.getPresence", &[("user", "U123")])
            .await;
        assert!(result.is_ok());

        let events = observer.events.lock();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("slack:users.getPresence:success=true"));
    }
}
