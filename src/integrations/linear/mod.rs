pub mod client;
pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::config::LinearIntegrationConfig;
use crate::integrations::Integration;
use crate::observability::traits::Observer;
use crate::tools::traits::Tool;

use self::client::LinearClient;
use self::tools::all_linear_tools;

/// Native Linear integration — provides 14 tools, no channel.
pub struct LinearIntegration {
    client: Arc<LinearClient>,
}

impl LinearIntegration {
    pub fn new(config: LinearIntegrationConfig, observer: Arc<dyn Observer>) -> Self {
        Self {
            client: Arc::new(LinearClient::new(config.api_key, observer)),
        }
    }
}

#[async_trait]
impl Integration for LinearIntegration {
    fn name(&self) -> &str {
        "linear"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        all_linear_tools(Arc::clone(&self.client))
    }

    async fn health_check(&self) -> bool {
        self.client
            .graphql("query { viewer { id } }", &json!({}))
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::client::LinearClient;
    use super::tools::all_linear_tools;
    use super::LinearIntegration;
    use crate::config::LinearIntegrationConfig;
    use crate::integrations::Integration;
    use crate::observability::noop::NoopObserver;
    use std::sync::Arc;

    #[test]
    fn all_linear_tools_returns_14_tools() {
        let client = Arc::new(LinearClient::new("lin_api_test".into(), Arc::new(NoopObserver)));
        let tools = all_linear_tools(client);
        assert_eq!(tools.len(), 14);
    }

    #[test]
    fn all_linear_tools_have_valid_json_schemas() {
        let client = Arc::new(LinearClient::new("lin_api_test".into(), Arc::new(NoopObserver)));
        let tools = all_linear_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"],
                "object",
                "Tool {} schema must be object",
                tool.name()
            );
        }
    }

    #[test]
    fn linear_integration_name() {
        let config = LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        };
        let integration = LinearIntegration::new(config, Arc::new(NoopObserver));
        assert_eq!(integration.name(), "linear");
    }

    #[test]
    fn linear_integration_returns_14_tools() {
        let config = LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        };
        let integration = LinearIntegration::new(config, Arc::new(NoopObserver));
        assert_eq!(integration.tools().len(), 14);
    }

    #[test]
    fn linear_integration_as_channel_returns_none() {
        let config = LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        };
        let integration = LinearIntegration::new(config, Arc::new(NoopObserver));
        assert!(integration.as_channel().is_none());
    }
}
