# GitHub Integration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a native GitHub integration with three read-only GraphQL tools (`github_commits`, `github_prs`, `github_pr_detail`) following the Linear integration pattern, plus extend `linear_issues` to include attachment data for PR linkage detection.

**Architecture:** Three files in `src/integrations/github/` (mod.rs, client.rs, tools.rs) mirroring Linear's pattern. GraphQL client with Bearer auth, retry-on-429 using `X-RateLimit-Reset` (unix seconds). Config adds `[integrations.github]` with `token` and optional `owner`. Catalog registry updated from `ComingSoon` to config-aware status.

**Tech Stack:** Rust, reqwest (GraphQL over HTTP), wiremock (tests), serde_json, async-trait, tokio

---

### Task 1: Config — `GitHubIntegrationConfig` struct and wiring

**Files:**
- Modify: `src/config/integrations.rs` (add struct + field on `IntegrationsConfig` + test)
- Modify: `src/config/mod.rs` (add re-export)

**Step 1: Write the failing test**

Add to the `integration_config_tests` module in `src/config/integrations.rs`:

```rust
#[test]
fn github_integration_config_deserializes() {
    let toml_str = r#"
token = "ghp_test123"
owner = "netspore"
"#;
    let config: GitHubIntegrationConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.token, "ghp_test123");
    assert_eq!(config.owner.as_deref(), Some("netspore"));
}

#[test]
fn github_integration_config_owner_optional() {
    let toml_str = r#"
token = "ghp_test123"
"#;
    let config: GitHubIntegrationConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.token, "ghp_test123");
    assert!(config.owner.is_none());
}

#[test]
fn integrations_config_has_github_field() {
    let config = IntegrationsConfig::default();
    assert!(config.github.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib integration_config_tests -- github`
Expected: FAIL — `GitHubIntegrationConfig` not found

**Step 3: Write the struct and wire it**

Add to `src/config/integrations.rs` after `LinearIntegrationConfig`:

```rust
/// GitHub integration configuration (`[integrations.github]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GitHubIntegrationConfig {
    pub token: String,
    #[serde(default)]
    pub owner: Option<String>,
}
```

Add `github` field to `IntegrationsConfig`:

```rust
pub struct IntegrationsConfig {
    pub slack: Option<SlackIntegrationConfig>,
    pub linear: Option<LinearIntegrationConfig>,
    pub github: Option<GitHubIntegrationConfig>,
}
```

Add `GitHubIntegrationConfig` to the re-export list in `src/config/mod.rs` (in the `pub use schema::{...}` block, alphabetically near `GatewayConfig`).

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib integration_config_tests`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/config/integrations.rs src/config/mod.rs
git commit -m "feat(config): add GitHubIntegrationConfig with token and owner fields"
```

---

### Task 2: Client — `GitHubClient` with GraphQL, auth, rate-limit retry

**Files:**
- Create: `src/integrations/github/client.rs`

**Step 1: Write the failing tests**

Create `src/integrations/github/client.rs` with tests only (implementation stubs that fail):

```rust
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::debug;

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
    /// Rate limited — client retried internally and exhausted retries.
    RateLimited { reset_at: u64 },
    /// HTTP 401 or token-related error.
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
}

impl GitHubClient {
    /// Production constructor.
    pub fn new(token: String, default_owner: Option<String>) -> Self {
        Self::new_with_base_url(token, "https://api.github.com".into(), default_owner)
    }

