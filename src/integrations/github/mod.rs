pub mod client;
pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::config::GitHubIntegrationConfig;
use crate::integrations::Integration;
use crate::tools::traits::Tool;

use self::client::GitHubClient;
use self::tools::all_github_tools;

/// Native GitHub integration — provides 3 read-only tools, no channel.
pub struct GitHubIntegration {
    client: Arc<GitHubClient>,
}

impl GitHubIntegration {
    pub fn new(config: GitHubIntegrationConfig) -> Self {
        Self {
            client: Arc::new(GitHubClient::new(config.token, config.owner)),
        }
    }
}

#[async_trait]
impl Integration for GitHubIntegration {
    fn name(&self) -> &str {
        "github"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        all_github_tools(Arc::clone(&self.client))
    }

    async fn health_check(&self) -> bool {
        self.client
            .graphql("query { viewer { login } }", &json!({}))
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::client::GitHubClient;
    use super::tools::all_github_tools;
    use std::sync::Arc;

    #[test]
    fn all_github_tools_returns_3_tools() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn all_github_tools_have_valid_json_schemas() {
        let client = Arc::new(GitHubClient::new("ghp_test".into(), None));
        let tools = all_github_tools(client);
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
    fn github_integration_name() {
        use super::GitHubIntegration;
        use crate::config::GitHubIntegrationConfig;
        use crate::integrations::Integration;

        let config = GitHubIntegrationConfig {
            token: "ghp_test".into(),
            owner: None,
        };
        let integration = GitHubIntegration::new(config);
        assert_eq!(integration.name(), "github");
    }

    #[test]
    fn github_integration_returns_3_tools() {
        use super::GitHubIntegration;
        use crate::config::GitHubIntegrationConfig;
        use crate::integrations::Integration;

        let config = GitHubIntegrationConfig {
            token: "ghp_test".into(),
            owner: None,
        };
        let integration = GitHubIntegration::new(config);
        assert_eq!(integration.tools().len(), 3);
    }

    #[test]
    fn github_integration_as_channel_returns_none() {
        use super::GitHubIntegration;
        use crate::config::GitHubIntegrationConfig;
        use crate::integrations::Integration;

        let config = GitHubIntegrationConfig {
            token: "ghp_test".into(),
            owner: None,
        };
        let integration = GitHubIntegration::new(config);
        assert!(integration.as_channel().is_none());
    }
}
