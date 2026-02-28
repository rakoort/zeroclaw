use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearTeamsTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearTeamsTool {
    fn name(&self) -> &str {
        "linear_teams"
    }

    fn description(&self) -> &str {
        "List all teams in the Linear workspace"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let output = self.config.run(&["teams"]).await?;

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
    fn linear_teams_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearTeamsTool { config };

        assert_eq!(tool.name(), "linear_teams");

        let schema = tool.parameters_schema();
        assert!(schema["properties"].is_object());
        assert!(
            schema.get("required").is_none(),
            "teams tool should have no required params"
        );
    }
}
