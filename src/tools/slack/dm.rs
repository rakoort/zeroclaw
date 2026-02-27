use std::sync::Arc;

use async_trait::async_trait;

use super::SlackToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct SlackDmTool {
    pub config: Arc<SlackToolConfig>,
}

#[async_trait]
impl Tool for SlackDmTool {
    fn name(&self) -> &str {
        "slack_dm"
    }

    fn description(&self) -> &str {
        "Send a direct message to a Slack user"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "user_id":  { "type": "string", "description": "Slack user ID to message" },
                "message":  { "type": "string", "description": "Message text to send" },
                "ritual":   { "type": "string", "description": "Ritual context for the action" },
                "context":  { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["user_id", "message", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let user_id = args
            .get("user_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: user_id"))?;
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
                "dm",
                user_id,
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
    fn slack_dm_tool_metadata_and_required_params() {
        let config = Arc::new(SlackToolConfig::new(
            "slack-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = SlackDmTool { config };

        assert_eq!(tool.name(), "slack_dm");

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
            required_strs.contains(&"user_id"),
            "missing required param: user_id"
        );
        assert!(
            required_strs.contains(&"message"),
            "missing required param: message"
        );
    }
}
