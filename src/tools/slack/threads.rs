use std::sync::Arc;

use async_trait::async_trait;

use super::SlackToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct SlackThreadsTool {
    pub config: Arc<SlackToolConfig>,
}

#[async_trait]
impl Tool for SlackThreadsTool {
    fn name(&self) -> &str {
        "slack_threads"
    }

    fn description(&self) -> &str {
        "Fetch replies in a Slack thread"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "thread_ts":  { "type": "string", "description": "Thread timestamp" }
            },
            "required": ["channel_id", "thread_ts"]
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

        let output = self.config.run(&["threads", channel_id, thread_ts]).await?;

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
    fn slack_threads_tool_metadata_and_required_params() {
        let config = Arc::new(SlackToolConfig::new(
            "slack-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = SlackThreadsTool { config };

        assert_eq!(tool.name(), "slack_threads");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(
            required_strs.contains(&"channel_id"),
            "missing required param: channel_id"
        );
        assert!(
            required_strs.contains(&"thread_ts"),
            "missing required param: thread_ts"
        );
    }
}
