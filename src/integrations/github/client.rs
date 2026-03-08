use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::debug;

use crate::observability::traits::{Observer, ObserverEvent};

const MAX_RETRIES: u32 = 3;

/// A single GraphQL error from the GitHub API.
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubGraphqlError {
    pub message: String,
    pub path: Option<Vec<String>>,
}

/// Errors from the GitHub GraphQL API.
#[derive(Debug)]
pub enum GitHubApiError {
    /// HTTP 429 or 403 with X-RateLimit-Remaining: 0 — retries exhausted.
    RateLimited { reset_at: u64 },
    /// HTTP 401 or non-rate-limit 403.
    AuthError { message: String },
    /// GraphQL response contained `errors` array.
    GraphqlErrors { errors: Vec<GitHubGraphqlError> },
    /// Transport / DNS / TLS failure.
    Network(reqwest::Error),
}

impl std::fmt::Display for GitHubApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { reset_at } => write!(f, "rate limited (reset at {reset_at})"),
            Self::AuthError { message } => write!(f, "auth error: {message}"),
            Self::GraphqlErrors { errors } => {
                let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                write!(f, "graphql errors: {}", msgs.join("; "))
            }
            Self::Network(e) => write!(f, "network: {e}"),
        }
    }
}

impl std::error::Error for GitHubApiError {}

/// Shared GitHub GraphQL API client with retry-on-rate-limit.
pub struct GitHubClient {
    http: reqwest::Client,
    token: String,
    base_url: String,
    default_owner: Option<String>,
    observer: Arc<dyn Observer>,
}

impl GitHubClient {
    /// Production constructor — base URL defaults to `https://api.github.com`.
    pub fn new(token: String, default_owner: Option<String>, observer: Arc<dyn Observer>) -> Self {
        Self::new_with_base_url(token, "https://api.github.com".into(), default_owner, observer)
    }

