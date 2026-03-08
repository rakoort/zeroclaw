use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::client::{SlackApiError, SlackClient};
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

fn err_result(e: SlackApiError) -> ToolResult {
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

// ── SlackSendTool ───────────────────────────────────────────────────

pub struct SlackSendTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackSendTool {
    fn name(&self) -> &str {
        "slack_send"
    }

    fn description(&self) -> &str {
        "Send a message to a Slack channel"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "message": { "type": "string", "description": "Message text" }
            },
            "required": ["channel_id", "message"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let channel_id = match require_param(&args, "channel_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let message = match require_param(&args, "message") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self
            .client
            .api_post(
                "chat.postMessage",
                &json!({"channel": channel_id, "text": message}),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackDmTool ─────────────────────────────────────────────────────

pub struct SlackDmTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackDmTool {
    fn name(&self) -> &str {
        "slack_dm"
    }

    fn description(&self) -> &str {
        "Send a direct message to a Slack user"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "user_id": { "type": "string", "description": "Slack user ID" },
                "message": { "type": "string", "description": "Message text" }
            },
            "required": ["user_id", "message"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let user_id = match require_param(&args, "user_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let message = match require_param(&args, "message") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self
            .client
            .api_post(
                "chat.postMessage",
                &json!({"channel": user_id, "text": message}),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackSendThreadTool ─────────────────────────────────────────────

pub struct SlackSendThreadTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackSendThreadTool {
    fn name(&self) -> &str {
        "slack_send_thread"
    }

    fn description(&self) -> &str {
        "Reply to a Slack thread"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "thread_ts": { "type": "string", "description": "Thread timestamp" },
                "message": { "type": "string", "description": "Message text" }
            },
            "required": ["channel_id", "thread_ts", "message"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let channel_id = match require_param(&args, "channel_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let thread_ts = match require_param(&args, "thread_ts") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let message = match require_param(&args, "message") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self
            .client
            .api_post(
                "chat.postMessage",
                &json!({
                    "channel": channel_id,
                    "thread_ts": thread_ts,
                    "text": message
                }),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackSendFileTool ───────────────────────────────────────────────

pub struct SlackSendFileTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackSendFileTool {
    fn name(&self) -> &str {
        "slack_send_file"
    }

    fn description(&self) -> &str {
        "Upload a file to a Slack channel"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "file_path": { "type": "string", "description": "Path to the file to upload" }
            },
            "required": ["channel_id", "file_path"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let channel_id = match require_param(&args, "channel_id") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };
        let file_path = match require_param(&args, "file_path") {
            Ok(v) => v.to_string(),
            Err(e) => return Ok(e),
        };

        // Step 1: Get upload URL
        let file_bytes = match tokio::fs::read(&file_path).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("failed to read file: {e}")),
                });
            }
        };

        let filename = std::path::Path::new(&file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");

        let upload_url_resp = match self
            .client
            .api_get(
                "files.getUploadURLExternal",
                &[
                    ("filename", filename),
                    ("length", &file_bytes.len().to_string()),
                ],
            )
            .await
        {
            Ok(v) => v,
            Err(e) => return Ok(err_result(e)),
        };

        let upload_url = match upload_url_resp.get("upload_url").and_then(Value::as_str) {
            Some(u) => u.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing upload_url in response".into()),
                });
            }
        };

        let file_id = upload_url_resp
            .get("file_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // Step 2: Upload file to presigned URL
        let http = reqwest::Client::new();
        if let Err(e) = http.post(&upload_url).body(file_bytes).send().await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("file upload failed: {e}")),
            });
        }

        // Step 3: Complete the upload
        match self
            .client
            .api_post(
                "files.completeUploadExternal",
                &json!({
                    "files": [{"id": file_id}],
                    "channel_id": channel_id
                }),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackHistoryTool ────────────────────────────────────────────────

pub struct SlackHistoryTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackHistoryTool {
    fn name(&self) -> &str {
        "slack_history"
    }

    fn description(&self) -> &str {
        "Get recent messages from a Slack channel"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "limit": { "type": "integer", "description": "Number of messages (default 20)" }
            },
            "required": ["channel_id"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let channel_id = match require_param(&args, "channel_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .to_string();

        match self
            .client
            .api_get(
                "conversations.history",
                &[("channel", channel_id), ("limit", &limit)],
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackDmHistoryTool ──────────────────────────────────────────────

pub struct SlackDmHistoryTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackDmHistoryTool {
    fn name(&self) -> &str {
        "slack_dm_history"
    }

    fn description(&self) -> &str {
        "Get recent direct messages with a Slack user"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "user_id": { "type": "string", "description": "Slack user ID" },
                "limit": { "type": "integer", "description": "Number of messages (default 20)" }
            },
            "required": ["user_id"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let user_id = match require_param(&args, "user_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .to_string();

        // Open or find the DM channel first
        let open_resp = match self
            .client
            .api_post("conversations.open", &json!({"users": user_id}))
            .await
        {
            Ok(v) => v,
            Err(e) => return Ok(err_result(e)),
        };

        let channel_id = match open_resp
            .get("channel")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
        {
            Some(id) => id.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("could not resolve DM channel".into()),
                });
            }
        };

        match self
            .client
            .api_get(
                "conversations.history",
                &[("channel", &channel_id), ("limit", &limit)],
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackThreadsTool ────────────────────────────────────────────────

pub struct SlackThreadsTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackThreadsTool {
    fn name(&self) -> &str {
        "slack_threads"
    }

    fn description(&self) -> &str {
        "Get replies in a Slack thread"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "thread_ts": { "type": "string", "description": "Thread timestamp" },
                "limit": { "type": "integer", "description": "Number of replies (default 50)" }
            },
            "required": ["channel_id", "thread_ts"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let channel_id = match require_param(&args, "channel_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let thread_ts = match require_param(&args, "thread_ts") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .to_string();

        match self
            .client
            .api_get(
                "conversations.replies",
                &[
                    ("channel", channel_id),
                    ("ts", thread_ts),
                    ("limit", &limit),
                ],
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackPresenceTool ───────────────────────────────────────────────

pub struct SlackPresenceTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackPresenceTool {
    fn name(&self) -> &str {
        "slack_presence"
    }

    fn description(&self) -> &str {
        "Check a Slack user's presence status"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "user_id": { "type": "string", "description": "Slack user ID" }
            },
            "required": ["user_id"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let user_id = match require_param(&args, "user_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self
            .client
            .api_get("users.getPresence", &[("user", user_id)])
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── SlackReactTool ──────────────────────────────────────────────────

pub struct SlackReactTool {
    pub(crate) client: Arc<SlackClient>,
}

#[async_trait]
impl Tool for SlackReactTool {
    fn name(&self) -> &str {
        "slack_react"
    }

    fn description(&self) -> &str {
        "Add an emoji reaction to a Slack message"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "timestamp": { "type": "string", "description": "Message timestamp" },
                "emoji": { "type": "string", "description": "Emoji name without colons" }
            },
            "required": ["channel_id", "timestamp", "emoji"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let channel_id = match require_param(&args, "channel_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let timestamp = match require_param(&args, "timestamp") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let emoji = match require_param(&args, "emoji") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        match self
            .client
            .api_post(
                "reactions.add",
                &json!({
                    "channel": channel_id,
                    "timestamp": timestamp,
                    "name": emoji
                }),
            )
            .await
        {
            Ok(v) => Ok(ok_result(&v)),
            Err(e) => Ok(err_result(e)),
        }
    }
}

// ── Factory ─────────────────────────────────────────────────────────

/// Return all 9 Slack tools backed by the given client.
pub fn all_slack_tools(client: Arc<SlackClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(SlackDmTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackSendTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackSendThreadTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackSendFileTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackHistoryTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackDmHistoryTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackThreadsTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackPresenceTool {
            client: Arc::clone(&client),
        }),
        Arc::new(SlackReactTool { client }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::noop::NoopObserver;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_client(server: &MockServer) -> Arc<SlackClient> {
        Arc::new(SlackClient::new_with_base_url(
            "xoxb-test".into(),
            String::new(),
            server.uri(),
            Arc::new(NoopObserver),
        ))
    }

    #[tokio::test]
    async fn slack_send_missing_channel_id_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = SlackSendTool { client };
        let result = tool.execute(json!({"message": "hi"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("channel_id"));
    }

    #[tokio::test]
    async fn slack_send_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "ts": "1234"})),
            )
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = SlackSendTool { client };
        let result = tool
            .execute(json!({"channel_id": "C123", "message": "hello"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn slack_history_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations.history"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "messages": []})),
            )
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = SlackHistoryTool { client };
        let result = tool.execute(json!({"channel_id": "C123"})).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn slack_react_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/reactions.add"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = SlackReactTool { client };
        let result = tool
            .execute(json!({
                "channel_id": "C123",
                "timestamp": "1234.5678",
                "emoji": "thumbsup"
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn slack_presence_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/users.getPresence"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "presence": "active"})),
            )
            .mount(&server)
            .await;

        let client = mock_client(&server);
        let tool = SlackPresenceTool { client };
        let result = tool.execute(json!({"user_id": "U123"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("active"));
    }

    #[tokio::test]
    async fn slack_dm_missing_user_id_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = SlackDmTool { client };
        let result = tool.execute(json!({"message": "hello"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("user_id"));
    }

    #[tokio::test]
    async fn slack_send_thread_missing_thread_ts_returns_error() {
        let server = MockServer::start().await;
        let client = mock_client(&server);
        let tool = SlackSendThreadTool { client };
        let result = tool
            .execute(json!({"channel_id": "C123", "message": "reply"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("thread_ts"));
    }

    #[test]
    fn all_slack_tools_have_valid_json_schemas() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into(), Arc::new(NoopObserver)));
        let tools = all_slack_tools(client);
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"],
                "object",
                "Tool {} schema must be object",
                tool.name()
            );
            assert!(
                schema.get("properties").is_some(),
                "Tool {} must have properties",
                tool.name()
            );
        }
    }

    #[test]
    fn all_slack_tools_have_unique_names() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into(), Arc::new(NoopObserver)));
        let tools = all_slack_tools(client);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 9);
    }
}
