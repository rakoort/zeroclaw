use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearUpdateIssueTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearUpdateIssueTool {
    fn name(&self) -> &str {
        "linear_update_issue"
    }

    fn description(&self) -> &str {
        "Update an existing issue in Linear"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "issue_id":    { "type": "string", "description": "Linear issue ID to update" },
                "title":       { "type": "string", "description": "New issue title" },
                "description": { "type": "string", "description": "New issue description" },
                "state_id":    { "type": "string", "description": "New workflow state ID" },
                "assignee_id": { "type": "string", "description": "New assignee user ID" },
                "priority":    { "type": "integer", "description": "Priority level (0-4)" },
                "ritual":      { "type": "string", "description": "Ritual context for the action" },
                "context":     { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["issue_id", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let issue_id = args
            .get("issue_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: issue_id"))?;
        let ritual = args
            .get("ritual")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: ritual"))?;
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: context"))?;

        let mut cli_args: Vec<&str> = vec!["update-issue", issue_id];

        if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
            cli_args.extend(&["--title", title]);
        }
        if let Some(description) = args.get("description").and_then(|v| v.as_str()) {
            cli_args.extend(&["--description", description]);
        }
        if let Some(state_id) = args.get("state_id").and_then(|v| v.as_str()) {
            cli_args.extend(&["--state", state_id]);
        }
        if let Some(assignee_id) = args.get("assignee_id").and_then(|v| v.as_str()) {
            cli_args.extend(&["--assignee", assignee_id]);
        }

        let priority_str;
        if let Some(priority) = args.get("priority").and_then(|v| v.as_i64()) {
            priority_str = priority.to_string();
            cli_args.extend(&["--priority", &priority_str]);
        }

        cli_args.extend(&["--ritual", ritual, "--context", context]);

        let output = self.config.run(&cli_args).await?;

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
    fn linear_update_issue_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = super::LinearUpdateIssueTool { config };

        assert_eq!(tool.name(), "linear_update_issue");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(required_strs.contains(&"ritual"));
        assert!(required_strs.contains(&"context"));
        assert!(required_strs.contains(&"issue_id"));
    }
}
