# Service Integrations Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace script-based Slack/Linear tools and unify Slack channel + tools into a single Integration trait, delivering native Rust API clients with shared rate limiting, structured errors, and observability.

**Architecture:** Integration trait owns an authenticated API client and exposes tools via the Tool trait. SlackIntegration also implements Channel. Wiring collects integrations at startup, extracts tools for the LLM, and registers channels for the orchestrator. Existing channel/tool infrastructure stays unchanged.

**Tech Stack:** Rust, async-trait, reqwest, serde_json, tokio, tokio-tungstenite, wiremock 0.6 (tests)

**Design doc:** `docs/plans/2026-02-28-service-integrations-design.md`

---

### Task 1: Integration Trait and Module Scaffolding

Rework `src/integrations/mod.rs` to define the `Integration` trait and a `collect_integrations()` factory. The existing CLI catalog (`IntegrationEntry`, `IntegrationCategory`, `registry.rs`) moves to a `catalog` submodule so the top-level module owns the trait.

**Files:**
- Modify: `src/integrations/mod.rs` (currently lines 1-228 — CLI catalog)
- Create: `src/integrations/catalog.rs` (relocated catalog code)
- Create: `src/integrations/catalog_registry.rs` (relocated from `registry.rs`)
- Delete: `src/integrations/registry.rs` (content moves to `catalog_registry.rs`)
- Modify: `src/lib.rs:58` — no change needed, `pub(crate) mod integrations` already exists
- Test: inline `#[cfg(test)]` in `src/integrations/mod.rs`

**Step 1: Write the failing test**

