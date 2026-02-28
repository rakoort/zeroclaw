use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearCreateCycleTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearCreateCycleTool {
    fn name(&self) -> &str {
        "linear_create_cycle"
    }

    fn description(&self) -> &str {
        "Create a cycle in Linear"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name":      { "type": "string", "description": "Cycle name" },
                "team_id":   { "type": "string", "description": "Linear team ID" },
                "starts_at": { "type": "string", "description": "Cycle start date (ISO 8601)" },
                "ends_at":   { "type": "string", "description": "Cycle end date (ISO 8601)" },
                "ritual":    { "type": "string", "description": "Ritual context for the action" },
                "context":   { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["name", "team_id", "starts_at", "ends_at", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: name"))?;
        let team_id = args
            .get("team_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: team_id"))?;
        let starts_at = args
            .get("starts_at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: starts_at"))?;
        let ends_at = args
            .get("ends_at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: ends_at"))?;
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
                "create-cycle",
                name,
                "--team",
                team_id,
                "--starts-at",
                starts_at,
                "--ends-at",
                ends_at,
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
    fn linear_create_cycle_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearCreateCycleTool { config };

        assert_eq!(tool.name(), "linear_create_cycle");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(required_strs.contains(&"ritual"));
        assert!(required_strs.contains(&"context"));
        assert!(required_strs.contains(&"name"));
        assert!(required_strs.contains(&"team_id"));
        assert!(required_strs.contains(&"starts_at"));
        assert!(required_strs.contains(&"ends_at"));
    }
}
