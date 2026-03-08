use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::client::{LinearApiError, LinearClient};
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

fn err_result(e: LinearApiError) -> ToolResult {
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

// ── LinearIssuesTool ────────────────────────────────────────────────

pub struct LinearIssuesTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearIssuesTool {
    fn name(&self) -> &str {
        "linear_issues"
    }
    fn description(&self) -> &str {
        "List issues for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" },
                "limit": { "type": "integer", "description": "Max issues (default 50)" },
                "state": { "type": "string", "description": "Filter by state name" }
            },
            "required": ["team_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
        let state_filter = opt_str(&args, "state");

        let filter = if let Some(ref state) = state_filter {
            format!(r#", filter: {{ state: {{ name: {{ eq: "{state}" }} }} }}"#)
        } else {
            String::new()
        };

        let query = format!(
            r#"query($teamId: String!) {{
                team(id: $teamId) {{
                    issues(first: {limit}{filter}) {{
                        nodes {{ id identifier title state {{ name }} assignee {{ name }} priority attachments {{ nodes {{ url title subtitle sourceType }} }} }}
                    }}
                }}
            }}"#
        );

        match self
            .client
            .graphql(&query, &json!({"teamId": team_id}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearCreateIssueTool ───────────────────────────────────────────

pub struct LinearCreateIssueTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearCreateIssueTool {
    fn name(&self) -> &str {
        "linear_create_issue"
    }
    fn description(&self) -> &str {
        "Create a new Linear issue"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" },
                "title": { "type": "string", "description": "Issue title" },
                "description": { "type": "string", "description": "Issue description (markdown)" }
            },
            "required": ["team_id", "title"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let title = match require_param(&args, "title") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let description = opt_str(&args, "description");

        let mut input = json!({"teamId": team_id, "title": title});
        if let Some(desc) = description {
            input["description"] = json!(desc);
        }

        let query = r#"mutation($input: IssueCreateInput!) {
            issueCreate(input: $input) {
                success
                issue { id identifier title url }
            }
        }"#;

        match self.client.graphql(query, &json!({"input": input})).await {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearUpdateIssueTool ───────────────────────────────────────────

pub struct LinearUpdateIssueTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearUpdateIssueTool {
    fn name(&self) -> &str {
        "linear_update_issue"
    }
    fn description(&self) -> &str {
        "Update an existing Linear issue"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "issue_id": { "type": "string", "description": "Linear issue ID" },
                "title": { "type": "string", "description": "New title" },
                "description": { "type": "string", "description": "New description" },
                "state_id": { "type": "string", "description": "New state ID" },
                "assignee_id": { "type": "string", "description": "New assignee ID" }
            },
            "required": ["issue_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let issue_id = match require_param(&args, "issue_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };

        let mut input = json!({});
        if let Some(v) = opt_str(&args, "title") {
            input["title"] = json!(v);
        }
        if let Some(v) = opt_str(&args, "description") {
            input["description"] = json!(v);
        }
        if let Some(v) = opt_str(&args, "state_id") {
            input["stateId"] = json!(v);
        }
        if let Some(v) = opt_str(&args, "assignee_id") {
            input["assigneeId"] = json!(v);
        }

        let query = r#"mutation($id: String!, $input: IssueUpdateInput!) {
            issueUpdate(id: $id, input: $input) {
                success
                issue { id identifier title state { name } }
            }
        }"#;

        match self
            .client
            .graphql(query, &json!({"id": issue_id, "input": input}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearArchiveIssueTool ──────────────────────────────────────────

pub struct LinearArchiveIssueTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearArchiveIssueTool {
    fn name(&self) -> &str {
        "linear_archive_issue"
    }
    fn description(&self) -> &str {
        "Archive a Linear issue"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "issue_id": { "type": "string", "description": "Linear issue ID" }
            },
            "required": ["issue_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let issue_id = match require_param(&args, "issue_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };

        let query = r#"mutation($id: String!) {
            issueArchive(id: $id) { success }
        }"#;

        match self.client.graphql(query, &json!({"id": issue_id})).await {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearAddCommentTool ────────────────────────────────────────────

pub struct LinearAddCommentTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearAddCommentTool {
    fn name(&self) -> &str {
        "linear_add_comment"
    }
    fn description(&self) -> &str {
        "Add a comment to a Linear issue"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "issue_id": { "type": "string", "description": "Linear issue ID" },
                "body": { "type": "string", "description": "Comment text (markdown)" }
            },
            "required": ["issue_id", "body"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let issue_id = match require_param(&args, "issue_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let body = match require_param(&args, "body") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        let query = r#"mutation($input: CommentCreateInput!) {
            commentCreate(input: $input) {
                success
                comment { id body }
            }
        }"#;

        match self
            .client
            .graphql(
                query,
                &json!({"input": {"issueId": issue_id, "body": body}}),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearTeamsTool ─────────────────────────────────────────────────

pub struct LinearTeamsTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearTeamsTool {
    fn name(&self) -> &str {
        "linear_teams"
    }
    fn description(&self) -> &str {
        "List all teams in the Linear workspace"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let query = r#"query {
            teams { nodes { id name key } }
        }"#;
        match self.client.graphql(query, &json!({})).await {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearUsersTool ─────────────────────────────────────────────────

pub struct LinearUsersTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearUsersTool {
    fn name(&self) -> &str {
        "linear_users"
    }
    fn description(&self) -> &str {
        "List members of a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" }
            },
            "required": ["team_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let query = r#"query($teamId: String!) {
            team(id: $teamId) {
                members { nodes { id name email displayName } }
            }
        }"#;
        match self
            .client
            .graphql(query, &json!({"teamId": team_id}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearProjectsTool ──────────────────────────────────────────────

pub struct LinearProjectsTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearProjectsTool {
    fn name(&self) -> &str {
        "linear_projects"
    }
    fn description(&self) -> &str {
        "List projects for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" }
            },
            "required": ["team_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let query = r#"query($teamId: String!) {
            team(id: $teamId) {
                projects { nodes { id name state } }
            }
        }"#;
        match self
            .client
            .graphql(query, &json!({"teamId": team_id}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearCyclesTool ────────────────────────────────────────────────

pub struct LinearCyclesTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearCyclesTool {
    fn name(&self) -> &str {
        "linear_cycles"
    }
    fn description(&self) -> &str {
        "List cycles for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" }
            },
            "required": ["team_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let query = r#"query($teamId: String!) {
            team(id: $teamId) {
                cycles { nodes { id name number startsAt endsAt } }
            }
        }"#;
        match self
            .client
            .graphql(query, &json!({"teamId": team_id}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearLabelsTool ────────────────────────────────────────────────

pub struct LinearLabelsTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearLabelsTool {
    fn name(&self) -> &str {
        "linear_labels"
    }
    fn description(&self) -> &str {
        "List labels for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" }
            },
            "required": ["team_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let query = r#"query($teamId: String!) {
            team(id: $teamId) {
                labels { nodes { id name color } }
            }
        }"#;
        match self
            .client
            .graphql(query, &json!({"teamId": team_id}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearStatesTool ────────────────────────────────────────────────

pub struct LinearStatesTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearStatesTool {
    fn name(&self) -> &str {
        "linear_states"
    }
    fn description(&self) -> &str {
        "List workflow states for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" }
            },
            "required": ["team_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let query = r#"query($teamId: String!) {
            team(id: $teamId) {
                states { nodes { id name type color position } }
            }
        }"#;
        match self
            .client
            .graphql(query, &json!({"teamId": team_id}))
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearCreateLabelTool ───────────────────────────────────────────

pub struct LinearCreateLabelTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearCreateLabelTool {
    fn name(&self) -> &str {
        "linear_create_label"
    }
    fn description(&self) -> &str {
        "Create a label for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" },
                "name": { "type": "string", "description": "Label name" },
                "color": { "type": "string", "description": "Hex color (e.g. #ff0000)" }
            },
            "required": ["team_id", "name"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let name = match require_param(&args, "name") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let color = opt_str(&args, "color");

        let mut input = json!({"teamId": team_id, "name": name});
        if let Some(c) = color {
            input["color"] = json!(c);
        }

        let query = r#"mutation($input: IssueLabelCreateInput!) {
            issueLabelCreate(input: $input) {
                success
                issueLabel { id name color }
            }
        }"#;
        match self.client.graphql(query, &json!({"input": input})).await {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearCreateProjectTool ─────────────────────────────────────────

pub struct LinearCreateProjectTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearCreateProjectTool {
    fn name(&self) -> &str {
        "linear_create_project"
    }
    fn description(&self) -> &str {
        "Create a project for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" },
                "name": { "type": "string", "description": "Project name" },
                "description": { "type": "string", "description": "Project description" }
            },
            "required": ["team_id", "name"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let name = match require_param(&args, "name") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let description = opt_str(&args, "description");

        let mut input = json!({"teamIds": [team_id], "name": name});
        if let Some(d) = description {
            input["description"] = json!(d);
        }

        let query = r#"mutation($input: ProjectCreateInput!) {
            projectCreate(input: $input) {
                success
                project { id name url }
            }
        }"#;
        match self.client.graphql(query, &json!({"input": input})).await {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── LinearCreateCycleTool ───────────────────────────────────────────

pub struct LinearCreateCycleTool {
    pub(crate) client: Arc<LinearClient>,
}

#[async_trait]
impl Tool for LinearCreateCycleTool {
    fn name(&self) -> &str {
        "linear_create_cycle"
    }
    fn description(&self) -> &str {
        "Create a cycle for a Linear team"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" },
                "name": { "type": "string", "description": "Cycle name" },
                "start_date": { "type": "string", "description": "Start date (ISO 8601)" },
                "end_date": { "type": "string", "description": "End date (ISO 8601)" }
            },
            "required": ["team_id", "name", "start_date", "end_date"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let team_id = match require_param(&args, "team_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let name = match require_param(&args, "name") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let start_date = match require_param(&args, "start_date") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let end_date = match require_param(&args, "end_date") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        let query = r#"mutation($input: CycleCreateInput!) {
            cycleCreate(input: $input) {
                success
                cycle { id name number startsAt endsAt }
            }
        }"#;
        let input = json!({
            "teamId": team_id,
            "name": name,
            "startsAt": start_date,
            "endsAt": end_date
        });
        match self.client.graphql(query, &json!({"input": input})).await {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── Factory ─────────────────────────────────────────────────────────

/// Return all 14 Linear tools backed by the given client.
pub fn all_linear_tools(client: Arc<LinearClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(LinearIssuesTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearCreateIssueTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearUpdateIssueTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearArchiveIssueTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearAddCommentTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearTeamsTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearUsersTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearProjectsTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearCyclesTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearLabelsTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearStatesTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearCreateLabelTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearCreateProjectTool {
            client: Arc::clone(&client),
        }),
        Arc::new(LinearCreateCycleTool { client }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::noop::NoopObserver;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_client(server: &MockServer) -> Arc<LinearClient> {
        Arc::new(LinearClient::new_with_base_url(
            "lin_api_test".into(),
            server.uri(),
            Arc::new(NoopObserver),
        ))
    }

    #[tokio::test]
    async fn linear_create_issue_missing_team_id_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = LinearCreateIssueTool { client };
        let result = tool.execute(json!({"title": "Bug"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("team_id"));
    }

    #[tokio::test]
    async fn linear_create_issue_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"issueCreate": {"success": true, "issue": {"id": "ISS-1", "identifier": "ENG-1", "title": "Bug fix", "url": "https://linear.app/issue/ENG-1"}}}
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = LinearCreateIssueTool { client };
        let result = tool
            .execute(json!({"team_id": "team_1", "title": "Bug fix"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn linear_teams_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"teams": {"nodes": [{"id": "t1", "name": "Engineering", "key": "ENG"}]}}
            })))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = LinearTeamsTool { client };
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Engineering"));
    }

    #[tokio::test]
    async fn linear_archive_issue_missing_id_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = LinearArchiveIssueTool { client };
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("issue_id"));
    }

    #[test]
    fn all_linear_tools_have_valid_json_schemas() {
        let client = Arc::new(LinearClient::new("lin_api_test".into(), Arc::new(NoopObserver)));
        let tools = all_linear_tools(client);
        assert_eq!(tools.len(), 14);
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
                                        "url": "https://github.com/zeroclaw_org/zeroclaw/pull/42",
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

    #[test]
    fn all_linear_tools_have_unique_names() {
        let client = Arc::new(LinearClient::new("lin_api_test".into(), Arc::new(NoopObserver)));
        let tools = all_linear_tools(client);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 14);
    }
}