In `src/integrations/mod.rs`, add a test that constructs a dummy `Integration` and calls `name()`, `tools()`, `health_check()`, and `as_channel()`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::traits::Tool;

    struct DummyIntegration;

    #[async_trait::async_trait]
    impl Integration for DummyIntegration {
        fn name(&self) -> &str { "dummy" }
        fn tools(&self) -> Vec<Arc<dyn Tool>> { vec![] }
    }

    #[tokio::test]
    async fn dummy_integration_default_methods() {
        let i = DummyIntegration;
        assert_eq!(i.name(), "dummy");
        assert!(i.tools().is_empty());
        assert!(i.health_check().await);
        assert!(i.as_channel().is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::tests::dummy_integration_default_methods`
Expected: FAIL — `Integration` trait doesn't exist yet.

**Step 3: Move catalog code and implement the trait**

1. Create `src/integrations/catalog.rs` — move `IntegrationStatus`, `IntegrationCategory`, `IntegrationEntry`, `handle_command()`, `show_integration_info()`, and the catalog tests from the current `mod.rs`.
2. Create `src/integrations/catalog_registry.rs` — move `all_integrations()` and its tests from `src/integrations/registry.rs`.
3. Rewrite `src/integrations/mod.rs`:

```rust
pub mod catalog;
mod catalog_registry;

use async_trait::async_trait;
use std::sync::Arc;
use crate::channels::traits::Channel;
use crate::tools::traits::Tool;

#[async_trait]
pub trait Integration: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    async fn health_check(&self) -> bool { true }
    fn as_channel(&self) -> Option<Arc<dyn Channel>> { None }
}
```

4. Update any imports that reference `integrations::registry` to use `integrations::catalog_registry` (via the `catalog` module's re-exports). Update `handle_command` callers to use `integrations::catalog::handle_command`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations`
Expected: PASS — both new trait tests and relocated catalog tests pass.

**Step 5: Commit**

```bash
git add src/integrations/
git commit -m "feat(integrations): define Integration trait and relocate catalog to submodule"
```

---

### Task 2: Add IntegrationApiCall Observer Event

Add the new `IntegrationApiCall` variant to `ObserverEvent` so clients can emit it. Also fix the Prometheus observer to handle `ClassificationResult`, `PlannerRequest`, `PlannerResponse`, and `FallbackTriggered` (currently ignored at `src/observability/prometheus.rs:227-230`).

**Files:**
- Modify: `src/observability/traits.rs:10-86` — add variant to `ObserverEvent`
- Modify: `src/observability/prometheus.rs:227-230` — handle new + existing events
- Test: inline `#[cfg(test)]` in both files

**Step 1: Write the failing test**

In `src/observability/traits.rs`, add a test that constructs and clones the new event:

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
    };
    let cloned = event.clone();
    assert!(matches!(cloned, ObserverEvent::IntegrationApiCall { .. }));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib observability::traits::tests::integration_api_call_event_is_cloneable`
Expected: FAIL — variant doesn't exist.

**Step 3: Add the variant**

In `src/observability/traits.rs`, after `FallbackTriggered` (line 85), add:

```rust
/// An integration made an API call to an external service.
IntegrationApiCall {
    integration: String,
    method: String,
    success: bool,
    duration_ms: u64,
    error: Option<String>,
    retries: u32,
},
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib observability::traits::tests`
Expected: PASS.

**Step 5: Update Prometheus observer to handle new + ignored events**

In `src/observability/prometheus.rs`, replace the ignore block at lines 227-230 with counters. Add struct fields for the new metrics and handle each event:

- `IntegrationApiCall` → increment `zeroclaw_integration_api_calls_total{integration, method, success}`, observe duration histogram, increment retries counter
- `ClassificationResult` → increment `zeroclaw_classification_total{tier}`
- `PlannerRequest` → increment `zeroclaw_planner_requests_total{model}`
- `PlannerResponse` → no-op (or observe latency if desired)
- `FallbackTriggered` → increment `zeroclaw_fallback_triggered_total{hint, failed_model, fallback_model}`

**Step 6: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS — no regressions. The new variant is handled in all match arms.

**Step 7: Commit**

```bash
git add src/observability/
git commit -m "feat(observability): add IntegrationApiCall event and fix Prometheus gaps"
```

---

### Task 3: Integration Config Schema

Add `integrations` section to the config schema with `SlackIntegrationConfig` and `LinearIntegrationConfig`.

**Files:**
- Modify: `src/config/integrations.rs:900-909` — add new config structs after `ToolsConfig`
- Modify: `src/config/schema.rs:25-159` — add `integrations` field to `Config`
- Modify: `src/config/schema.rs` default impl (~line 307) — add default
- Modify: `src/config/mod.rs` — re-export new types
- Test: inline tests in `src/config/integrations.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn slack_integration_config_deserializes() {
    let toml_str = r#"
bot_token = "xoxb-test"
app_token = "xapp-test"
channel_id = "C123"
allowed_users = ["U111"]
mention_only = true
"#;
    let config: SlackIntegrationConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.bot_token, "xoxb-test");
    assert_eq!(config.channel_id.as_deref(), Some("C123"));
    assert!(config.mention_only);
}

#[test]
fn linear_integration_config_deserializes() {
    let toml_str = r#"
api_key = "lin_api_test"
"#;
    let config: LinearIntegrationConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.api_key, "lin_api_test");
}

#[test]
fn integrations_config_defaults_to_empty() {
    let config: IntegrationsConfig = IntegrationsConfig::default();
    assert!(config.slack.is_none());
    assert!(config.linear.is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib config -- integration_config`
Expected: FAIL — types don't exist.

**Step 3: Implement config structs**

In `src/config/integrations.rs`, after `ToolsConfig` (line 909), add:

```rust
/// Top-level integrations configuration (`[integrations]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct IntegrationsConfig {
    pub slack: Option<SlackIntegrationConfig>,
    pub linear: Option<LinearIntegrationConfig>,
}

/// Slack integration configuration (`[integrations.slack]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SlackIntegrationConfig {
    pub bot_token: String,
    pub app_token: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default = "default_mention_only_integration")]
    pub mention_only: bool,
    pub mention_regex: Option<String>,
    pub triage_model: Option<String>,
}

fn default_mention_only_integration() -> bool { true }