    /// Test constructor — caller supplies a wiremock base URL.
    pub fn new_with_base_url(
        token: String,
        base_url: String,
        default_owner: Option<String>,
        observer: Arc<dyn Observer>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            base_url,
            default_owner,
            observer,
        }
    }

    /// Returns the configured default owner, if any.
    pub fn default_owner(&self) -> Option<&str> {
        self.default_owner.as_deref()
    }

    /// Execute a GraphQL query or mutation.
    ///
    /// GitHub uses `Bearer` prefix in the Authorization header and requires a
    /// `User-Agent` header. Rate limits are signalled via HTTP 429 or HTTP 403
    /// with `X-RateLimit-Remaining: 0`. The reset time is in `X-RateLimit-Reset`
    /// as unix epoch seconds.
    pub async fn graphql(&self, query: &str, variables: &Value) -> Result<Value, GitHubApiError> {
        let url = format!("{}/graphql", self.base_url);
        let body = json!({ "query": query, "variables": variables });
        let mut retries = 0u32;
        let call_start = std::time::Instant::now();
        let mut total_rate_limit_wait_ms: u64 = 0;
        let mut last_status: Option<u16> = None;

        let result: Result<Value, GitHubApiError> = loop {
            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("User-Agent", "zeroclaw")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(GitHubApiError::Network)?;

            let status = resp.status().as_u16();
            last_status = Some(status);

            // Rate limiting — GitHub returns 429 or 403 with X-RateLimit-Remaining: 0.
            let is_rate_limited = status == 429
                || (status == 403
                    && resp
                        .headers()
                        .get("X-RateLimit-Remaining")
                        .and_then(|v| v.to_str().ok())
                        == Some("0"));

            if is_rate_limited {
                let reset_secs = resp
                    .headers()
                    .get("X-RateLimit-Reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);

                if retries >= MAX_RETRIES {
                    break Err(GitHubApiError::RateLimited {
                        reset_at: reset_secs,
                    });
                }

                let wait = compute_wait_from_reset_secs(reset_secs);
                total_rate_limit_wait_ms += wait.as_millis() as u64;
                debug!(reset_secs, ?wait, retries, "github rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            // Auth errors — 401 or non-rate-limit 403.
            if status == 401 || status == 403 {
                let text = resp.text().await.unwrap_or_default();
                break Err(GitHubApiError::AuthError { message: text });
            }

            let json_resp: Value = resp.json().await.map_err(GitHubApiError::Network)?;

            // GraphQL errors.
            if let Some(errors) = json_resp.get("errors") {
                if let Ok(errs) = serde_json::from_value::<Vec<GitHubGraphqlError>>(errors.clone())
                {
                    if !errs.is_empty() {
                        break Err(GitHubApiError::GraphqlErrors { errors: errs });
                    }
                }
            }

            // Return `data` directly (not the wrapper).
            match json_resp.get("data").cloned() {
                Some(data) => break Ok(data),
                None => break Ok(json_resp),
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

        self.observer.record_event(&ObserverEvent::IntegrationApiCall {
            integration: "github".into(),
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

/// Compute how long to wait based on the reset timestamp (seconds since epoch).
fn compute_wait_from_reset_secs(reset_secs: u64) -> Duration {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    if reset_secs > now_secs {
        Duration::from_secs(reset_secs - now_secs)
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

    fn test_client(server: &MockServer) -> GitHubClient {
        GitHubClient::new_with_base_url(
            "ghp_test123".into(),
            server.uri(),
            Some("zeroclaw_org".into()),
            Arc::new(NoopObserver),
        )
    }

    #[tokio::test]
    async fn graphql_success_returns_data() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": {"viewer": {"login": "zeroclaw_user"}}})),
            )
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { login } }", &json!({}))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["viewer"]["login"], "zeroclaw_user");
    }

    #[tokio::test]
    async fn graphql_auth_header_uses_bearer_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("Authorization", "Bearer ghp_test123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { login } }", &json!({}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn graphql_errors_return_graphql_error_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"errors": [{"message": "Not found", "path": ["repository"]}]}),
            ))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { repository { id } }", &json!({}))
            .await;
        assert!(matches!(result, Err(GitHubApiError::GraphqlErrors { .. })));
    }

    #[tokio::test]
    async fn rate_limited_retries_using_reset_header_secs() {
        let server = MockServer::start().await;
        let reset_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 1;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(429)
                    .append_header("X-RateLimit-Reset", reset_secs.to_string()),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { login } }", &json!({}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn rate_limited_via_403_with_remaining_zero() {
        let server = MockServer::start().await;
        let reset_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 1;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(403)
                    .append_header("X-RateLimit-Remaining", "0")
                    .append_header("X-RateLimit-Reset", reset_secs.to_string()),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { login } }", &json!({}))
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

        let client = test_client(&server);
        let result = client
            .graphql("query { viewer { login } }", &json!({}))
            .await;
        assert!(matches!(result, Err(GitHubApiError::AuthError { .. })));
    }

    #[test]
    fn github_api_error_display() {
        let e = GitHubApiError::AuthError {
            message: "Unauthorized".into(),
        };
        assert!(e.to_string().contains("Unauthorized"));

        let e = GitHubApiError::GraphqlErrors {
            errors: vec![GitHubGraphqlError {
                message: "Not found".into(),
                path: Some(vec!["repository".into()]),
            }],
        };
        assert!(e.to_string().contains("Not found"));

        let e = GitHubApiError::RateLimited { reset_at: 12345 };
        assert!(e.to_string().contains("12345"));
    }

    #[test]
    fn default_owner_returns_configured_value() {
        let client = GitHubClient::new(
            "ghp_test123".into(),
            Some("zeroclaw_org".into()),
            Arc::new(NoopObserver),
        );
        assert_eq!(client.default_owner(), Some("zeroclaw_org"));
    }

    #[test]
    fn default_owner_returns_none_when_not_set() {
        let client = GitHubClient::new("ghp_test123".into(), None, Arc::new(NoopObserver));
        assert_eq!(client.default_owner(), None);
    }

    #[test]
    fn compute_wait_from_reset_secs_future() {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let wait = compute_wait_from_reset_secs(now_secs + 5);
        assert!(wait.as_secs() > 0);
        assert!(wait.as_secs() <= 5);
    }

    #[test]
    fn compute_wait_from_reset_secs_past() {
        let wait = compute_wait_from_reset_secs(0);
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
                    .set_body_json(json!({"data": {"viewer": {"login": "zeroclaw_user"}}})),
            )
            .mount(&server)
            .await;

        let observer = Arc::new(CapturingObserver::default());
        let client = GitHubClient::new_with_base_url(
            "ghp_test123".into(),
            server.uri(),
            Some("zeroclaw_org".into()),
            observer.clone() as Arc<dyn Observer>,
        );

        let result = client
            .graphql("query { viewer { login } }", &json!({}))
            .await;
        assert!(result.is_ok());

        let events = observer.events.lock();
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("github:graphql:success=true:status=Some(200)"));
    }
}
