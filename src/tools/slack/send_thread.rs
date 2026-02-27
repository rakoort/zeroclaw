use std::sync::Arc;

use async_trait::async_trait;

use super::SlackToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct SlackSendThreadTool {
    pub config: Arc<SlackToolConfig>,
}

#[async_trait]
impl Tool for SlackSendThreadTool {
    fn name(&self) -> &str {
        "slack_send_thread"
    }

    fn description(&self) -> &str {
        "Reply to a Slack thread"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "thread_ts":  { "type": "string", "description": "Thread timestamp to reply to" },
                "message":    { "type": "string", "description": "Reply message text" },
                "ritual":     { "type": "string", "description": "Ritual context for the action" },
                "context":    { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["channel_id", "thread_ts", "message", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let channel_id = args
            .get("channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: channel_id"))?;
        let thread_ts = args
            .get("thread_ts")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: thread_ts"))?;
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: message"))?;
        let ritual = args
            .get("ritual")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: ritual"))?;
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: context"))?;

        let output = self
            .config
            .run(&[
                "send-thread",
                channel_id,
                thread_ts,
                message,
                "--ritual",
                ritual,
                "--context",
                context,
            ])
            .await?;

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn slack_send_thread_tool_metadata_and_required_params() {
        let config = Arc::new(SlackToolConfig::new(
            "slack-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = SlackSendThreadTool { config };

        assert_eq!(tool.name(), "slack_send_thread");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(
            required_strs.contains(&"ritual"),
            "missing required param: ritual"
        );
        assert!(
            required_strs.contains(&"context"),
            "missing required param: context"
        );
        assert!(
            required_strs.contains(&"channel_id"),
            "missing required param: channel_id"
        );
        assert!(
            required_strs.contains(&"thread_ts"),
            "missing required param: thread_ts"
        );
        assert!(
            required_strs.contains(&"message"),
            "missing required param: message"
        );
    }
}