/// Linear integration configuration (`[integrations.linear]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinearIntegrationConfig {
    pub api_key: String,
}
```

Add `integrations: IntegrationsConfig` to `Config` struct and its `Default` impl. Re-export from `src/config/mod.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib config`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/config/
git commit -m "feat(config): add integrations config section for Slack and Linear"
```

---

### Task 4: SlackClient — HTTP, Auth, Rate Limiting

Build the shared Slack API client with rate-limit retry, error types, `ok: false` envelope parsing, and observer event emission.

**Files:**
- Create: `src/integrations/slack/mod.rs` — re-exports
- Create: `src/integrations/slack/client.rs` — `SlackClient`, `SlackApiError`
- Modify: `src/integrations/mod.rs` — add `pub mod slack;`
- Test: inline `#[cfg(test)]` in `client.rs` using `wiremock::MockServer`

**Step 1: Write the failing tests**

In `src/integrations/slack/client.rs`, write tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path, header};

    #[tokio::test]
    async fn api_post_success_returns_parsed_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "ts": "123"})))
            .mount(&server).await;

        let client = SlackClient::new_with_base_url(
            "xoxb-test".into(), String::new(), server.uri(),
        );
        let result = client.api_post("chat.postMessage", &serde_json::json!({})).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["ts"], "123");
    }

    #[tokio::test]
    async fn api_post_auth_error_returns_slack_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth.test"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": false, "error": "invalid_auth"})))
            .mount(&server).await;

        let client = SlackClient::new_with_base_url(
            "xoxb-bad".into(), String::new(), server.uri(),
        );
        let result = client.api_post("auth.test", &serde_json::json!({})).await;
        assert!(matches!(result, Err(SlackApiError::AuthError { .. })));
    }

    #[tokio::test]
    async fn api_post_rate_limited_retries_and_succeeds() {
        let server = MockServer::start().await;
        // First call returns 429, second returns success
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(429)
                .append_header("Retry-After", "0"))
            .up_to_n_times(1)
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true})))
            .mount(&server).await;

        let client = SlackClient::new_with_base_url(
            "xoxb-test".into(), String::new(), server.uri(),
        );
        let result = client.api_post("chat.postMessage", &serde_json::json!({})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn api_post_includes_bearer_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth.test"))
            .and(header("Authorization", "Bearer xoxb-my-token"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true})))
            .mount(&server).await;

        let client = SlackClient::new_with_base_url(
            "xoxb-my-token".into(), String::new(), server.uri(),
        );
        let result = client.api_post("auth.test", &serde_json::json!({})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn api_get_success_returns_parsed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/users.getPresence"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "presence": "active"})))
            .mount(&server).await;

        let client = SlackClient::new_with_base_url(
            "xoxb-test".into(), String::new(), server.uri(),
        );
        let result = client.api_get("users.getPresence", &[("user", "U123")]).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["presence"], "active");
    }

    #[tokio::test]
    async fn api_post_channel_not_found_returns_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": false, "error": "channel_not_found"})))
            .mount(&server).await;

        let client = SlackClient::new_with_base_url(
            "xoxb-test".into(), String::new(), server.uri(),
        );
        let result = client.api_post("chat.postMessage", &serde_json::json!({})).await;
        assert!(matches!(result, Err(SlackApiError::ApiError { .. })));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::slack::client::tests`
Expected: FAIL — module doesn't exist.

**Step 3: Implement SlackClient**

Create `src/integrations/slack/mod.rs`:
```rust
pub mod client;
```

Create `src/integrations/slack/client.rs` with:
- `SlackApiError` enum: `RateLimited { retry_after: Duration }`, `AuthError { error: String }`, `ApiError { method: String, error: String }`, `Network(reqwest::Error)`
- `SlackClient` struct with `http: reqwest::Client`, `bot_token: String`, `app_token: String`, `base_url: String`
- `new(bot_token, app_token)` — defaults base_url to `https://slack.com`
- `new_with_base_url(bot_token, app_token, base_url)` — for tests
- `api_post(method, body)` — POST to `{base_url}/api/{method}`, bearer auth, parse envelope, retry on 429
- `api_get(method, params)` — GET with query params, same retry logic
- `api_post_multipart(method, form)` — for file uploads

