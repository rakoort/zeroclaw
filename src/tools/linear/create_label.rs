use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearCreateLabelTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearCreateLabelTool {
    fn name(&self) -> &str {
        "linear_create_label"
    }

    fn description(&self) -> &str {
        "Create a label in Linear"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name":    { "type": "string", "description": "Label name" },
                "team_id": { "type": "string", "description": "Linear team ID" },
                "color":   { "type": "string", "description": "Label color (hex)" },
                "ritual":  { "type": "string", "description": "Ritual context for the action" },
                "context": { "type": "string", "description": "Additional context for the action" }
            },
            "required": ["name", "team_id", "ritual", "context"]
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
        let ritual = args
            .get("ritual")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: ritual"))?;
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required param: context"))?;

        let mut cli_args: Vec<&str> = vec!["create-label", name, "--team", team_id];

        if let Some(color) = args.get("color").and_then(|v| v.as_str()) {
            cli_args.extend(&["--color", color]);
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

    use super::*;

    #[test]
    fn linear_create_label_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearCreateLabelTool { config };

        assert_eq!(tool.name(), "linear_create_label");

        let schema = tool.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();

        assert!(required_strs.contains(&"ritual"));
        assert!(required_strs.contains(&"context"));
        assert!(required_strs.contains(&"name"));
        assert!(required_strs.contains(&"team_id"));
    }
}
