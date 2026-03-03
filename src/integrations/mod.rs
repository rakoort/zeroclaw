pub mod catalog;
pub(crate) mod catalog_registry;
pub mod linear;
pub mod slack;

// Re-export catalog types for callers (gateway API, main.rs CLI).
#[allow(unused_imports)]
pub use catalog::{handle_command, IntegrationCategory, IntegrationEntry, IntegrationStatus};

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
        integrations.push(Arc::new(slack::SlackIntegration::new(slack_config.clone())));
    }

    if let Some(ref linear_config) = config.integrations.linear {
        integrations.push(Arc::new(linear::LinearIntegration::new(
            linear_config.clone(),
        )));
    }

    integrations
}

/// Build a one-line-per-integration summary of active (configured) integrations
/// for use in the classifier prompt. Only includes integrations with runtime
/// tools (i.e., those returned by `collect_integrations`).
pub fn active_integration_summary(config: &Config) -> String {
    let integrations = collect_integrations(config);
    if integrations.is_empty() {
        return String::new();
    }
    let catalog = catalog_registry::all_integrations();
    let mut lines = Vec::new();
    for integration in &integrations {
        let name = integration.name();
        let description = catalog
            .iter()
            .find(|e| e.name.to_lowercase() == name.to_lowercase())
            .map(|e| e.description)
            .unwrap_or("External integration");
        lines.push(format!("- {name}: {description}"));
    }
    lines.join("\n")
}

/// Compute the tool names to exclude based on classifier integration selection.
/// Returns tool names from integrations NOT in `selected`.
/// If `selected` is empty, all integration tools are excluded.
pub fn excluded_tool_names(
    integration_tool_names: &std::collections::HashMap<String, Vec<String>>,
    selected: &[String],
) -> Vec<String> {
    let mut excluded = Vec::new();
    for (integration_name, tool_names) in integration_tool_names {
        if !selected
            .iter()
            .any(|s| s.eq_ignore_ascii_case(integration_name))
        {
            excluded.extend(tool_names.iter().cloned());
        }
    }
    excluded
}

/// Build a mapping of integration name to its tool names from configured integrations.
pub fn build_integration_tool_map(config: &Config) -> std::collections::HashMap<String, Vec<String>> {
    let integrations = collect_integrations(config);
    integrations
        .iter()
        .map(|i| {
            let name = i.name().to_string();
            let tool_names: Vec<String> = i.tools().iter().map(|t| t.spec().name.clone()).collect();
            (name, tool_names)
        })
        .collect()
}

/// Filter integration tools to only those from the selected integrations.
/// Returns tools from integrations whose `name()` appears in `selected`.
pub fn filter_tools_by_integrations(
    integrations: &[Arc<dyn Integration>],
    selected: &[String],
) -> Vec<Arc<dyn Tool>> {
    integrations
        .iter()
        .filter(|i| selected.iter().any(|s| s.eq_ignore_ascii_case(i.name())))
        .flat_map(|i| i.tools())
        .collect()
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

    #[test]
    fn active_integration_summary_returns_configured_integrations() {
        let mut config = crate::config::Config::default();
        config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });
        let summary = active_integration_summary(&config);
        assert!(
            summary.contains("linear"),
            "should contain integration name"
        );
    }

    #[test]
    fn active_integration_summary_empty_when_none_configured() {
        let config = crate::config::Config::default();
        let summary = active_integration_summary(&config);
        assert!(
            summary.is_empty(),
            "should be empty when no integrations configured"
        );
    }

    #[test]
    fn filter_tools_by_integrations_keeps_matching() {
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
        config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });

        let integrations = collect_integrations(&config);
        let selected = vec!["linear".to_string()];
        let filtered = filter_tools_by_integrations(&integrations, &selected);

        // Should have linear tools (14) but not slack tools (9)
        assert_eq!(filtered.len(), 14);
        for tool in &filtered {
            assert!(
                tool.spec().name.starts_with("linear_"),
                "Expected linear tool, got: {}",
                tool.spec().name
            );
        }
    }

    #[test]
    fn filter_tools_by_integrations_empty_selection_returns_empty() {
        let mut config = crate::config::Config::default();
        config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });
        let integrations = collect_integrations(&config);
        let filtered = filter_tools_by_integrations(&integrations, &[]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_tools_by_integrations_case_insensitive() {
        let mut config = crate::config::Config::default();
        config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });
        let integrations = collect_integrations(&config);
        let selected = vec!["Linear".to_string()];
        let filtered = filter_tools_by_integrations(&integrations, &selected);
        assert_eq!(filtered.len(), 14);
    }

    #[test]
    fn excluded_tool_names_excludes_unselected() {
        let mut map = std::collections::HashMap::new();
        map.insert("slack".to_string(), vec!["slack_post".into(), "slack_reply".into()]);
        map.insert("linear".to_string(), vec!["linear_create".into()]);

        let selected = vec!["linear".to_string()];
        let excluded = excluded_tool_names(&map, &selected);

        assert!(excluded.contains(&"slack_post".to_string()));
        assert!(excluded.contains(&"slack_reply".to_string()));
        assert!(!excluded.contains(&"linear_create".to_string()));
    }

    #[test]
    fn excluded_tool_names_case_insensitive() {
        let mut map = std::collections::HashMap::new();
        map.insert("slack".to_string(), vec!["slack_post".into()]);

        let selected = vec!["Slack".to_string()];
        let excluded = excluded_tool_names(&map, &selected);
        assert!(excluded.is_empty());
    }

    #[test]
    fn excluded_tool_names_empty_selection_excludes_all() {
        let mut map = std::collections::HashMap::new();
        map.insert("slack".to_string(), vec!["slack_post".into()]);
        map.insert("linear".to_string(), vec!["linear_create".into()]);

        let excluded = excluded_tool_names(&map, &[]);
        assert_eq!(excluded.len(), 2);
    }

    #[test]
    fn build_integration_tool_map_returns_empty_for_default_config() {
        let config = crate::config::Config::default();
        let map = build_integration_tool_map(&config);
        assert!(map.is_empty());
    }

    #[test]
    fn build_integration_tool_map_includes_configured_integrations() {
        let mut config = crate::config::Config::default();
        config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
            api_key: "lin_api_test".into(),
        });
        let map = build_integration_tool_map(&config);
        assert!(map.contains_key("linear"));
        assert_eq!(map["linear"].len(), 14);
    }
}