Auth errors are `invalid_auth`, `token_revoked`, `not_authed`, `account_inactive`. All other `ok: false` errors map to `ApiError`.

Rate limit retry: up to 3 attempts, read `Retry-After` header, wait, retry. Emit `ObserverEvent::IntegrationApiCall` on each completed call (requires an optional `observer` field on `SlackClient`).

Add `pub mod slack;` to `src/integrations/mod.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations::slack::client::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/slack/
git commit -m "feat(integrations): implement SlackClient with rate limiting and error types"
```

---

### Task 5: Slack Tools (9 Tools)

Implement 9 tool structs, each wrapping `Arc<SlackClient>`, implementing the `Tool` trait.

**Files:**
- Create: `src/integrations/slack/tools.rs` — all 9 tool structs + impls
- Modify: `src/integrations/slack/mod.rs` — add `pub mod tools;`
- Test: inline `#[cfg(test)]` in `tools.rs`

**Step 1: Write the failing tests**

Write tests for 2-3 representative tools and a schema validity test covering all 9:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path};

    async fn mock_client(server: &MockServer) -> Arc<SlackClient> {
        Arc::new(SlackClient::new_with_base_url(
            "xoxb-test".into(), String::new(), server.uri(),
        ))
    }

    #[tokio::test]
    async fn slack_send_missing_channel_id_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server).await;
        let tool = SlackSendTool { client };
        let result = tool.execute(serde_json::json!({"message": "hi"})).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn slack_send_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "ts": "1234"})))
            .mount(&server).await;

        let client = mock_client(&server).await;
        let tool = SlackSendTool { client };
        let result = tool.execute(serde_json::json!({
            "channel_id": "C123", "message": "hello"
        })).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn slack_history_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations.history"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "messages": []})))
            .mount(&server).await;

        let client = mock_client(&server).await;
        let tool = SlackHistoryTool { client };
        let result = tool.execute(serde_json::json!({"channel_id": "C123"})).await.unwrap();
        assert!(result.success);
    }

    #[test]
    fn all_slack_tools_have_valid_json_schemas() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into()));
        let tools = all_slack_tools(client);
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(schema["type"], "object", "Tool {} schema must be object", tool.name());
            assert!(schema.get("properties").is_some(), "Tool {} must have properties", tool.name());
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::slack::tools::tests`
Expected: FAIL — module doesn't exist.

**Step 3: Implement all 9 tools**

Each tool struct holds `client: Arc<SlackClient>`. Implement `Tool` trait (`name()`, `description()`, `parameters_schema()`, `execute()`).

Tools: `SlackDmTool`, `SlackSendTool`, `SlackSendThreadTool`, `SlackSendFileTool`, `SlackHistoryTool`, `SlackDmHistoryTool`, `SlackThreadsTool`, `SlackPresenceTool`, `SlackReactTool`.

Add a public `fn all_slack_tools(client: Arc<SlackClient>) -> Vec<Arc<dyn Tool>>` that returns all 9.

Reference the design doc's tool table (`docs/plans/2026-02-28-service-integrations-design.md` lines 173-183) for each tool's API method and parameters.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations::slack::tools::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/slack/tools.rs
git commit -m "feat(integrations): implement 9 native Slack tools"
```

---

### Task 6: SlackIntegration (Integration + Channel)

Implement `SlackIntegration` struct that implements both `Integration` and `Channel`. Port Socket Mode, message parsing, thread tracking, allowlist enforcement, and triage gating from the existing `SlackChannel` (`src/channels/slack.rs:298-1606`).

**Files:**
- Modify: `src/integrations/slack/mod.rs` — add `SlackIntegration` struct and impls
- Test: inline `#[cfg(test)]` in `mod.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::Integration;

    #[test]
    fn slack_integration_name() {
        let config = SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        };
        let integration = SlackIntegration::new(config);
        assert_eq!(integration.name(), "slack");
    }

    #[test]
    fn slack_integration_returns_9_tools() {
        let config = SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        };
        let integration = SlackIntegration::new(config);
        assert_eq!(integration.tools().len(), 9);
    }

    #[test]
    fn slack_integration_as_channel_returns_some() {
        let config = SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        };
        let integration = Arc::new(SlackIntegration::new(config));
        assert!(integration.as_channel().is_some());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::slack::tests`
