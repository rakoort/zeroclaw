pub mod catalog;
pub(crate) mod catalog_registry;
pub mod linear;
pub mod slack;

// Re-export catalog types for callers (gateway API, main.rs CLI).
#[allow(unused_imports)]
pub use catalog::{
    handle_command, IntegrationCategory, IntegrationEntry, IntegrationStatus,
};

use async_trait::async_trait;
use std::sync::Arc;

use crate::channels::traits::Channel;
use crate::config::Config;
use crate::tools::traits::Tool;

/// A runtime integration that owns an authenticated API client and exposes
/// tools (and optionally a channel) to the agent.
#[async_trait]
pub trait Integration: Send + Sync {
    /// Short identifier for this integration (e.g. `"slack"`, `"linear"`).
    fn name(&self) -> &str;

    /// Tools provided by this integration for LLM function calling.
    fn tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Quick connectivity check. Returns `true` if the API is reachable.
    async fn health_check(&self) -> bool {
        true
    }

    /// If this integration also acts as a channel, return a channel reference.
    fn as_channel(&self) -> Option<Arc<dyn Channel>> {
        None
    }
}

/// Build integrations from config. Returns empty vec when no integrations are configured.
pub fn collect_integrations(config: &Config) -> Vec<Arc<dyn Integration>> {
    let mut integrations: Vec<Arc<dyn Integration>> = Vec::new();

    if let Some(ref slack_config) = config.integrations.slack {
        integrations.push(Arc::new(slack::SlackIntegration::new(
            slack_config.clone(),
        )));
    }

    if let Some(ref linear_config) = config.integrations.linear {
        integrations.push(Arc::new(linear::LinearIntegration::new(
            linear_config.clone(),
        )));
    }

    integrations
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyIntegration;

    #[async_trait]
    impl Integration for DummyIntegration {
        fn name(&self) -> &str {
            "dummy"
        }
        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }
    }

    #[tokio::test]
    async fn dummy_integration_default_methods() {
        let i = DummyIntegration;
        assert_eq!(i.name(), "dummy");
        assert!(i.tools().is_empty());
        assert!(i.health_check().await);
        assert!(i.as_channel().is_none());
    }

    #[test]
    fn collect_integrations_returns_empty_for_default_config() {
        let config = crate::config::Config::default();
        let integrations = collect_integrations(&config);
        assert!(integrations.is_empty());
    }

    #[test]
    fn collect_integrations_returns_slack_when_configured() {
        let mut config = crate::config::Config::default();
        config.integrations.slack = Some(crate::config::SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        });
        let integrations = collect_integrations(&config);
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0].name(), "slack");
        assert_eq!(integrations[0].tools().len(), 9);
        assert!(integrations[0].as_channel().is_some());
    }

    #[test]
    fn collect_integrations_returns_linear_when_configured() {
        let mut config = crate::config::Config::default();
        config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });
        let integrations = collect_integrations(&config);
        assert_eq!(integrations.len(), 1);
        assert_eq!(integrations[0].name(), "linear");
        assert_eq!(integrations[0].tools().len(), 14);
        assert!(integrations[0].as_channel().is_none());
    }
}
