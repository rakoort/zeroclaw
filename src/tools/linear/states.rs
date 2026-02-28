use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearStatesTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearStatesTool {
    fn name(&self) -> &str {
        "linear_states"
    }

    fn description(&self) -> &str {
        "List workflow states for a Linear team"
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

        let output = self.config.run(&["states", "--team", team_id]).await?;

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
    fn linear_states_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearStatesTool { config };

        assert_eq!(tool.name(), "linear_states");

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