Expected: FAIL — `SlackIntegration` doesn't exist.

**Step 3: Implement SlackIntegration**

```rust
pub struct SlackIntegration {
    client: Arc<SlackClient>,
    config: SlackIntegrationConfig,
    participated_threads: std::sync::Mutex<std::collections::HashSet<String>>,
    mention_regex: Option<regex::Regex>,
}
```

Implement `Integration`:
- `name()` → `"slack"`
- `tools()` → `all_slack_tools(Arc::clone(&self.client))`
- `health_check()` → `self.client.api_post("auth.test", &json!({})).await.is_ok()`
- `as_channel()` → `Some(Arc::new(self_clone))` — requires `SlackIntegration` to be wrapped in `Arc` and cloneable, or use `Arc<Self>` pattern

Implement `Channel` on `SlackIntegration`:
- Port `listen()` from `src/channels/slack.rs` — Socket Mode connection, envelope parsing, ack, thread tracking, allowlist, triage gating. Replace direct HTTP calls with `self.client.api_post()` / `self.client.api_get()`.
- Port `send()` — chunked message posting via `self.client.api_post("chat.postMessage", ...)`
- Port `add_reaction()` / `remove_reaction()` — delegate to `self.client.api_post("reactions.add/remove", ...)`
- Port `start_typing()` — no-op (Slack doesn't support bot typing indicators via API)
- Port draft methods if the existing Slack channel supports them

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations::slack`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/slack/
git commit -m "feat(integrations): implement SlackIntegration with Channel + Integration traits"
```

---

### Task 7: LinearClient — GraphQL, Auth, Rate Limiting

Build the Linear API client with GraphQL support, auth, rate-limit handling, and error types.

**Files:**
- Create: `src/integrations/linear/mod.rs` — re-exports
- Create: `src/integrations/linear/client.rs` — `LinearClient`, `LinearApiError`
- Modify: `src/integrations/mod.rs` — add `pub mod linear;`
- Test: inline `#[cfg(test)]` in `client.rs` using `wiremock::MockServer`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path, header};

    #[tokio::test]
    async fn graphql_success_returns_data() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "data": {"viewer": {"id": "user_123"}}
                })))
            .mount(&server).await;

        let client = LinearClient::new_with_base_url("lin_api_test".into(), server.uri());
        let result = client.graphql("query { viewer { id } }", &serde_json::json!({})).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["viewer"]["id"], "user_123");
    }

    #[tokio::test]
    async fn graphql_auth_header_has_no_bearer_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("Authorization", "lin_api_key_123"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": {}})))
            .mount(&server).await;

        let client = LinearClient::new_with_base_url("lin_api_key_123".into(), server.uri());
        let result = client.graphql("query { viewer { id } }", &serde_json::json!({})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn graphql_errors_return_graphql_error_type() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "errors": [{"message": "Not found", "path": ["issue"]}]
                })))
            .mount(&server).await;

        let client = LinearClient::new_with_base_url("lin_api_test".into(), server.uri());
        let result = client.graphql("query { issue { id } }", &serde_json::json!({})).await;
        assert!(matches!(result, Err(LinearApiError::GraphqlErrors { .. })));
    }

    #[tokio::test]
    async fn rate_limited_retries_using_reset_header_ms() {
        let server = MockServer::start().await;
        // Return 429 with reset timestamp in milliseconds (now + 100ms)
        let reset_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_millis() as u64 + 100;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(429)
                .append_header("X-RateLimit-Requests-Reset", reset_ms.to_string()))
            .up_to_n_times(1)
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": {}})))
            .mount(&server).await;

        let client = LinearClient::new_with_base_url("lin_api_test".into(), server.uri());
        let result = client.graphql("query { viewer { id } }", &serde_json::json!({})).await;
        assert!(result.is_ok());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::linear::client::tests`
Expected: FAIL — module doesn't exist.

**Step 3: Implement LinearClient**

Create `src/integrations/linear/mod.rs`:
```rust
pub mod client;
```

Create `src/integrations/linear/client.rs`:
- `LinearApiError` enum: `RateLimited { reset_at_ms: u64 }`, `AuthError { message: String }`, `GraphqlErrors { errors: Vec<LinearGraphqlError> }`, `Network(reqwest::Error)`
- `LinearGraphqlError` struct: `message: String`, `path: Option<Vec<String>>`
- `LinearClient` struct: `http: reqwest::Client`, `api_key: String`, `base_url: String`
- `new(api_key)` — base_url = `https://api.linear.app`
- `new_with_base_url(api_key, base_url)` — for tests
- `graphql(query, variables)` — POST to `{base_url}/graphql`, `Authorization: {api_key}` (**no Bearer**), parse response, handle errors array, retry on 429 using `X-RateLimit-Requests-Reset` (milliseconds)

