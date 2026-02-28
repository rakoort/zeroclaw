use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearIssuesTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearIssuesTool {
    fn name(&self) -> &str {
        "linear_issues"
    }

    fn description(&self) -> &str {
        "List issues for a Linear team"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" },
                "limit":   { "type": "integer", "description": "Maximum number of issues to return" },
                "state":   { "type": "string", "description": "Filter by workflow state name" }
            },
            "required": ["team_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let team_id = args
            .get("team_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: team_id"))?;

        let mut cli_args: Vec<&str> = vec!["issues", "--team", team_id];

        let limit_str;
        if let Some(limit) = args.get("limit").and_then(|v| v.as_i64()) {
            limit_str = limit.to_string();
            cli_args.extend(&["--limit", &limit_str]);
        }

        if let Some(state) = args.get("state").and_then(|v| v.as_str()) {
            cli_args.extend(&["--state", state]);
        }

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

    use super::*;

    #[test]
    fn linear_issues_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearIssuesTool { config };

        assert_eq!(tool.name(), "linear_issues");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(required_strs.contains(&"team_id"));
        assert!(
            !required_strs.contains(&"ritual"),
            "read tool must not require ritual"
        );
    }
}
