use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearProjectsTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearProjectsTool {
    fn name(&self) -> &str {
        "linear_projects"
    }

    fn description(&self) -> &str {
        "List projects for a Linear team"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "team_id": { "type": "string", "description": "Linear team ID" }
            },
            "required": ["team_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let team_id = args
            .get("team_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: team_id"))?;

        let output = self.config.run(&["projects", "--team", team_id]).await?;

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
    fn linear_projects_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearProjectsTool { config };

        assert_eq!(tool.name(), "linear_projects");

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
