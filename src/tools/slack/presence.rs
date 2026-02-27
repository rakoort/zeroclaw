use std::sync::Arc;

use async_trait::async_trait;

use super::SlackToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct SlackPresenceTool {
    pub config: Arc<SlackToolConfig>,
}

#[async_trait]
impl Tool for SlackPresenceTool {
    fn name(&self) -> &str {
        "slack_presence"
    }

    fn description(&self) -> &str {
        "Check a Slack user's presence status"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "user_id": { "type": "string", "description": "Slack user ID" }
            },
            "required": ["user_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let user_id = args
            .get("user_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: user_id"))?;

        let output = self.config.run(&["presence", user_id]).await?;

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
    fn slack_presence_tool_metadata_and_required_params() {
        let config = Arc::new(SlackToolConfig::new(
            "slack-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = SlackPresenceTool { config };

        assert_eq!(tool.name(), "slack_presence");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(
            required_strs.contains(&"user_id"),
            "missing required param: user_id"
        );
    }
}
