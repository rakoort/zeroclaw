use std::sync::Arc;

use async_trait::async_trait;

use super::SlackToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct SlackReactTool {
    pub config: Arc<SlackToolConfig>,
}

#[async_trait]
impl Tool for SlackReactTool {
    fn name(&self) -> &str {
        "slack_react"
    }

    fn description(&self) -> &str {
        "Add an emoji reaction to a Slack message"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "Slack channel ID" },
                "timestamp":  { "type": "string", "description": "Message timestamp to react to" },
                "emoji":      { "type": "string", "description": "Emoji name without colons" },
                "ritual":     { "type": "string", "description": "Ritual context for the action" },
                "context":    { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["channel_id", "timestamp", "emoji", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let channel_id = args
            .get("channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: channel_id"))?;
        let timestamp = args
            .get("timestamp")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: timestamp"))?;
        let emoji = args
            .get("emoji")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: emoji"))?;
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
                "react",
                channel_id,
                timestamp,
                emoji,
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
    fn slack_react_tool_metadata_and_required_params() {
        let config = Arc::new(SlackToolConfig::new(
            "slack-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = SlackReactTool { config };

        assert_eq!(tool.name(), "slack_react");

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
            required_strs.contains(&"timestamp"),
            "missing required param: timestamp"
        );
        assert!(
            required_strs.contains(&"emoji"),
            "missing required param: emoji"
        );
    }
}