Add `pub mod linear;` to `src/integrations/mod.rs`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations::linear::client::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/linear/
git commit -m "feat(integrations): implement LinearClient with GraphQL and rate limiting"
```

---

### Task 8: Linear Tools (14 Tools)

Implement 14 tool structs wrapping `Arc<LinearClient>`.

**Files:**
- Create: `src/integrations/linear/tools.rs` — all 14 tool structs + impls
- Modify: `src/integrations/linear/mod.rs` — add `pub mod tools;`
- Test: inline `#[cfg(test)]` in `tools.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path};

    async fn mock_client(server: &MockServer) -> Arc<LinearClient> {
        Arc::new(LinearClient::new_with_base_url("lin_api_test".into(), server.uri()))
    }

    #[tokio::test]
    async fn linear_create_issue_missing_team_id_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server).await;
        let tool = LinearCreateIssueTool { client };
        let result = tool.execute(serde_json::json!({"title": "Bug"})).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn linear_create_issue_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "data": {"issueCreate": {"success": true, "issue": {"id": "ISS-1"}}}
                })))
            .mount(&server).await;

        let client = mock_client(&server).await;
        let tool = LinearCreateIssueTool { client };
        let result = tool.execute(serde_json::json!({
            "team_id": "team_1", "title": "Bug fix"
        })).await.unwrap();
        assert!(result.success);
    }

    #[test]
    fn all_linear_tools_have_valid_json_schemas() {
        let client = Arc::new(LinearClient::new("lin_api_test".into()));
        let tools = all_linear_tools(client);
        assert_eq!(tools.len(), 14);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(schema["type"], "object", "Tool {} schema must be object", tool.name());
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::linear::tools::tests`
Expected: FAIL — module doesn't exist.

**Step 3: Implement all 14 tools**

Each struct holds `client: Arc<LinearClient>`. Reference design doc lines 275-291 for GraphQL operations and parameters.

Tools: `LinearIssuesTool`, `LinearCreateIssueTool`, `LinearUpdateIssueTool`, `LinearArchiveIssueTool`, `LinearAddCommentTool`, `LinearTeamsTool`, `LinearUsersTool`, `LinearProjectsTool`, `LinearCyclesTool`, `LinearLabelsTool`, `LinearStatesTool`, `LinearCreateLabelTool`, `LinearCreateProjectTool`, `LinearCreateCycleTool`.

Add `fn all_linear_tools(client: Arc<LinearClient>) -> Vec<Arc<dyn Tool>>`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations::linear::tools::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/linear/tools.rs
git commit -m "feat(integrations): implement 14 native Linear tools"
```

---

### Task 9: LinearIntegration

Implement `LinearIntegration` — Integration only, no Channel.

**Files:**
- Modify: `src/integrations/linear/mod.rs` — add `LinearIntegration` struct + impl
- Test: inline tests

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::Integration;

    #[test]
    fn linear_integration_name() {
        let config = LinearIntegrationConfig { api_key: "lin_api_test".into() };
        let integration = LinearIntegration::new(config);
        assert_eq!(integration.name(), "linear");
    }

    #[test]
    fn linear_integration_returns_14_tools() {
        let config = LinearIntegrationConfig { api_key: "lin_api_test".into() };
        let integration = LinearIntegration::new(config);
        assert_eq!(integration.tools().len(), 14);
    }

    #[test]
    fn linear_integration_as_channel_returns_none() {
        let config = LinearIntegrationConfig { api_key: "lin_api_test".into() };
        let integration = LinearIntegration::new(config);
        assert!(integration.as_channel().is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::linear::tests`
Expected: FAIL.

**Step 3: Implement LinearIntegration**

```rust
pub struct LinearIntegration {
    client: Arc<LinearClient>,
}

impl LinearIntegration {
    pub fn new(config: LinearIntegrationConfig) -> Self {
        Self {
            client: Arc::new(LinearClient::new(config.api_key)),
        }
    }
}

#[async_trait]
impl Integration for LinearIntegration {
    fn name(&self) -> &str { "linear" }
    fn tools(&self) -> Vec<Arc<dyn Tool>> { all_linear_tools(Arc::clone(&self.client)) }
    async fn health_check(&self) -> bool {
        self.client.graphql("query { viewer { id } }", &serde_json::json!({})).await.is_ok()
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations::linear::tests`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/linear/mod.rs
git commit -m "feat(integrations): implement LinearIntegration"
```

---

### Task 10: Integration Factory and Wiring

Implement `collect_integrations()` in `src/integrations/mod.rs` and wire into the startup path. Integration tools and channels replace the script-based tool registration and the old Slack channel construction.

**Files:**
- Modify: `src/integrations/mod.rs` — add `collect_integrations()`
- Modify: `src/tools/mod.rs:323-353` — skip Slack script tools when integration is active
- Modify: `src/tools/mod.rs:355+` — skip Linear script tools when integration is active
- Modify: `src/channels/factory.rs:298-311` — skip SlackChannel when integration is active
- Modify: startup path (wherever `all_tools` and `collect_configured_channels` are called) — add integration collection
- Test: integration test in `tests/`

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_integrations_returns_empty_for_default_config() {
        let config = Config::default();
        let integrations = collect_integrations(&config);
        assert!(integrations.is_empty());
    }

    #[test]
    fn collect_integrations_returns_slack_when_configured() {
        let mut config = Config::default();
        config.integrations.slack = Some(SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        });
        let integrations = collect_integrations(&config);
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0].name(), "slack");
        assert_eq!(integrations[0].tools().len(), 9);
        assert!(integrations[0].as_channel().is_some());
    }

    #[test]
    fn collect_integrations_returns_linear_when_configured() {
        let mut config = Config::default();
        config.integrations.linear = Some(LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });
        let integrations = collect_integrations(&config);
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0].name(), "linear");
        assert_eq!(integrations[0].tools().len(), 14);
        assert!(integrations[0].as_channel().is_none());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::tests::collect_integrations`
Expected: FAIL — function doesn't exist.

**Step 3: Implement collect_integrations and update wiring**

In `src/integrations/mod.rs`:
```rust
pub fn collect_integrations(config: &Config) -> Vec<Arc<dyn Integration>> {
    let mut integrations: Vec<Arc<dyn Integration>> = Vec::new();

    if let Some(ref slack_config) = config.integrations.slack {
        integrations.push(Arc::new(
            slack::SlackIntegration::new(slack_config.clone())
        ));
    }

    if let Some(ref linear_config) = config.integrations.linear {
        integrations.push(Arc::new(
            linear::LinearIntegration::new(linear_config.clone())
        ));
    }

    integrations
}
```

Update `src/tools/mod.rs` around line 323: Gate the Slack script tool block with `if config.integrations.slack.is_none()` so script tools are skipped when the integration is active. Same for Linear at line 355.

Update `src/channels/factory.rs` around line 298: Gate the `SlackChannel` construction with `if config.integrations.slack.is_none()` so the old channel is skipped when the integration provides it.

Update the startup path (orchestrator or main) to call `collect_integrations()`, extract tools via `.tools()`, and extract channels via `.as_channel()`, appending them to the existing tool and channel lists.

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw`
Expected: PASS — all existing tests still pass, new factory tests pass.

