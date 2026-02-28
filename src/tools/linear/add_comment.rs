use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearAddCommentTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearAddCommentTool {
    fn name(&self) -> &str {
        "linear_add_comment"
    }

    fn description(&self) -> &str {
        "Add a comment to a Linear issue"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "issue_id": { "type": "string", "description": "Linear issue ID" },
                "body":     { "type": "string", "description": "Comment body text" },
                "ritual":   { "type": "string", "description": "Ritual context for the action" },
                "context":  { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["issue_id", "body", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let issue_id = args
            .get("issue_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: issue_id"))?;
        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: body"))?;
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
                "add-comment",
                issue_id,
                body,
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
    fn linear_add_comment_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearAddCommentTool { config };

        assert_eq!(tool.name(), "linear_add_comment");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(required_strs.contains(&"ritual"));
        assert!(required_strs.contains(&"context"));
        assert!(required_strs.contains(&"issue_id"));
        assert!(required_strs.contains(&"body"));
    }
}