    /// Test constructor — caller supplies a wiremock base URL.
    pub fn new_with_base_url(
        token: String,
        base_url: String,
        default_owner: Option<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            base_url,
            default_owner,
        }
    }

    /// Returns the configured default owner, if any.
    pub fn default_owner(&self) -> Option<&str> {
        self.default_owner.as_deref()
    }

    /// Execute a GraphQL query against the GitHub API.
    ///
    /// GitHub uses `Authorization: Bearer {token}` and requires a `User-Agent` header.
    /// Rate limiting: GitHub returns `X-RateLimit-Remaining` and `X-RateLimit-Reset`
    /// (unix epoch seconds). On 403 with remaining=0 or 429, sleep until reset.
    pub async fn graphql(&self, query: &str, variables: &Value) -> Result<Value, GitHubApiError> {
        let url = format!("{}/graphql", self.base_url);
        let body = json!({ "query": query, "variables": variables });
        let mut retries = 0u32;

        loop {
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

            // Rate limiting — GitHub returns 429 or 403 with X-RateLimit-Remaining: 0
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
                    return Err(GitHubApiError::RateLimited {
                        reset_at: reset_secs,
                    });
                }

                let wait = compute_wait_from_reset_secs(reset_secs);
                debug!(reset_secs, ?wait, retries, "github rate limited, retrying");
                tokio::time::sleep(wait).await;
                retries += 1;
                continue;
            }

            // Auth errors (non-rate-limit 401/403).
            if status == 401 || status == 403 {
                let text = resp.text().await.unwrap_or_default();
                return Err(GitHubApiError::AuthError { message: text });
            }

            let json: Value = resp.json().await.map_err(GitHubApiError::Network)?;

            // GraphQL errors.
            if let Some(errors) = json.get("errors") {
                if let Ok(errs) =
                    serde_json::from_value::<Vec<GitHubGraphqlError>>(errors.clone())
                {
                    if !errs.is_empty() {
                        return Err(GitHubApiError::GraphqlErrors { errors: errs });
                    }
                }
            }

            // Return `data` directly.
            match json.get("data").cloned() {
                Some(data) => return Ok(data),
                None => return Ok(json),
            }
        }
    }
}

