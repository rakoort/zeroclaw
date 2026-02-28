// Watch tools — Tool trait implementations for watch management.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::{NewWatch, WatchManager};
use crate::tools::traits::{Tool, ToolResult};

// ---------------------------------------------------------------------------
// WatchTool — register a new event watch
// ---------------------------------------------------------------------------

pub struct WatchTool {
    manager: Arc<WatchManager>,
}

impl WatchTool {
    pub fn new(manager: Arc<WatchManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for WatchTool {
    fn name(&self) -> &str {
        "watch"
    }

    fn description(&self) -> &str {
        "Register a new event watch to monitor for specific messages"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "event_type": {
                    "type": "string",
                    "description": "Type of event to watch for (e.g. dm_reply, channel_message)"
                },
                "context": {
                    "type": "string",
                    "description": "Context describing what the agent is watching for and why"
                },
                "channel_name": {
                    "type": "string",
                    "description": "Channel name to monitor (e.g. slack, discord)"
                },
                "match_user_id": {
                    "type": "string",
                    "description": "Optional user ID filter — only match messages from this user"
                },
                "match_channel_id": {
                    "type": "string",
                    "description": "Optional channel ID filter — only match messages in this channel"
                },
                "match_thread_ts": {
                    "type": "string",
                    "description": "Optional thread timestamp filter — only match messages in this thread"
                },
                "reminder_after_minutes": {
                    "type": "integer",
                    "description": "Minutes after which a reminder is sent if no match yet"
                },
                "reminder_message": {
                    "type": "string",
                    "description": "Message to include in the reminder notification"
                },
                "expires_minutes": {
                    "type": "integer",
                    "description": "Minutes after which the watch expires automatically"
                },
                "on_expire": {
                    "type": "string",
                    "description": "Message to include in the expiry notification"
                }
            },
            "required": ["event_type", "context", "channel_name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let event_type = args
            .get("event_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: event_type"))?
            .to_string();
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: context"))?
            .to_string();
        let channel_name = args
            .get("channel_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: channel_name"))?
            .to_string();

        let match_user_id = args
            .get("match_user_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let match_channel_id = args
            .get("match_channel_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let match_thread_ts = args
            .get("match_thread_ts")
            .and_then(|v| v.as_str())
            .map(String::from);
        let reminder_after_minutes = args.get("reminder_after_minutes").and_then(|v| v.as_i64());
        let reminder_message = args
            .get("reminder_message")
            .and_then(|v| v.as_str())
            .map(String::from);
        let expires_minutes = args.get("expires_minutes").and_then(|v| v.as_i64());
        let on_expire = args
            .get("on_expire")
            .and_then(|v| v.as_str())
            .map(String::from);

        let watch = NewWatch {
            event_type,
            match_user_id,
            match_channel_id,
            match_thread_ts,
            context,
            reminder_after_minutes,
            reminder_message,
            expires_minutes,
            on_expire,
            channel_name,
        };

        let id = self.manager.register(watch).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Watch registered with ID: {id}"),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// WatchListTool — list all active watches
// ---------------------------------------------------------------------------

pub struct WatchListTool {
    manager: Arc<WatchManager>,
}

impl WatchListTool {
    pub fn new(manager: Arc<WatchManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for WatchListTool {
    fn name(&self) -> &str {
        "watch_list"
    }

    fn description(&self) -> &str {
        "List all active watches"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let watches = self.manager.active_watches().await;

        let watch_list: Vec<serde_json::Value> = watches
            .into_iter()
            .map(|w| {
                json!({
                    "id": w.id,
                    "event_type": w.event_type,
                    "context": w.context,
                    "channel_name": w.channel_name,
                    "match_user_id": w.match_user_id,
                    "match_channel_id": w.match_channel_id,
                    "match_thread_ts": w.match_thread_ts,
                    "reminder_after_minutes": w.reminder_after_minutes,
                    "reminder_message": w.reminder_message,
                    "expires_minutes": w.expires_minutes,
                    "on_expire": w.on_expire,
                    "created_at": w.created_at,
                    "status": w.status,
                })
            })
            .collect();

        let output = serde_json::to_string_pretty(&watch_list).unwrap_or_else(|_| "[]".to_string());

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// WatchCancelTool — cancel an active watch by ID
// ---------------------------------------------------------------------------

pub struct WatchCancelTool {
    manager: Arc<WatchManager>,
}

impl WatchCancelTool {
    pub fn new(manager: Arc<WatchManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for WatchCancelTool {
    fn name(&self) -> &str {
        "watch_cancel"
    }

    fn description(&self) -> &str {
        "Cancel an active watch by ID"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The watch ID to cancel"
                }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: id"))?;

        self.manager.cancel(id).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Watch {id} cancelled"),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::tools::traits::Tool;
    use crate::watches::{NewWatch, WatchManager, WatchStore};
    use rusqlite::Connection;

    fn test_manager() -> Arc<WatchManager> {
        let conn = Connection::open_in_memory().unwrap();
        WatchStore::init_schema(&conn).unwrap();
        let store = Arc::new(tokio::sync::Mutex::new(WatchStore { conn }));
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        Arc::new(WatchManager::new(store, tx))
    }

    #[test]
    fn watch_tool_metadata_and_required_params() {
        let mgr = test_manager();
        let tool = super::WatchTool::new(mgr);
        assert_eq!(tool.name(), "watch");
        assert!(!tool.description().is_empty());

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("event_type")));
        assert!(required.contains(&serde_json::json!("context")));
        assert!(required.contains(&serde_json::json!("channel_name")));
    }

    #[test]
    fn watch_list_tool_metadata() {
        let mgr = test_manager();
        let tool = super::WatchListTool::new(mgr);
        assert_eq!(tool.name(), "watch_list");
        assert!(!tool.description().is_empty());

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn watch_cancel_tool_metadata_and_required_params() {
        let mgr = test_manager();
        let tool = super::WatchCancelTool::new(mgr);
        assert_eq!(tool.name(), "watch_cancel");
        assert!(!tool.description().is_empty());

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("id")));
    }

    #[tokio::test]
    async fn watch_tool_execute_registers_watch() {
        let mgr = test_manager();
        let tool = super::WatchTool::new(Arc::clone(&mgr));

        let result = tool
            .execute(serde_json::json!({
                "event_type": "dm_reply",
                "context": "Waiting for reply from user",
                "channel_name": "slack",
                "match_user_id": "U_TEST_001"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.starts_with("Watch registered with ID:"));

        let active = mgr.active_watches().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].event_type, "dm_reply");
    }

    #[tokio::test]
    async fn watch_tool_execute_missing_required_param() {
        let mgr = test_manager();
        let tool = super::WatchTool::new(mgr);

        let result = tool
            .execute(serde_json::json!({
                "context": "test",
                "channel_name": "slack"
            }))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("event_type"));
    }

    #[tokio::test]
    async fn watch_list_tool_execute_returns_active_watches() {
        let mgr = test_manager();

        // Register a watch via the manager directly
        mgr.register(NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: None,
            match_channel_id: None,
            match_thread_ts: None,
            context: "test context".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        })
        .await
        .unwrap();

        let tool = super::WatchListTool::new(Arc::clone(&mgr));
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(result.success);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["event_type"], "dm_reply");
    }

    #[tokio::test]
    async fn watch_cancel_tool_execute_cancels_watch() {
        let mgr = test_manager();

        let id = mgr
            .register(NewWatch {
                event_type: "dm_reply".into(),
                match_user_id: None,
                match_channel_id: None,
                match_thread_ts: None,
                context: "test context".into(),
                reminder_after_minutes: None,
                reminder_message: None,
                expires_minutes: None,
                on_expire: None,
                channel_name: "slack".into(),
            })
            .await
            .unwrap();

        let tool = super::WatchCancelTool::new(Arc::clone(&mgr));
        let result = tool.execute(serde_json::json!({ "id": id })).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("cancelled"));

        let active = mgr.active_watches().await;
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn watch_cancel_tool_execute_missing_id() {
        let mgr = test_manager();
        let tool = super::WatchCancelTool::new(mgr);

        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("id"));
    }
}