**Step 5: Commit**

```bash
git add src/integrations/ src/tools/mod.rs src/channels/factory.rs
git commit -m "feat(integrations): wire collect_integrations into startup, gate script tools"
```

---

### Task 11: Watch System Tracing

Add `tracing::info!` / `tracing::debug!` calls to `WatchManager` for registration, match, expiry, and cancellation.

**Files:**
- Modify: `src/watches/` — the `WatchManager` implementation
- Test: verify log output with `tracing_subscriber` test utilities or just verify compilation

**Step 1: Identify tracing gaps**

Read `WatchManager` source to locate the functions that lack logging: `register()`, `check_match()`, `expire()`, `cancel()`.

**Step 2: Add tracing calls**

Add `tracing::info!` with structured fields for each lifecycle event:
- Registration: `tracing::info!(watch_id = %id, pattern = %pattern, "watch registered");`
- Match: `tracing::info!(watch_id = %id, "watch matched");`
- Expiry: `tracing::info!(watch_id = %id, "watch expired");`
- Cancellation: `tracing::info!(watch_id = %id, "watch cancelled");`

**Step 3: Run tests**

Run: `cargo test -p zeroclaw`
Expected: PASS — no regressions.

**Step 4: Commit**

```bash
git add src/watches/
git commit -m "feat(watches): add tracing for watch lifecycle events"
```

