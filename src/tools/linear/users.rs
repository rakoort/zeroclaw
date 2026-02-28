use std::sync::Arc;

use async_trait::async_trait;

use super::LinearToolConfig;
use crate::tools::traits::{Tool, ToolResult};

pub struct LinearUsersTool {
    pub config: Arc<LinearToolConfig>,
}

#[async_trait]
impl Tool for LinearUsersTool {
    fn name(&self) -> &str {
        "linear_users"
    }

    fn description(&self) -> &str {
        "List all users in the Linear workspace"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let output = self.config.run(&["users"]).await?;

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
    fn linear_users_tool_metadata_and_required_params() {
        let config = Arc::new(LinearToolConfig::new(
            "linear-cli.ts",
            Path::new("/workspace"),
        ));
        let tool = LinearUsersTool { config };

        assert_eq!(tool.name(), "linear_users");

        let schema = tool.parameters_schema();
        assert!(schema["properties"].is_object());
        assert!(
            schema.get("required").is_none(),
            "users tool should have no required params"
        );
    }
}
