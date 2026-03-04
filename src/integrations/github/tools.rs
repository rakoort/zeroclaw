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

fn resolve_owner(args: &Value, client: &GitHubClient) -> Result<String, ToolResult> {
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
                "repo": { "type": "string", "description": "Repository name" },
                "owner": { "type": "string", "description": "Repository owner (uses default if omitted)" },
                "branch": { "type": "string", "description": "Branch name (uses default branch if omitted)" },
                "limit": { "type": "integer", "description": "Max commits to return (default 20)" }
            },
            "required": ["repo"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let repo = match require_param(&args, "repo") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let owner = match resolve_owner(&args, &self.client) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
        let branch = opt_str(&args, "branch");

        let history_fields = format!(
            "history(first: {limit}) {{ nodes {{ oid messageHeadline author {{ name date }} committedDate }} }}"
        );

        let query = if let Some(ref _branch_name) = branch {
            format!(
                r#"query($owner: String!, $repo: String!, $branch: String!) {{
                    repository(owner: $owner, name: $repo) {{
                        ref(qualifiedName: $branch) {{
                            target {{
                                ... on Commit {{ {history_fields} }}
                            }}
                        }}
                    }}
                }}"#
            )
        } else {
            format!(
                r#"query($owner: String!, $repo: String!) {{
                    repository(owner: $owner, name: $repo) {{
                        defaultBranchRef {{
                            target {{
                                ... on Commit {{ {history_fields} }}
                            }}
                        }}
                    }}
                }}"#
            )
        };

        let variables = if let Some(ref branch_name) = branch {
            json!({"owner": owner, "repo": repo, "branch": branch_name})
        } else {
            json!({"owner": owner, "repo": repo})
        };

        match self.client.graphql(&query, &variables).await {
            Ok(v) => Ok(ok_result(&v)),
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
                "repo": { "type": "string", "description": "Repository name" },
                "owner": { "type": "string", "description": "Repository owner (uses default if omitted)" },
                "state": { "type": "string", "description": "PR state: open, closed, merged (default open)" },
                "author": { "type": "string", "description": "Filter by author login" },
                "limit": { "type": "integer", "description": "Max PRs to return (default 20)" }
            },
            "required": ["repo"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let repo = match require_param(&args, "repo") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let owner = match resolve_owner(&args, &self.client) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);

        let state_raw = opt_str(&args, "state").unwrap_or_else(|| "open".into());
        let state = match state_raw.to_lowercase().as_str() {
            "closed" => "CLOSED",
            "merged" => "MERGED",
            _ => "OPEN",
        };

        let author_filter = if let Some(ref author) = opt_str(&args, "author") {
            format!(r#", filterBy: {{ authorLogin: "{author}" }}"#)
        } else {
            String::new()
        };

        let query = format!(
            r#"query($owner: String!, $repo: String!) {{
                repository(owner: $owner, name: $repo) {{
                    pullRequests(first: {limit}, states: [{state}]{author_filter}, orderBy: {{ field: UPDATED_AT, direction: DESC }}) {{
                        nodes {{
                            number title state author {{ login }} createdAt updatedAt
                            reviewDecision headRefName isDraft
                        }}
                    }}
                }}
            }}"#
        );

        match self
            .client
            .graphql(&query, &json!({"owner": owner, "repo": repo}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
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
                "repo": { "type": "string", "description": "Repository name" },
                "owner": { "type": "string", "description": "Repository owner (uses default if omitted)" },
                "number": { "type": "integer", "description": "Pull request number" }
            },
            "required": ["repo", "number"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let repo = match require_param(&args, "repo") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let owner = match resolve_owner(&args, &self.client) {
            Ok(v) => v,
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
                    number title body state author { login }
                    createdAt updatedAt mergedAt
                    reviewDecision headRefName baseRefName isDraft
                    additions deletions changedFiles
                    labels(first: 20) { nodes { name } }
                    reviews(first: 20) { nodes { author { login } state body } }
                    commits(last: 1) { totalCount nodes { commit { oid messageHeadline } } }
                    files(first: 100) { nodes { path additions deletions } }
                }
            }
        }"#;

        match self
            .client
            .graphql(
                query,
                &json!({"owner": owner, "repo": repo, "number": number}),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── Factory ─────────────────────────────────────────────────────────

/// Return all GitHub tools backed by the given client.
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
            Some("zeroclaw_org".into()),
        ))
    }

    fn mock_client_no_default_owner(server: &MockServer) -> Arc<GitHubClient> {
        Arc::new(GitHubClient::new_with_base_url(
            "ghp_test123".into(),
            server.uri(),
            None,
        ))
    }

    // ── Factory tests ───────────────────────────────────────────────

    #[test]
    fn all_github_tools_returns_3_tools() {
        let client = Arc::new(GitHubClient::new("ghp_test123".into(), None));
        let tools = all_github_tools(client);
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn all_github_tools_have_valid_json_schemas() {
        let client = Arc::new(GitHubClient::new("ghp_test123".into(), None));
        let tools = all_github_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"],
                "object",
                "Tool {} schema must be object",
                tool.name()
            );
        }
    }

    #[test]
    fn all_github_tools_have_unique_names() {
        let client = Arc::new(GitHubClient::new("ghp_test123".into(), None));
        let tools = all_github_tools(client);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 3);
    }

    // ── Missing-param error tests ───────────────────────────────────

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

    // ── Owner resolution tests ──────────────────────────────────────

    #[tokio::test]
    async fn github_commits_owner_required_when_no_default() {
        let server = MockServer::start().await;
        let client = mock_client_no_default_owner(&server);
        let tool = GitHubCommitsTool { client };
        let result = tool.execute(json!({"repo": "zeroclaw"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("owner"));
    }

    // ── Happy-path tests ────────────────────────────────────────────

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
                                        "oid": "abc123def456",
                                        "messageHeadline": "feat: initial commit",
                                        "author": { "name": "zeroclaw_user", "date": "2026-03-01T00:00:00Z" },
                                        "committedDate": "2026-03-01T00:00:00Z"
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
        assert!(result.output.contains("abc123def456"));
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
                                "createdAt": "2026-03-01T00:00:00Z",
                                "updatedAt": "2026-03-02T00:00:00Z",
                                "reviewDecision": "APPROVED",
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
        assert!(result.output.contains("github integration"));
        assert!(result.output.contains("42"));
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
                            "body": "Adds GitHub GraphQL integration tools.",
                            "state": "OPEN",
                            "author": { "login": "zeroclaw_user" },
                            "createdAt": "2026-03-01T00:00:00Z",
                            "updatedAt": "2026-03-02T00:00:00Z",
                            "mergedAt": null,
                            "reviewDecision": "APPROVED",
                            "headRefName": "feat/github-integration",
                            "baseRefName": "main",
                            "isDraft": false,
                            "additions": 150,
                            "deletions": 10,
                            "changedFiles": 3,
                            "labels": { "nodes": [{ "name": "enhancement" }] },
                            "reviews": { "nodes": [{ "author": { "login": "zeroclaw_maintainer" }, "state": "APPROVED", "body": "Looks good" }] },
                            "commits": { "totalCount": 5, "nodes": [{ "commit": { "oid": "abc123", "messageHeadline": "feat: final polish" } }] },
                            "files": { "nodes": [{ "path": "src/tools.rs", "additions": 100, "deletions": 5 }] }
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
        assert!(result.output.contains("github integration"));
        assert!(result.output.contains("APPROVED"));
        assert!(result.output.contains("150"));
        assert!(result.output.contains("enhancement"));
    }

    #[tokio::test]
    async fn github_prs_explicit_owner_overrides_default() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "repository": {
                        "pullRequests": {
                            "nodes": []
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = GitHubPrsTool { client };
        // Provide explicit owner that differs from default "zeroclaw_org"
        let result = tool
            .execute(json!({"repo": "zeroclaw", "owner": "zeroclaw_other_org"}))
            .await
            .unwrap();
        assert!(result.success);
    }
}
