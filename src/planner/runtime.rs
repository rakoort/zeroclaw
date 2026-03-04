use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::Config;
use crate::memory::Memory;
use crate::observability::{self, Observer};
use crate::providers::{self, Provider};
use crate::runtime::{self, RuntimeAdapter};
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool, ToolSpec};

/// Core runtime for the planner module. Owns the provider, tools,
/// observer, and model configuration needed for plan_then_execute().
pub struct PlannerRuntime {
    pub provider: Box<dyn Provider>,
    pub tools: Vec<Box<dyn Tool>>,
    pub tool_specs: Vec<ToolSpec>,
    pub observer: Arc<dyn Observer>,
    pub memory: Arc<dyn Memory>,
    pub planner_model: Option<String>,
    pub executor_model: String,
    pub model_routes: HashMap<String, String>,
    pub temperature: f64,
    pub max_tool_iterations: usize,
    pub max_executor_iterations: usize,
}

impl PlannerRuntime {
    /// Build a PlannerRuntime from config. Constructs provider, tools,
    /// observer, memory — everything needed for plan execution.
    pub fn from_config(config: &Config) -> Result<Self> {
        let observer: Arc<dyn Observer> =
            Arc::from(observability::create_observer(&config.observability));
        let rt: Arc<dyn RuntimeAdapter> = Arc::from(runtime::create_runtime(&config.runtime)?);
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));

        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory_with_storage_and_routes(
                &config.memory,
                &config.embedding_routes,
                Some(&config.storage.provider.config),
                &config.workspace_dir,
                config.api_key.as_deref(),
            )?);

        let composio_key = if config.composio.enabled {
            config.composio.api_key.as_deref()
        } else {
            None
        };
        let composio_entity_id = if config.composio.enabled {
            Some(config.composio.entity_id.as_str())
        } else {
            None
        };

        let tools = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            rt,
            memory.clone(),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.workspace_dir,
            &config.agents,
            config.api_key.as_deref(),
            config,
        );

        let tool_specs = tools.iter().map(|t| t.spec()).collect();

        let provider_name = config.default_provider.as_deref().unwrap_or("openrouter");
        let model_name = config
            .default_model
            .as_deref()
            .unwrap_or("anthropic/claude-sonnet-4-20250514")
            .to_string();

        let provider: Box<dyn Provider> = providers::create_routed_provider(
            provider_name,
            config.api_key.as_deref(),
            config.api_url.as_deref(),
            &config.reliability,
            &config.model_routes,
            &model_name,
        )?;

        let model_routes: HashMap<String, String> = config
            .model_routes
            .iter()
            .map(|route| (route.hint.clone(), route.model.clone()))
            .collect();

        let planner_model = model_routes.get("planner").cloned();

        Ok(Self {
            provider,
            tools,
            tool_specs,
            observer,
            memory,
            planner_model,
            executor_model: model_name,
            model_routes,
            temperature: config.default_temperature,
            max_tool_iterations: config.agent.max_tool_iterations,
            max_executor_iterations: config.agent.max_executor_action_iterations,
        })
    }
}

#[cfg(test)]
mod tests {}
