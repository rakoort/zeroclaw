# Integration Observability Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire up `IntegrationApiCall` observer events in GitHub, Linear, and Slack clients so integration API calls are no longer black boxes.

**Architecture:** Extend the `IntegrationApiCall` event with 3 new fields (`status_code`, `response_size_bytes`, `rate_limit_wait_ms`). Inject `Arc<dyn Observer>` into integration clients at construction time. Emit one event per HTTP call after the retry loop resolves.

**Tech Stack:** Rust, tracing, reqwest, wiremock (tests), parking_lot (test observer)

---

### Task 1: Extend IntegrationApiCall Event Schema

**Files:**
- Modify: `src/observability/traits.rs:87-94`

**Step 1: Write the failing test**

Add a test in `src/observability/traits.rs` that constructs an `IntegrationApiCall` with the three new fields and clones it:

```rust
#[test]
fn integration_api_call_event_with_extended_fields_is_cloneable() {
    let event = ObserverEvent::IntegrationApiCall {
        integration: "github".into(),
        method: "graphql".into(),
        success: true,
        duration_ms: 150,
        error: None,
        retries: 0,
        status_code: Some(200),
        response_size_bytes: Some(1024),
        rate_limit_wait_ms: None,
    };
    let cloned = event.clone();
    assert!(matches!(cloned, ObserverEvent::IntegrationApiCall { .. }));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib observability::traits::tests::integration_api_call_event_with_extended_fields_is_cloneable`
Expected: FAIL — `status_code`, `response_size_bytes`, `rate_limit_wait_ms` fields don't exist yet.

**Step 3: Add the three new fields to IntegrationApiCall**

In `src/observability/traits.rs`, extend the `IntegrationApiCall` variant (lines 87-94):

```rust
IntegrationApiCall {
    integration: String,
    method: String,
    success: bool,
    duration_ms: u64,
    error: Option<String>,
    retries: u32,
    status_code: Option<u16>,
    response_size_bytes: Option<u64>,
    rate_limit_wait_ms: Option<u64>,
},
```

**Step 4: Fix all existing match arms that destructure IntegrationApiCall**

The compiler will flag every match arm. Update each one to include the new fields:

- `src/observability/traits.rs:272-283` — existing clone test: add the 3 fields with `None`/`None`/`None`
- `src/observability/log.rs:124-150` — LogObserver handler: destructure and log new fields (next task)
- `src/observability/otel.rs:196` — skip arm: already uses `..` pattern, should compile
- `src/observability/prometheus.rs:231` — skip arm: already uses `..` pattern, should compile

For the existing clone test at line 272, update to include the new fields:

```rust
#[test]
fn integration_api_call_event_is_cloneable() {
    let event = ObserverEvent::IntegrationApiCall {
        integration: "slack".into(),
        method: "chat.postMessage".into(),
        success: true,
        duration_ms: 150,
        error: None,
        retries: 0,
        status_code: None,
        response_size_bytes: None,
        rate_limit_wait_ms: None,
    };
    let cloned = event.clone();
    assert!(matches!(cloned, ObserverEvent::IntegrationApiCall { .. }));
}
```

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib observability`
Expected: PASS — all observer tests compile and pass with new fields.

**Step 6: Commit**

```
git add src/observability/traits.rs
git commit -m "feat(observability): extend IntegrationApiCall with status_code, response_size_bytes, rate_limit_wait_ms"
```

---

### Task 2: Update LogObserver Handler

**Files:**
- Modify: `src/observability/log.rs:124-150`

**Step 1: Write the failing test**

Add a test in `src/observability/log.rs` that exercises the handler with the new fields:

```rust
#[test]
fn log_observer_integration_api_call_success_no_panic() {
    let obs = LogObserver::new();
    obs.record_event(&ObserverEvent::IntegrationApiCall {
        integration: "github".into(),
        method: "graphql".into(),
        success: true,
        duration_ms: 200,
        error: None,
        retries: 0,
        status_code: Some(200),
        response_size_bytes: Some(4096),
        rate_limit_wait_ms: None,
    });
}