/// Compute wait duration from a reset timestamp in unix epoch seconds.
#[allow(clippy::cast_possible_truncation)]
fn compute_wait_from_reset_secs(reset_secs: u64) -> Duration {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    if reset_secs > now_secs {
        Duration::from_secs(reset_secs - now_secs)
    } else {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client(server: &MockServer) -> GitHubClient {
        GitHubClient::new_with_base_url(
            "ghp_test123".into(),
            server.uri(),
            Some("netspore".into()),
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
            .and(header("User-Agent", "zeroclaw"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": {}})),
            )
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": {}})),
            )
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": {}})),
            )
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
            .respond_with(
                ResponseTemplate::new(401).set_body_string("Bad credentials"),
            )
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
            message: "Bad credentials".into(),
        };
        assert!(e.to_string().contains("Bad credentials"));

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
        let client = GitHubClient::new("ghp_test".into(), Some("netspore".into()));
        assert_eq!(client.default_owner(), Some("netspore"));
    }

    #[test]
    fn default_owner_returns_none_when_not_set() {
        let client = GitHubClient::new("ghp_test".into(), None);
        assert!(client.default_owner().is_none());
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
}
```

**Step 2: Create the module file so it compiles**

Create `src/integrations/github/mod.rs` with a temporary stub:

```rust
pub mod client;
```

Do NOT add `pub mod github;` to `src/integrations/mod.rs` yet — do that in Task 4. For now, run client tests directly:

Run: `cargo test --lib integrations::github::client`

This won't work until the module is registered. Instead, add `pub mod github;` to `src/integrations/mod.rs` now (just the module declaration, no collect_integrations changes yet).

**Step 3: Run tests to verify they pass**

Run: `cargo test --lib integrations::github::client`
Expected: all PASS

**Step 4: Commit**

```bash
git add src/integrations/github/client.rs src/integrations/github/mod.rs src/integrations/mod.rs
git commit -m "feat(github): add GitHubClient with GraphQL, Bearer auth, and rate-limit retry"
```

---

### Task 3: Tools — `github_commits`, `github_prs`, `github_pr_detail`

**Files:**
- Create: `src/integrations/github/tools.rs`
- Modify: `src/integrations/github/mod.rs` (add `pub mod tools;`)

**Step 1: Write failing tests first, then implementation**

Create `src/integrations/github/tools.rs` with the full implementation and tests:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::client::{GitHubApiError, GitHubClient};
use crate::tools::traits::{Tool, ToolResult};

// ── Helpers ─────────────────────────────────────────────────────────

fn require_param<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolResult> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("missing required parameter: {key}")),
        })
}

fn err_result(e: GitHubApiError) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(e.to_string()),
    }
}

fn ok_result(value: &Value) -> ToolResult {
    ToolResult {
        success: true,
        output: serde_json::to_string_pretty(value).unwrap_or_default(),
        error: None,
    }
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Resolve owner: explicit param > client default > error.
fn resolve_owner<'a>(
    args: &'a Value,
    client: &'a GitHubClient,
) -> Result<String, ToolResult> {
    if let Some(owner) = opt_str(args, "owner") {
        return Ok(owner);
    }
    if let Some(default) = client.default_owner() {
        return Ok(default.to_string());
    }
    Err(ToolResult {
        success: false,
        output: String::new(),
        error: Some("missing required parameter: owner (no default configured)".into()),
    })
}

// ── GitHubCommitsTool ───────────────────────────────────────────────

pub struct GitHubCommitsTool {
    pub(crate) client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for GitHubCommitsTool {
    fn name(&self) -> &str {
        "github_commits"
    }
    fn description(&self) -> &str {
        "List recent commits on a branch"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "owner": { "type": "string", "description": "Repository owner (org or user). Falls back to config default." },
                "repo": { "type": "string", "description": "Repository name" },
                "branch": { "type": "string", "description": "Branch name (default: repo default branch)" },
                "limit": { "type": "integer", "description": "Max commits to return (default 20)" }
            },
            "required": ["repo"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let owner = match resolve_owner(&args, &self.client) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let repo = match require_param(&args, "repo") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
        let branch = opt_str(&args, "branch");

        let (query, variables) = if let Some(ref branch_name) = branch {
            (
                r#"query($owner: String!, $repo: String!, $branch: String!, $limit: Int!) {
  repository(owner: $owner, name: $repo) {
    ref(qualifiedName: $branch) {
      target {
        ... on Commit {
          history(first: $limit) {
            nodes {
              oid
              messageHeadline
              author { name date }
              committedDate
            }
          }
        }
      }
    }
  }
}"#,
                json!({
                    "owner": owner,
                    "repo": repo,
                    "branch": branch_name,
                    "limit": limit
                }),
            )
        } else {
            (
                r#"query($owner: String!, $repo: String!, $limit: Int!) {
  repository(owner: $owner, name: $repo) {
    defaultBranchRef {
      target {
        ... on Commit {
          history(first: $limit) {
            nodes {
              oid
              messageHeadline
              author { name date }
              committedDate
            }
          }
        }
      }
    }
  }
}"#,
                json!({
                    "owner": owner,
                    "repo": repo,
                    "limit": limit
                }),
            )
        };

        match self.client.graphql(query, &variables).await {
            Ok(data) => Ok(ok_result(&data)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── GitHubPrsTool ───────────────────────────────────────────────────

pub struct GitHubPrsTool {
    pub(crate) client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for GitHubPrsTool {
    fn name(&self) -> &str {
        "github_prs"
    }
    fn description(&self) -> &str {
        "List pull requests with review status"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "owner": { "type": "string", "description": "Repository owner. Falls back to config default." },
                "repo": { "type": "string", "description": "Repository name" },
                "state": { "type": "string", "description": "PR state: open, closed, or merged (default: open)" },
                "author": { "type": "string", "description": "Filter by author login" },
                "limit": { "type": "integer", "description": "Max PRs to return (default 20)" }
            },
            "required": ["repo"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let owner = match resolve_owner(&args, &self.client) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let repo = match require_param(&args, "repo") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
        let state = opt_str(&args, "state").unwrap_or_else(|| "open".into());

        let gql_state = match state.to_lowercase().as_str() {
            "open" => "OPEN",
            "closed" => "CLOSED",
            "merged" => "MERGED",
            _ => "OPEN",
        };

        let query = r#"query($owner: String!, $repo: String!, $states: [PullRequestState!], $limit: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequests(first: $limit, states: $states, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        state
        author { login }
        createdAt
        updatedAt
        reviewDecision
        headRefName
        isDraft
      }
    }
  }
}"#;

        let variables = json!({
            "owner": owner,
            "repo": repo,
            "states": [gql_state],
            "limit": limit
        });

        match self.client.graphql(query, &variables).await {
            Ok(data) => Ok(ok_result(&data)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── GitHubPrDetailTool ──────────────────────────────────────────────

pub struct GitHubPrDetailTool {
    pub(crate) client: Arc<GitHubClient>,
}

#[async_trait]
impl Tool for GitHubPrDetailTool {
    fn name(&self) -> &str {
        "github_pr_detail"
    }
    fn description(&self) -> &str {
        "Get detailed information about a single pull request"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "owner": { "type": "string", "description": "Repository owner. Falls back to config default." },
                "repo": { "type": "string", "description": "Repository name" },
                "number": { "type": "integer", "description": "Pull request number" }
            },
            "required": ["repo", "number"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let owner = match resolve_owner(&args, &self.client) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let repo = match require_param(&args, "repo") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let number = match args.get("number").and_then(Value::as_i64) {
            Some(n) => n,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: number".into()),
                })
            }
        };

        let query = r#"query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      number
      title
      body
      state
      author { login }
      createdAt
      updatedAt
      mergedAt
      reviewDecision
      headRefName
      baseRefName
      isDraft
      additions
      deletions
      changedFiles
      labels(first: 10) {
        nodes { name }
      }
      reviews(first: 20) {
        nodes {
          author { login }
          state
          submittedAt
        }
      }
      commits(last: 1) {
        totalCount
        nodes {
          commit {
            oid
            messageHeadline
            committedDate
          }
        }
      }
      files(first: 50) {
        nodes {
          path
          additions
          deletions
        }
      }
    }
  }
}"#;

        let variables = json!({
            "owner": owner,
            "repo": repo,
            "number": number
        });

        match self.client.graphql(query, &variables).await {
            Ok(data) => Ok(ok_result(&data)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── Factory ─────────────────────────────────────────────────────────

pub fn all_github_tools(client: Arc<GitHubClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(GitHubCommitsTool {
            client: Arc::clone(&client),
        }),
        Arc::new(GitHubPrsTool {
            client: Arc::clone(&client),
        }),
        Arc::new(GitHubPrDetailTool { client }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_client(server: &MockServer) -> Arc<GitHubClient> {
        Arc::new(GitHubClient::new_with_base_url(
            "ghp_test123".into(),
            server.uri(),
            Some("netspore".into()),
        ))
    }

    // ── Schema / factory tests ──────────────────────────────────────

    #[test]
    fn all_github_tools_returns_3_tools() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn all_github_tools_have_valid_json_schemas() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"], "object",
                "Tool {} schema must be object",
                tool.name()
            );
        }
    }

    #[test]
    fn all_github_tools_have_unique_names() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        let original_len = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), original_len);
    }

    // ── Missing required params ─────────────────────────────────────

    #[tokio::test]
    async fn github_commits_missing_repo_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = GitHubCommitsTool { client };
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("repo"));
    }

    #[tokio::test]
    async fn github_prs_missing_repo_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = GitHubPrsTool { client };
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("repo"));
    }

    #[tokio::test]
    async fn github_pr_detail_missing_repo_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = GitHubPrDetailTool { client };
        let result = tool.execute(json!({"number": 1})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("repo"));
    }

    #[tokio::test]
    async fn github_pr_detail_missing_number_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = GitHubPrDetailTool { client };
        let result = tool.execute(json!({"repo": "zeroclaw"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("number"));
    }

    #[tokio::test]
    async fn github_commits_owner_required_when_no_default() {
        let server = MockServer::start().await;
        let client = Arc::new(GitHubClient::new_with_base_url(
            "ghp_test".into(),
            server.uri(),
            None,
        ));
        let tool = GitHubCommitsTool { client };
        let result = tool.execute(json!({"repo": "zeroclaw"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("owner"));
    }

    // ── Happy path ──────────────────────────────────────────────────

    #[tokio::test]
    async fn github_commits_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "repository": {
                        "defaultBranchRef": {
                            "target": {
                                "history": {
                                    "nodes": [{
                                        "oid": "abc123",
                                        "messageHeadline": "feat: initial commit",
                                        "author": { "name": "zeroclaw_user", "date": "2026-03-04T10:00:00Z" },
                                        "committedDate": "2026-03-04T10:00:00Z"
                                    }]
                                }
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = GitHubCommitsTool { client };
        let result = tool.execute(json!({"repo": "zeroclaw"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("abc123"));
        assert!(result.output.contains("initial commit"));
    }

    #[tokio::test]
    async fn github_prs_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "repository": {
                        "pullRequests": {
                            "nodes": [{
                                "number": 42,
                                "title": "feat: add github integration",
                                "state": "OPEN",
                                "author": { "login": "zeroclaw_user" },
                                "createdAt": "2026-03-04T10:00:00Z",
                                "updatedAt": "2026-03-04T12:00:00Z",
                                "reviewDecision": "REVIEW_REQUIRED",
                                "headRefName": "feat/github-integration",
                                "isDraft": false
                            }]
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = GitHubPrsTool { client };
        let result = tool.execute(json!({"repo": "zeroclaw"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("42"));
        assert!(result.output.contains("github integration"));
    }

    #[tokio::test]
    async fn github_pr_detail_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "repository": {
                        "pullRequest": {
                            "number": 42,
                            "title": "feat: add github integration",
                            "body": "Adds three read-only GitHub tools",
                            "state": "OPEN",
                            "author": { "login": "zeroclaw_user" },
                            "createdAt": "2026-03-04T10:00:00Z",
                            "updatedAt": "2026-03-04T12:00:00Z",
                            "mergedAt": null,
                            "reviewDecision": "APPROVED",
                            "headRefName": "feat/github-integration",
                            "baseRefName": "main",
                            "isDraft": false,
                            "additions": 340,
                            "deletions": 12,
                            "changedFiles": 8,
                            "labels": { "nodes": [{ "name": "enhancement" }] },
                            "reviews": { "nodes": [{ "author": { "login": "reviewer_user" }, "state": "APPROVED", "submittedAt": "2026-03-04T11:00:00Z" }] },
                            "commits": { "totalCount": 5, "nodes": [{ "commit": { "oid": "def456", "messageHeadline": "chore: final cleanup", "committedDate": "2026-03-04T11:30:00Z" } }] },
                            "files": { "nodes": [{ "path": "src/integrations/github/mod.rs", "additions": 45, "deletions": 0 }] }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = GitHubPrDetailTool { client };
        let result = tool
            .execute(json!({"repo": "zeroclaw", "number": 42}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("340"));
        assert!(result.output.contains("APPROVED"));
        assert!(result.output.contains("def456"));
    }

    #[tokio::test]
    async fn github_prs_explicit_owner_overrides_default() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "repository": { "pullRequests": { "nodes": [] } } }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = GitHubPrsTool { client };
        // Explicit owner should work even though default is also set
        let result = tool
            .execute(json!({"owner": "other-org", "repo": "other-repo"}))
            .await
            .unwrap();
        assert!(result.success);
    }
}
```

Add `pub mod tools;` to `src/integrations/github/mod.rs`.

**Step 2: Run tests to verify they pass**

Run: `cargo test --lib integrations::github::tools`
Expected: all PASS

**Step 3: Commit**

```bash
git add src/integrations/github/tools.rs src/integrations/github/mod.rs
git commit -m "feat(github): add github_commits, github_prs, github_pr_detail tools"
```

---

### Task 4: Integration struct and wiring into `collect_integrations`

**Files:**
- Modify: `src/integrations/github/mod.rs` (add `GitHubIntegration` struct + `Integration` impl)
- Modify: `src/integrations/mod.rs` (add to `collect_integrations`)

**Step 1: Write failing tests**

Add tests to `src/integrations/github/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::client::GitHubClient;
    use super::tools::all_github_tools;
    use super::GitHubIntegration;
    use crate::config::GitHubIntegrationConfig;
    use crate::integrations::Integration;
    use std::sync::Arc;

    #[test]
    fn all_github_tools_returns_3_tools() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn all_github_tools_have_valid_json_schemas() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"], "object",
                "Tool {} schema must be object",
                tool.name()
            );
        }
    }

    #[test]
    fn github_integration_name() {
        let config = GitHubIntegrationConfig {
            token: "ghp_test".into(),
            owner: None,
        };
        let integration = GitHubIntegration::new(config);
        assert_eq!(integration.name(), "github");
    }

    #[test]
    fn github_integration_returns_3_tools() {
        let config = GitHubIntegrationConfig {
            token: "ghp_test".into(),
            owner: None,
        };
        let integration = GitHubIntegration::new(config);
        assert_eq!(integration.tools().len(), 3);
    }

    #[test]
    fn github_integration_as_channel_returns_none() {
        let config = GitHubIntegrationConfig {
            token: "ghp_test".into(),
            owner: None,
        };
        let integration = GitHubIntegration::new(config);
        assert!(integration.as_channel().is_none());
    }
}
```

Add test to `src/integrations/mod.rs` tests module:

```rust
#[test]
fn collect_integrations_returns_github_when_configured() {
    let mut config = crate::config::Config::default();
    config.integrations.github = Some(crate::config::GitHubIntegrationConfig {
        token: "ghp_test".into(),
        owner: None,
    });
    let integrations = collect_integrations(&config);
    assert_eq!(integrations.len(), 1);
    assert_eq!(integrations[0].name(), "github");
    assert_eq!(integrations[0].tools().len(), 3);
    assert!(integrations[0].as_channel().is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib integrations::github::tests -- github_integration`
Expected: FAIL — `GitHubIntegration` not found

**Step 3: Implement `GitHubIntegration` struct**

Update `src/integrations/github/mod.rs`:

```rust
pub mod client;
pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::config::GitHubIntegrationConfig;
use crate::integrations::Integration;
use crate::tools::traits::Tool;

use self::client::GitHubClient;
use self::tools::all_github_tools;

/// Native GitHub integration — provides 3 read-only tools, no channel.
pub struct GitHubIntegration {
    client: Arc<GitHubClient>,
}

impl GitHubIntegration {
    pub fn new(config: GitHubIntegrationConfig) -> Self {
        Self {
            client: Arc::new(GitHubClient::new(config.token, config.owner)),
        }
    }
}

#[async_trait]
impl Integration for GitHubIntegration {
    fn name(&self) -> &str {
        "github"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        all_github_tools(Arc::clone(&self.client))
    }

    async fn health_check(&self) -> bool {
        self.client
            .graphql("query { viewer { login } }", &json!({}))
            .await
            .is_ok()
    }
}
```

Add GitHub to `collect_integrations` in `src/integrations/mod.rs`:

```rust
if let Some(ref github_config) = config.integrations.github {
    integrations.push(Arc::new(github::GitHubIntegration::new(
        github_config.clone(),
    )));
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib integrations::github && cargo test --lib integrations::tests`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/integrations/github/mod.rs src/integrations/mod.rs
git commit -m "feat(github): add GitHubIntegration struct and wire into collect_integrations"
```

---

### Task 5: Catalog registry — update GitHub from `ComingSoon` to config-aware

**Files:**
- Modify: `src/integrations/catalog_registry.rs`

**Step 1: Write failing test**

Add to `src/integrations/catalog_registry.rs` tests:

```rust
#[test]
fn github_active_when_integration_configured() {
    let mut config = Config::default();
    config.integrations.github = Some(crate::config::GitHubIntegrationConfig {
        token: "ghp_test".into(),
        owner: None,
    });
    let entries = all_integrations();
    let gh = entries.iter().find(|e| e.name == "GitHub").unwrap();
    assert!(matches!(
        (gh.status_fn)(&config),
        IntegrationStatus::Active
    ));
}

#[test]
fn github_available_when_not_configured() {
    let config = Config::default();
    let entries = all_integrations();
    let gh = entries.iter().find(|e| e.name == "GitHub").unwrap();
    assert!(matches!(
        (gh.status_fn)(&config),
        IntegrationStatus::Available
    ));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib catalog_registry -- github`
Expected: FAIL — GitHub is still `ComingSoon`

**Step 3: Update the catalog entry**

In `src/integrations/catalog_registry.rs`, change the GitHub entry from:

```rust
status_fn: |_| IntegrationStatus::ComingSoon,
```

to:

```rust
status_fn: |c| {
    if c.integrations.github.is_some() {
        IntegrationStatus::Active
    } else {
        IntegrationStatus::Available
    }
},
```

Also update the `coming_soon_integrations_stay_coming_soon` test — remove `"GitHub"` from the checked list if it's there (it's not — the test checks `"Nostr", "Spotify", "Home Assistant"` so no change needed).

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib catalog_registry`
Expected: all PASS

**Step 5: Commit**

```bash
git add src/integrations/catalog_registry.rs
git commit -m "feat(github): update catalog registry from ComingSoon to config-aware status"
```

---

### Task 6: Linear enhancement — add attachments to `linear_issues` query

**Files:**
- Modify: `src/integrations/linear/tools.rs` (update `LinearIssuesTool` query and test)

**Step 1: Write failing test**

Add a test that checks the GraphQL query sent by `linear_issues` includes the `attachments` field. Use wiremock request inspection:

```rust
#[tokio::test]
async fn linear_issues_query_includes_attachments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "team": {
                    "issues": {
                        "nodes": [{
                            "id": "issue_1",
                            "identifier": "SPO-45",
                            "title": "Test issue",
                            "state": { "name": "In Progress" },
                            "assignee": { "name": "zeroclaw_user" },
                            "priority": 1,
                            "attachments": {
                                "nodes": [{
                                    "url": "https://github.com/netspore/zeroclaw/pull/42",
                                    "title": "PR #42",
                                    "subtitle": "feat: github integration",
                                    "sourceType": "github"
                                }]
                            }
                        }]
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    let client = mock_client(&server);
    let tool = LinearIssuesTool { client };
    let result = tool.execute(json!({"team_id": "team_1"})).await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("attachments"));
    assert!(result.output.contains("github"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib integrations::linear::tools -- linear_issues_query_includes_attachments`
Expected: FAIL — output doesn't contain "attachments"

**Step 3: Update the GraphQL query**

In `src/integrations/linear/tools.rs`, find the `LinearIssuesTool` execute method's GraphQL query and add the `attachments` field after `priority`:

Add `attachments { nodes { url title subtitle sourceType } }` to the issues query, inside the `nodes` selection.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib integrations::linear::tools`
Expected: all PASS (existing tests should still pass since the response data is a superset)

**Step 5: Commit**

```bash
git add src/integrations/linear/tools.rs
git commit -m "feat(linear): add attachments field to linear_issues query for PR linkage detection"
```

---

### Task 7: Full validation — `cargo fmt`, `cargo clippy`, `cargo test`

**Files:** None (validation only)

**Step 1: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: no formatting issues (fix any if found)

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings

**Step 3: Run full test suite**

Run: `cargo test`
Expected: all tests PASS

**Step 4: If any issues, fix and re-run**

Fix any compilation errors, lint warnings, or test failures before proceeding.

**Step 5: No commit needed (validation only)**

---

### Task 8: Integration tests for `collect_integrations` with GitHub + Linear together

**Files:**
- Modify: `src/integrations/mod.rs` (add test)

**Step 1: Write the test**

Add to `src/integrations/mod.rs` tests:

```rust
#[test]
fn collect_integrations_returns_both_linear_and_github_when_configured() {
    let mut config = crate::config::Config::default();
    config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
        api_key: "lin_api_test".into(),
    });
    config.integrations.github = Some(crate::config::GitHubIntegrationConfig {
        token: "ghp_test".into(),
        owner: Some("netspore".into()),
    });
    let integrations = collect_integrations(&config);
    assert_eq!(integrations.len(), 2);
    let names: Vec<&str> = integrations.iter().map(|i| i.name()).collect();
    assert!(names.contains(&"linear"));
    assert!(names.contains(&"github"));
}

#[test]
fn build_integration_tool_map_includes_github_when_configured() {
    let mut config = crate::config::Config::default();
    config.integrations.github = Some(crate::config::GitHubIntegrationConfig {
        token: "ghp_test".into(),
        owner: None,
    });
    let map = build_integration_tool_map(&config);
    assert!(map.contains_key("github"));
    assert_eq!(map["github"].len(), 3);
}
```

**Step 2: Run tests**

Run: `cargo test --lib integrations::tests`
Expected: all PASS

**Step 3: Commit**

```bash
git add src/integrations/mod.rs
git commit -m "test(github): add integration tests for collect_integrations with GitHub"
```