---

### Task 12: Update Integration Registry for New Integrations

Update the CLI catalog (`src/integrations/catalog_registry.rs`) so the Slack and Linear entries reflect their new status when configured via `config.integrations.*`.

**Files:**
- Modify: `src/integrations/catalog_registry.rs` — update Slack and Linear `status_fn`
- Test: update existing registry tests

**Step 1: Write the failing test**

```rust
#[test]
fn slack_active_via_integration_config() {
    let mut config = Config::default();
    config.integrations.slack = Some(SlackIntegrationConfig {
        bot_token: "xoxb-test".into(),
        app_token: "xapp-test".into(),
        channel_id: None,
        allowed_users: vec![],
        mention_only: true,
        mention_regex: None,
        triage_model: None,
    });
    let entries = all_integrations();
    let slack = entries.iter().find(|e| e.name == "Slack").unwrap();
    assert!(matches!((slack.status_fn)(&config), IntegrationStatus::Active));
}

#[test]
fn linear_active_via_integration_config() {
    let mut config = Config::default();
    config.integrations.linear = Some(LinearIntegrationConfig {
        api_key: "lin_api_test".into(),
    });
    let entries = all_integrations();
    let linear = entries.iter().find(|e| e.name == "Linear").unwrap();
    assert!(matches!((linear.status_fn)(&config), IntegrationStatus::Active));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib integrations::catalog_registry::tests`
Expected: FAIL — status functions don't check `config.integrations.*`.

**Step 3: Update status functions**

For Slack: check `config.integrations.slack.is_some() || config.channels_config.slack.is_some()`.
For Linear: check `config.integrations.linear.is_some()` (was `ComingSoon`, now conditionally `Active`).

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib integrations`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/integrations/catalog_registry.rs
git commit -m "feat(integrations): update CLI catalog for Slack/Linear integration status"
```

---

### Task 13: Full Verification and Cleanup

Final pass: run full test suite, verify all integrations work end-to-end, clean up unused imports.

**Files:**
- All modified files from previous tasks

**Step 1: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS — all tests pass, no warnings.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS — no new warnings.

**Step 3: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: PASS — code formatted.

**Step 4: Verify test count**

Confirm new test count: should be ~60-80 new tests across integrations modules.

**Step 5: Commit any cleanup**

```bash
git add -A
git commit -m "chore: clean up unused imports and fix clippy warnings"
```
