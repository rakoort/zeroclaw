use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::debug;

use crate::observability::traits::{Observer, ObserverEvent};

const MAX_RETRIES: u32 = 3;

/// A single GraphQL error from the Linear API.
#[derive(Debug, Clone, Deserialize)]
pub struct LinearGraphqlError {
    pub message: String,
    pub path: Option<Vec<String>>,
}

/// Errors from the Linear GraphQL API.
#[derive(Debug)]
pub enum LinearApiError {
    /// HTTP 429 — the client already retried internally.
    RateLimited { reset_at_ms: u64 },
    /// HTTP 401/403 or authentication-related GraphQL error.
    AuthError { message: String },
    /// GraphQL response contained `errors` array.
    GraphqlErrors { errors: Vec<LinearGraphqlError> },
    /// Transport / DNS / TLS failure.
    Network(reqwest::Error),
}

impl std::fmt::Display for LinearApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { reset_at_ms } => write!(f, "rate limited (reset at {reset_at_ms})"),
            Self::AuthError { message } => write!(f, "auth error: {message}"),
            Self::GraphqlErrors { errors } => {
                let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                write!(f, "graphql errors: {}", msgs.join("; "))
            }
            Self::Network(e) => write!(f, "network: {e}"),
        }
    }
}

impl std::error::Error for LinearApiError {}

/// Shared Linear GraphQL API client with retry-on-429.
pub struct LinearClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    observer: Arc<dyn Observer>,
}

impl LinearClient {
    /// Production constructor — base URL defaults to `https://api.linear.app`.
    pub fn new(api_key: String, observer: Arc<dyn Observer>) -> Self {
        Self::new_with_base_url(api_key, "https://api.linear.app".into(), observer)
    }

