use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearCreateIssueTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearCreateIssueTool {
    fn name(&self) -> &str {
        "linear_create_issue"
    }

    fn description(&self) -> &str {
        "Create a new issue in Linear"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title":       { "type": "string", "description": "Issue title" },
                "description": { "type": "string", "description": "Issue description" },
                "team_id":     { "type": "string", "description": "Linear team ID" },
                "ritual":      { "type": "string", "description": "Ritual context for the action" },
                "context":     { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["title", "description", "team_id", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: title"))?;
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: description"))?;
        let team_id = args
            .get("team_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: team_id"))?;
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
                "create-issue",
                title,
                description,
                "--team",
                team_id,
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
    use std::sync::Arc;

    use crate::tools::linear::LinearToolConfig;
    use crate::tools::traits::Tool;

    #[test]
    fn linear_create_issue_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = super::LinearCreateIssueTool { config };

        assert_eq!(tool.name(), "linear_create_issue");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(required_strs.contains(&"ritual"));
        assert!(required_strs.contains(&"context"));
        assert!(required_strs.contains(&"title"));
        assert!(required_strs.contains(&"description"));
        assert!(required_strs.contains(&"team_id"));
    }
}