#[test]
fn log_observer_integration_api_call_failure_no_panic() {
    let obs = LogObserver::new();
    obs.record_event(&ObserverEvent::IntegrationApiCall {
        integration: "slack".into(),
        method: "chat.postMessage".into(),
        success: false,
        duration_ms: 5000,
        error: Some("rate limited".into()),
        retries: 3,
        status_code: Some(429),
        response_size_bytes: None,
        rate_limit_wait_ms: Some(30000),
    });
}
```

**Step 2: Run tests to verify they pass (handler already exists, just doesn't log new fields)**

Run: `cargo test --lib observability::log::tests::log_observer_integration_api_call`
Expected: PASS (existing handler compiles but doesn't log the new fields yet).

**Step 3: Update LogObserver handler to log new fields**

Replace the `IntegrationApiCall` match arm in `src/observability/log.rs`:

```rust
ObserverEvent::IntegrationApiCall {
    integration,
    method,
    success,
    duration_ms,
    error,
    retries,
    status_code,
    response_size_bytes,
    rate_limit_wait_ms,
} => {
    if *success {
        info!(
            integration = %integration,
            method = %method,
            duration_ms = duration_ms,
            retries = retries,
            status_code = status_code.unwrap_or(0),
            response_size_bytes = response_size_bytes.unwrap_or(0),
            rate_limit_wait_ms = rate_limit_wait_ms.unwrap_or(0),
            "integration.api_call"
        );
    } else {
        warn!(
            integration = %integration,
            method = %method,
            duration_ms = duration_ms,
            retries = retries,
            status_code = status_code.unwrap_or(0),
            response_size_bytes = response_size_bytes.unwrap_or(0),
            rate_limit_wait_ms = rate_limit_wait_ms.unwrap_or(0),
            error = error.as_deref().unwrap_or("unknown"),
            "integration.api_call.error"
        );
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib observability::log`
Expected: PASS

**Step 5: Commit**

```
git add src/observability/log.rs
git commit -m "feat(observability): log extended IntegrationApiCall fields in LogObserver"
```

---

### Task 3: Update OtelObserver and PrometheusObserver Skip Arms

**Files:**
- Modify: `src/observability/otel.rs:196`
- Modify: `src/observability/prometheus.rs:231`

**Step 1: Verify existing tests still pass**

Run: `cargo test --lib observability::otel && cargo test --lib observability::prometheus`
Expected: PASS — both use `..` pattern in their skip arms so the new fields compile without changes.

If either fails because the match arm uses explicit field destructuring rather than `..`:
- Update to use `..` or add the new field names with `_` prefix.

**Step 2: Add a test for IntegrationApiCall in OtelObserver**

In `src/observability/otel.rs` tests, add within `records_all_events_without_panic`:

```rust
obs.record_event(&ObserverEvent::IntegrationApiCall {
    integration: "github".into(),
    method: "graphql".into(),
    success: true,
    duration_ms: 100,
    error: None,
    retries: 0,
    status_code: Some(200),
    response_size_bytes: Some(512),
    rate_limit_wait_ms: None,
});
```

**Step 3: Add a test for IntegrationApiCall in PrometheusObserver**

In `src/observability/prometheus.rs` tests, add within `records_all_events_without_panic`:

```rust
obs.record_event(&ObserverEvent::IntegrationApiCall {
    integration: "slack".into(),
    method: "chat.postMessage".into(),
    success: true,
    duration_ms: 150,
    error: None,
    retries: 0,
    status_code: Some(200),
    response_size_bytes: Some(256),
    rate_limit_wait_ms: None,
});
```

**Step 4: Run tests**

Run: `cargo test --lib observability`
Expected: PASS

**Step 5: Commit**

```
git add src/observability/otel.rs src/observability/prometheus.rs
git commit -m "test(observability): cover IntegrationApiCall in Otel and Prometheus observer tests"
```

---

### Task 4: Inject Observer into GitHubClient

**Files:**
- Modify: `src/integrations/github/client.rs:45-70` (struct + constructors)
- Modify: `src/integrations/github/mod.rs:17-27` (integration constructor)

**Step 1: Write the failing test**

Add a test in `src/integrations/github/client.rs` that verifies the client emits an `IntegrationApiCall` event on a successful GraphQL call:

```rust
#[tokio::test]
async fn graphql_emits_integration_api_call_event_on_success() {
    use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
    use parking_lot::Mutex;

    #[derive(Default)]
    struct CapturingObserver {
        events: Mutex<Vec<String>>,
    }
    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            if let ObserverEvent::IntegrationApiCall { integration, method, success, status_code, .. } = event {
                self.events.lock().push(format!(
                    "{integration}:{method}:success={success}:status={status_code:?}"
                ));
            }
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn name(&self) -> &str { "capturing" }
        fn as_any(&self) -> &dyn std::any::Any { self }
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

    let result = client.graphql("query { viewer { login } }", &json!({})).await;
    assert!(result.is_ok());

    let events = observer.events.lock();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("github:graphql:success=true:status=Some(200)"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib integrations::github::client::tests::graphql_emits_integration_api_call_event_on_success`
Expected: FAIL — `new_with_base_url` doesn't accept observer parameter yet.

**Step 3: Add observer field to GitHubClient and update constructors**

In `src/integrations/github/client.rs`:

```rust
use std::sync::Arc;
use crate::observability::traits::{Observer, ObserverEvent};

pub struct GitHubClient {
    http: reqwest::Client,
    token: String,
    base_url: String,
    default_owner: Option<String>,
    observer: Arc<dyn Observer>,
}

impl GitHubClient {
    pub fn new(token: String, default_owner: Option<String>, observer: Arc<dyn Observer>) -> Self {
        Self::new_with_base_url(token, "https://api.github.com".into(), default_owner, observer)
    }

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
```

**Step 4: Add observer emission in the graphql() method**

Wrap the retry loop with timing and emit after resolution. In `src/integrations/github/client.rs`, update `graphql()`:

```rust
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

        if status == 401 || status == 403 {
            let text = resp.text().await.unwrap_or_default();
            break Err(GitHubApiError::AuthError { message: text });
        }

        let json_resp: Value = resp.json().await.map_err(GitHubApiError::Network)?;

        if let Some(errors) = json_resp.get("errors") {
            if let Ok(errs) = serde_json::from_value::<Vec<GitHubGraphqlError>>(errors.clone()) {
                if !errs.is_empty() {
                    break Err(GitHubApiError::GraphqlErrors { errors: errs });
                }
            }
        }

        match json_resp.get("data").cloned() {
            Some(data) => break Ok(data),
            None => break Ok(json_resp),
        }
    };

    let duration_ms = call_start.elapsed().as_millis() as u64;
    let success = result.is_ok();
    let error = result.as_ref().err().map(|e| e.to_string());
    // Estimate response size from the result value (for success cases)
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
        error: error.clone(),
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
```

**Step 5: Update all test helper constructors and call sites**

In `src/integrations/github/client.rs` tests, update `test_client`:

```rust
fn test_client(server: &MockServer) -> GitHubClient {
    GitHubClient::new_with_base_url(
        "ghp_test123".into(),
        server.uri(),
        Some("zeroclaw_org".into()),
        Arc::new(crate::observability::noop::NoopObserver),
    )
}
```

And update standalone tests that call `GitHubClient::new(...)`:

```rust
let client = GitHubClient::new(
    "ghp_test123".into(),
    Some("zeroclaw_org".into()),
    Arc::new(crate::observability::noop::NoopObserver),
);
```

Add `use std::sync::Arc;` to the test module if not already present.

**Step 6: Update GitHubIntegration constructor**

In `src/integrations/github/mod.rs`, update:

```rust
use std::sync::Arc;
use crate::observability::traits::Observer;

pub struct GitHubIntegration {
    client: Arc<GitHubClient>,
}

impl GitHubIntegration {
    pub fn new(config: GitHubIntegrationConfig, observer: Arc<dyn Observer>) -> Self {
        Self {
            client: Arc::new(GitHubClient::new(config.token, config.owner, observer)),
        }
    }
}
```

Fix all call sites in `src/integrations/github/mod.rs` tests that construct `GitHubClient` or `GitHubIntegration` directly to pass `Arc::new(NoopObserver)`.

**Step 7: Run tests to verify they pass**

Run: `cargo test --lib integrations::github`
Expected: PASS

**Step 8: Commit**

```
git add src/integrations/github/client.rs src/integrations/github/mod.rs
git commit -m "feat(github): inject observer into GitHubClient, emit IntegrationApiCall events"
```

---

### Task 5: Inject Observer into LinearClient

**Files:**
- Modify: `src/integrations/linear/client.rs:45-64` (struct + constructors)
- Modify: `src/integrations/linear/mod.rs:17-27` (integration constructor)

**Step 1: Write the failing test**

Add in `src/integrations/linear/client.rs`:

```rust
#[tokio::test]
async fn graphql_emits_integration_api_call_event_on_success() {
    use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
    use parking_lot::Mutex;

    #[derive(Default)]
    struct CapturingObserver {
        events: Mutex<Vec<String>>,
    }
    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            if let ObserverEvent::IntegrationApiCall { integration, method, success, status_code, .. } = event {
                self.events.lock().push(format!(
                    "{integration}:{method}:success={success}:status={status_code:?}"
                ));
            }
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn name(&self) -> &str { "capturing" }
        fn as_any(&self) -> &dyn std::any::Any { self }
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

    let result = client.graphql("query { viewer { id } }", &serde_json::json!({})).await;
    assert!(result.is_ok());

    let events = observer.events.lock();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("linear:graphql:success=true:status=Some(200)"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib integrations::linear::client::tests::graphql_emits_integration_api_call_event_on_success`
Expected: FAIL

**Step 3: Add observer field and update constructors**

Same pattern as GitHub. In `src/integrations/linear/client.rs`:

```rust
use std::sync::Arc;
use crate::observability::traits::{Observer, ObserverEvent};

pub struct LinearClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    observer: Arc<dyn Observer>,
}

impl LinearClient {
    pub fn new(api_key: String, observer: Arc<dyn Observer>) -> Self {
        Self::new_with_base_url(api_key, "https://api.linear.app".into(), observer)
    }

    pub fn new_with_base_url(api_key: String, base_url: String, observer: Arc<dyn Observer>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url,
            observer,
        }
    }
```

**Step 4: Add observer emission in graphql()**

Same pattern as GitHub — wrap the loop, track timing/retries/rate-limit wait, emit after resolution. The implementation mirrors Task 4 Step 4 with `integration: "linear"` and using `compute_wait_from_reset` for millisecond-based reset.

**Step 5: Update LinearIntegration constructor**

In `src/integrations/linear/mod.rs`:

```rust
impl LinearIntegration {
    pub fn new(config: LinearIntegrationConfig, observer: Arc<dyn Observer>) -> Self {
        Self {
            client: Arc::new(LinearClient::new(config.api_key, observer)),
        }
    }
}
```

**Step 6: Fix all test call sites**

Update all `LinearClient::new(...)` and `LinearClient::new_with_base_url(...)` calls in tests to pass `Arc::new(NoopObserver)`. Same for `LinearIntegration::new(...)`.

**Step 7: Run tests**

Run: `cargo test --lib integrations::linear`
Expected: PASS

**Step 8: Commit**

```
git add src/integrations/linear/client.rs src/integrations/linear/mod.rs
git commit -m "feat(linear): inject observer into LinearClient, emit IntegrationApiCall events"
```

---

### Task 6: Inject Observer into SlackClient

**Files:**
- Modify: `src/integrations/slack/client.rs:44-66` (struct + constructors)
- Modify: `src/integrations/slack/mod.rs:39-67` (integration constructor)

**Step 1: Write the failing tests**

Add in `src/integrations/slack/client.rs`:

```rust
#[tokio::test]
async fn api_post_emits_integration_api_call_event_on_success() {
    use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
    use parking_lot::Mutex;

    #[derive(Default)]
    struct CapturingObserver {
        events: Mutex<Vec<String>>,
    }
    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            if let ObserverEvent::IntegrationApiCall { integration, method, success, status_code, .. } = event {
                self.events.lock().push(format!(
                    "{integration}:{method}:success={success}:status={status_code:?}"
                ));
            }
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn name(&self) -> &str { "capturing" }
        fn as_any(&self) -> &dyn std::any::Any { self }
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

    let result = client.api_post("chat.postMessage", &serde_json::json!({})).await;
    assert!(result.is_ok());

    let events = observer.events.lock();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("slack:chat.postMessage:success=true"));
}

#[tokio::test]
async fn api_get_emits_integration_api_call_event_on_success() {
    use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
    use parking_lot::Mutex;

    #[derive(Default)]
    struct CapturingObserver {
        events: Mutex<Vec<String>>,
    }
    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            if let ObserverEvent::IntegrationApiCall { integration, method, success, .. } = event {
                self.events.lock().push(format!(
                    "{integration}:{method}:success={success}"
                ));
            }
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn name(&self) -> &str { "capturing" }
        fn as_any(&self) -> &dyn std::any::Any { self }
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

    let result = client.api_get("users.getPresence", &[("user", "U123")]).await;
    assert!(result.is_ok());

    let events = observer.events.lock();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("slack:users.getPresence:success=true"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib integrations::slack::client::tests::api_post_emits`
Expected: FAIL

**Step 3: Add observer field and update constructors**

In `src/integrations/slack/client.rs`:

```rust
use std::sync::Arc;
use crate::observability::traits::{Observer, ObserverEvent};

pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
    #[allow(dead_code)]
    app_token: String,
    base_url: String,
    observer: Arc<dyn Observer>,
}

impl SlackClient {
    pub fn new(bot_token: String, app_token: String, observer: Arc<dyn Observer>) -> Self {
        Self::new_with_base_url(bot_token, app_token, "https://slack.com".into(), observer)
    }

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
```

**Step 4: Add observer emission in api_post() and api_get()**

Both methods get the same wrapping pattern. For `api_post`:

```rust
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
        error: error.clone(),
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
```

Apply the same pattern to `api_get()`.

**Step 5: Update SlackIntegration constructor**

In `src/integrations/slack/mod.rs`:

```rust
impl SlackIntegration {
    pub fn new(config: SlackIntegrationConfig, observer: Arc<dyn Observer>) -> Self {
        let client = Arc::new(SlackClient::new(
            config.bot_token.clone(),
            config.app_token.clone(),
            observer,
        ));
        Self::new_with_client(config, client)
    }
```

**Step 6: Fix all test call sites**

Update all `SlackClient::new(...)` and `SlackClient::new_with_base_url(...)` calls in tests to pass `Arc::new(NoopObserver)`. Same for `SlackIntegration::new(...)`.

**Step 7: Run tests**

Run: `cargo test --lib integrations::slack`
Expected: PASS

**Step 8: Commit**

```
git add src/integrations/slack/client.rs src/integrations/slack/mod.rs
git commit -m "feat(slack): inject observer into SlackClient, emit IntegrationApiCall events"
```

---

### Task 7: Wire Observer Through collect_integrations Factory

**Files:**
- Modify: `src/integrations/mod.rs:40-60` (factory function signature)
- Modify: `src/tools/mod.rs:318` (caller)
- Potentially modify: any other callers of `collect_integrations`

**Step 1: Write the failing test**

In `src/integrations/mod.rs` tests:

```rust
#[test]
fn collect_integrations_with_observer_returns_github_when_configured() {
    use crate::observability::noop::NoopObserver;

    let mut config = crate::config::Config::default();
    config.integrations.github = Some(crate::config::GitHubIntegrationConfig {
        token: "ghp_test".into(),
        owner: None,
    });
    let observer: Arc<dyn crate::observability::traits::Observer> = Arc::new(NoopObserver);
    let integrations = collect_integrations(&config, observer);
    assert_eq!(integrations.len(), 1);
    assert_eq!(integrations[0].name(), "github");
}
```

**Step 2: Run test to verify it fails**

Expected: FAIL — `collect_integrations` doesn't accept observer parameter yet.

**Step 3: Update collect_integrations signature**

In `src/integrations/mod.rs`:

```rust
use crate::observability::traits::Observer;

pub fn collect_integrations(config: &Config, observer: Arc<dyn Observer>) -> Vec<Arc<dyn Integration>> {
    let mut integrations: Vec<Arc<dyn Integration>> = Vec::new();

    if let Some(ref slack_config) = config.integrations.slack {
        integrations.push(Arc::new(slack::SlackIntegration::new(
            slack_config.clone(),
            observer.clone(),
        )));
    }

    if let Some(ref linear_config) = config.integrations.linear {
        integrations.push(Arc::new(linear::LinearIntegration::new(
            linear_config.clone(),
            observer.clone(),
        )));
    }

    if let Some(ref github_config) = config.integrations.github {
        integrations.push(Arc::new(github::GitHubIntegration::new(
            github_config.clone(),
            observer.clone(),
        )));
    }

    integrations
}
```

**Step 4: Update all callers**

The key callers to fix:

1. `src/tools/mod.rs:318` — `collect_integrations(root_config)` → needs observer param. The `all_tools_with_runtime()` function signature will need an `observer: Arc<dyn Observer>` parameter, or it needs access to one from its existing params. Check the function signature and thread it through.

2. `src/integrations/mod.rs:66` — `active_integration_summary()` calls `collect_integrations`. This function is used for prompt building (no tools executed), so pass `NoopObserver`:
   ```rust
   let integrations = collect_integrations(config, Arc::new(crate::observability::noop::NoopObserver));
   ```

3. `src/integrations/mod.rs:107` — `build_integration_tool_map()` same treatment as above.

4. All tests in `src/integrations/mod.rs` that call `collect_integrations(&config)` — add `Arc::new(NoopObserver)` as second arg.

**Step 5: Run full test suite**

Run: `cargo test --lib`
Expected: PASS — all callers updated.

**Step 6: Commit**

```
git add src/integrations/mod.rs src/tools/mod.rs
git commit -m "feat(integrations): thread observer through collect_integrations factory"
```

---

### Task 8: Add Failure and Rate-Limit Emission Tests

**Files:**
- Modify: `src/integrations/github/client.rs` (tests section)
- Modify: `src/integrations/linear/client.rs` (tests section)
- Modify: `src/integrations/slack/client.rs` (tests section)

**Step 1: Add failure emission test for GitHubClient**

```rust
#[tokio::test]
async fn graphql_emits_integration_api_call_event_on_auth_error() {
    // Same CapturingObserver pattern as Task 4
    // Mock 401 response
    // Assert event has success=false, status_code=Some(401), error contains "auth"
}
```

**Step 2: Add rate-limit emission test for GitHubClient**

```rust
#[tokio::test]
async fn graphql_emits_integration_api_call_event_with_rate_limit_wait() {
    // Mock 429 then 200
    // Assert event has retries=1, rate_limit_wait_ms=Some(>0)
}
```

**Step 3: Add failure emission test for LinearClient**

Same pattern, asserting `integration: "linear"`.

**Step 4: Add failure emission test for SlackClient (api_post)**

Same pattern, asserting `integration: "slack"`, `method: "chat.postMessage"`.

**Step 5: Run all integration tests**

Run: `cargo test --lib integrations`
Expected: PASS

**Step 6: Commit**

```
git add src/integrations/github/client.rs src/integrations/linear/client.rs src/integrations/slack/client.rs
git commit -m "test(integrations): add failure and rate-limit emission tests for all clients"
```

---

### Task 9: Full Validation

**Step 1: Run full test suite**

Run: `cargo test`
Expected: PASS

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS

**Step 3: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: PASS

**Step 4: Final commit if any fixups needed**

If clippy/fmt required changes:
```
git commit -m "chore: fix clippy/fmt warnings from integration observability"
```