    /// Test constructor — caller supplies a wiremock base URL.
    pub fn new_with_base_url(
        api_key: String,
        base_url: String,
        observer: Arc<dyn Observer>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url,
            observer,
        }
    }

    /// Execute a GraphQL query or mutation.
    ///
    /// Linear uses a raw API key in the Authorization header (no `Bearer` prefix).
    #[allow(clippy::cast_possible_truncation)]
    pub async fn graphql(&self, query: &str, variables: &Value) -> Result<Value, LinearApiError> {
        let url = format!("{}/graphql", self.base_url);
        let body = json!({ "query": query, "variables": variables });
        let mut retries = 0u32;
        let call_start = std::time::Instant::now();
        let mut total_rate_limit_wait_ms: u64 = 0;
        #[allow(unused_assignments)]
        let mut last_status: Option<u16> = None;

        let result: Result<Value, LinearApiError> = loop {
            let resp = self
                .http
                .post(&url)
                .header("Authorization", &self.api_key)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(LinearApiError::Network)?;

            let status = resp.status().as_u16();
            last_status = Some(status);

            // Rate limiting — Linear returns 429 with X-RateLimit-Requests-Reset (ms epoch).
            if status == 429 {
                let reset_ms = resp
                    .headers()
                    .get("X-RateLimit-Requests-Reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);

                if retries >= MAX_RETRIES {
                    break Err(LinearApiError::RateLimited {
                        reset_at_ms: reset_ms,
                    });
                }

                let wait = compute_wait_from_reset(reset_ms);
                total_rate_limit_wait_ms += wait.as_millis() as u64;
                debug!(reset_ms, ?wait, retries, "linear rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            // Auth errors.
            if status == 401 || status == 403 {
                let text = resp.text().await.unwrap_or_default();
                break Err(LinearApiError::AuthError { message: text });
            }

            let json: Value = resp.json().await.map_err(LinearApiError::Network)?;

            // GraphQL errors.
            if let Some(errors) = json.get("errors") {
                if let Ok(errs) = serde_json::from_value::<Vec<LinearGraphqlError>>(errors.clone())
                {
                    if !errs.is_empty() {
                        break Err(LinearApiError::GraphqlErrors { errors: errs });
                    }
                }
            }

            // Return `data` directly (not the wrapper).
            match json.get("data").cloned() {
                Some(data) => break Ok(data),
                None => break Ok(json),
            }
        };

        let duration_ms = call_start.elapsed().as_millis() as u64;
        let success = result.is_ok();
        let error = result.as_ref().err().map(|e| e.to_string());
        let response_size_bytes = result
            .as_ref()
            .ok()
            .and_then(|v| serde_json::to_string(v).ok())
            .map(|s| s.len() as u64);

        self.observer
            .record_event(&ObserverEvent::IntegrationApiCall {
                integration: "linear".into(),
                method: "graphql".into(),
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

/// Compute how long to wait based on the reset timestamp (ms since epoch).
#[allow(clippy::cast_possible_truncation)] // epoch ms fits u64 until year 584M
fn compute_wait_from_reset(reset_ms: u64) -> Duration {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;

    if reset_ms > now_ms {
        Duration::from_millis(reset_ms - now_ms)
    } else {
        // Reset time already passed; retry immediately.
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::noop::NoopObserver;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client(server: &MockServer) -> LinearClient {
        LinearClient::new_with_base_url("lin_api_test".into(), server.uri(), Arc::new(NoopObserver))
    }

    #[tokio::test]
    async fn graphql_success_returns_data() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": {"viewer": {"id": "user_123"}}})),
            )
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { id } }", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["viewer"]["id"], "user_123");
    }

    #[tokio::test]
    async fn graphql_auth_header_has_no_bearer_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("Authorization", "lin_api_key_123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {}})))
            .mount(&server)
            .await;

        let client = LinearClient::new_with_base_url(
            "lin_api_key_123".into(),
            server.uri(),
            Arc::new(NoopObserver),
        );
        let result = client
            .graphql("query { viewer { id } }", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn graphql_errors_return_graphql_error_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"errors": [{"message": "Not found", "path": ["issue"]}]}),
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { issue { id } }", &serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(LinearApiError::GraphqlErrors { .. })));
    }

    #[tokio::test]
    #[allow(clippy::cast_possible_truncation)]
    async fn rate_limited_retries_using_reset_header_ms() {
        let server = MockServer::start().await;
        let reset_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 100;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(429)
                    .append_header("X-RateLimit-Requests-Reset", reset_ms.to_string()),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {}})))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { id } }", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn auth_error_returns_auth_error_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let client =
            LinearClient::new_with_base_url("bad_key".into(), server.uri(), Arc::new(NoopObserver));
        let result = client
            .graphql("query { viewer { id } }", &serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(LinearApiError::AuthError { .. })));
    }

    #[test]
    fn linear_api_error_display() {
        let e = LinearApiError::AuthError {
            message: "Unauthorized".into(),
        };
        assert!(e.to_string().contains("Unauthorized"));

        let e = LinearApiError::GraphqlErrors {
            errors: vec![LinearGraphqlError {
                message: "Not found".into(),
                path: Some(vec!["issue".into()]),
            }],
        };
        assert!(e.to_string().contains("Not found"));

        let e = LinearApiError::RateLimited { reset_at_ms: 12345 };
        assert!(e.to_string().contains("12345"));
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn compute_wait_from_reset_future() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let wait = compute_wait_from_reset(now_ms + 500);
        assert!(wait.as_millis() > 0);
        assert!(wait.as_millis() <= 500);
    }

    #[test]
    fn compute_wait_from_reset_past() {
        let wait = compute_wait_from_reset(0);
        assert_eq!(wait, Duration::ZERO);
    }

    #[tokio::test]
    async fn graphql_emits_integration_api_call_event_on_success() {
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
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data": {"viewer": {"id": "user_123"}}})),
            )
            .mount(&server)
            .await;

        let observer = Arc::new(CapturingObserver::default());
        let client = LinearClient::new_with_base_url(
            "lin_api_test".into(),
            server.uri(),
            observer.clone() as Arc<dyn Observer>,
        );

        let result = client
            .graphql("query { viewer { id } }", &serde_json::json!({}))
            .await;
        assert!(result.is_ok());

        let events = observer.events.lock();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("linear:graphql:success=true:status=Some(200)"));
    }

    #[tokio::test]
    async fn graphql_emits_integration_api_call_event_on_auth_error() {
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
                    success,
                    status_code,
                    ..
                } = event
                {
                    self.events.lock().push(format!(
                        "{integration}:success={success}:status={status_code:?}"
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
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let observer = Arc::new(CapturingObserver::default());
        let client = LinearClient::new_with_base_url(
            "bad_key".into(),
            server.uri(),
            observer.clone() as Arc<dyn Observer>,
        );

        let result = client
            .graphql("query { viewer { id } }", &serde_json::json!({}))
            .await;
        assert!(result.is_err());

        let events = observer.events.lock();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("linear:success=false:status=Some(401)"));
    }
}
