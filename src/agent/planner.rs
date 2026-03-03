use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write;
use tokio_util::sync::CancellationToken;

use crate::observability::{Observer, ObserverEvent};
use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::tools::{Tool, ToolSpec};

#[derive(Debug, Clone, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub passthrough: bool,
    #[serde(default)]
    pub actions: Vec<PlanAction>,
}

impl Plan {
    pub fn is_passthrough(&self) -> bool {
        self.passthrough || self.actions.is_empty()
    }

    /// Group actions by group number, sorted ascending.
    pub fn grouped_actions(&self) -> Vec<Vec<&PlanAction>> {
        let mut groups: BTreeMap<u32, Vec<&PlanAction>> = BTreeMap::new();
        for action in &self.actions {
            groups.entry(action.group).or_default().push(action);
        }
        groups.into_values().collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanAction {
    #[serde(default = "default_group")]
    pub group: u32,
    #[serde(rename = "type")]
    pub action_type: String,
    pub description: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub params: serde_json::Value,
}

fn default_group() -> u32 {
    1
}

/// Parse a JSON string into a Plan.
pub fn parse_plan(json: &str) -> Result<Plan> {
    serde_json::from_str(json).map_err(|e| anyhow::anyhow!("Plan parse error: {e}"))
}

/// Extract JSON from an LLM response that may contain markdown fences.
pub fn parse_plan_from_response(response: &str) -> Result<Plan> {
    // Try direct parse first
    if let Ok(plan) = parse_plan(response.trim()) {
        return Ok(plan);
    }
    // Try extracting from ```json ... ``` fences
    if let Some(start) = response.find("```json") {
        let json_start = start + 7;
        if let Some(end) = response[json_start..].find("```") {
            return parse_plan(response[json_start..json_start + end].trim());
        }
    }
    // Try extracting from ``` ... ``` fences (without json tag)
    if let Some(start) = response.find("```") {
        let json_start = start + 3;
        if let Some(end) = response[json_start..].find("```") {
            let candidate = response[json_start..json_start + end].trim();
            if candidate.starts_with('{') {
                return parse_plan(candidate);
            }
        }
    }
    bail!("Could not extract plan JSON from response")
}

// ---------------------------------------------------------------------------
// Executor support types
// ---------------------------------------------------------------------------

/// Result of executing a single plan action.
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub action_type: String,
    pub group: u32,
    pub success: bool,
    pub summary: String,
    pub raw_output: String,
}

impl ActionResult {
    pub fn to_accumulated_line(&self) -> String {
        let status = if self.success { "" } else { "FAILED \u{2014} " };
        format!(
            "Action \"{}\" (group {}): {}{}",
            self.action_type, self.group, status, self.summary
        )
    }
}

/// Outcome of plan_then_execute().
pub enum PlanExecutionResult {
    /// Planner decided no planning needed -- caller should use normal agent_turn.
    Passthrough,
    /// Plan was executed action-by-action.
    Executed {
        output: String,
        action_results: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Executor prompt builder
// ---------------------------------------------------------------------------

/// Build the slim executor system prompt for a single action.
pub fn build_executor_prompt(action: &PlanAction, accumulated_results: &[String]) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are executing a single action from a plan. ");
    prompt.push_str("Use the available tools to accomplish exactly what is described. ");
    prompt.push_str("Do not add, skip, or modify the action. ");
    prompt.push_str("Do not make judgment calls \u{2014} follow the instructions exactly.\n\n");

    let _ = writeln!(prompt, "ACTION TYPE: {}", action.action_type);
    let _ = writeln!(prompt, "DESCRIPTION: {}", action.description);

    if !action.params.is_null()
        && action.params != serde_json::Value::Object(serde_json::Map::default())
    {
        let _ = writeln!(prompt, "PARAMETERS: {}", action.params);
    }

    if !action.tools.is_empty() {
        let _ = writeln!(prompt, "TOOLS TO USE: {}", action.tools.join(", "));
    }

    if !accumulated_results.is_empty() {
        prompt.push_str("\nRESULTS FROM PRIOR ACTIONS:\n");
        for line in accumulated_results {
            let _ = writeln!(prompt, "- {line}");
        }
        prompt.push_str("\nUse these results (URLs, IDs) in your action when referenced.\n");
    }

    prompt
}

/// Filter tools_registry to only tools matching the action's tools list.
pub fn filter_tool_names(all_tool_names: &[String], wanted: &[String]) -> Vec<String> {
    if wanted.is_empty() {
        return all_tool_names.to_vec();
    }
    all_tool_names
        .iter()
        .filter(|name| wanted.iter().any(|w| **name == *w))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Two-phase planner/executor orchestration
// ---------------------------------------------------------------------------

/// Two-phase planner/executor flow.
///
/// 1. Calls the planner model (no tools) to produce a JSON action plan.
/// 2. Parses the response; returns `Passthrough` if the planner deems it simple.
/// 3. Otherwise executes actions group-by-group via [`run_tool_call_loop`](super::loop_::run_tool_call_loop).
#[allow(clippy::too_many_arguments)]
pub async fn plan_then_execute(
    provider: &dyn Provider,
    planner_model: &str,
    executor_model: &str,
    system_prompt: &str,
    user_message: &str,
    memory_context: &str,
    tools_registry: &[Box<dyn Tool>],
    tool_specs: &[ToolSpec],
    observer: &dyn Observer,
    provider_name: &str,
    temperature: f64,
    max_tool_iterations: usize,
    max_executor_iterations: usize,
    // Channel context
    channel_name: &str,
    cancellation_token: Option<CancellationToken>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
) -> Result<PlanExecutionResult> {
    // Build planner messages (system prompt + context + user message, NO tools)
    let planner_system = format!(
        "{system_prompt}\n\n\
        You are in planning mode. Analyze the user's request and output a JSON action plan.\n\
        Do NOT call tools or write final content. Only output the plan.\n\
        If the request is simple (direct question, single lookup, casual conversation), \
        return {{\"passthrough\": true}}.\n\
        For multi-step tasks, break them into discrete actions with type, description, params, \
        tools, and group fields.\n\
        Assign group numbers: independent actions share a group, dependent actions get higher \
        group numbers.\n\
        Never fabricate data (URLs, IDs). If you need a value, add a lookup action before the \
        action that needs it.\n\
        Include all judgment calls in the plan. The executor follows instructions; it does not \
        make decisions.\n\
        Output ONLY valid JSON, no markdown fences, no commentary."
    );

    let planner_user = if memory_context.is_empty() {
        user_message.to_string()
    } else {
        format!("{memory_context}\n{user_message}")
    };

    let planner_messages = vec![
        ChatMessage::system(planner_system),
        ChatMessage::user(planner_user),
    ];

    // Step 1: Call planner (no tools)
    observer.record_event(&ObserverEvent::PlannerRequest {
        model: planner_model.to_string(),
    });

    let response = provider
        .chat(
            ChatRequest {
                messages: &planner_messages,
                tools: None,
                route_hint: Some("planner"),
            },
            planner_model,
            temperature,
        )
        .await?;

    let response_text = response.text.unwrap_or_default();

    observer.record_event(&ObserverEvent::PlannerResponse {
        model: planner_model.to_string(),
        plan_text: response_text.clone(),
    });

    // Step 2: Parse plan
    let plan = match parse_plan_from_response(&response_text) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!("Plan parse failed ({e}), falling back to passthrough");
            return Ok(PlanExecutionResult::Passthrough);
        }
    };

    // Step 3: Check passthrough
    if plan.is_passthrough() {
        return Ok(PlanExecutionResult::Passthrough);
    }

    // Step 4: Execute action-by-action, group-by-group
    let groups = plan.grouped_actions();
    let mut accumulated: Vec<String> = Vec::new();
    let mut last_output = String::new();
    let mut any_succeeded = false;
    let mut failed_group_ids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();

    let all_tool_names: Vec<String> = tool_specs.iter().map(|s| s.name.clone()).collect();

    for group in &groups {
        // Snapshot accumulated results before this group so all actions in the
        // group see the same prior-group context (they are independent).
        let group_accumulated = accumulated.clone();

        let futures: Vec<_> = group
            .iter()
            .map(|action| {
                let executor_system = build_executor_prompt(action, &group_accumulated);
                let wanted_tools = filter_tool_names(&all_tool_names, &action.tools);

                // Combine action-level exclusions with channel-level exclusions
                let mut combined_excluded = excluded_tools.to_vec();
                if !action.tools.is_empty() {
                    // Action specifies wanted tools; exclude everything else
                    combined_excluded.extend(
                        all_tool_names
                            .iter()
                            .filter(|name| !wanted_tools.contains(name))
                            .cloned(),
                    );
                }
                combined_excluded.sort();
                combined_excluded.dedup();

                let mut action_messages = vec![
                    ChatMessage::system(executor_system),
                    ChatMessage::user(action.description.clone()),
                ];

                // Clone action metadata for the async block so we don't hold
                // a borrow on `group` across the await point.
                let action_type = action.action_type.clone();
                let action_group = action.group;
                let action_desc = action.description.clone();
                let budget = max_tool_iterations.min(max_executor_iterations);

                let ct = cancellation_token.clone();

                async move {
                    let result = crate::agent::loop_::run_tool_call_loop(
                        provider,
                        &mut action_messages,
                        tools_registry,
                        observer,
                        provider_name,
                        executor_model,
                        temperature,
                        true, // silent -- suppress stdout during executor
                        None, // no approval manager
                        channel_name,
                        &crate::config::MultimodalConfig::default(),
                        max_tool_iterations.min(max_executor_iterations),
                        ct,
                        None, // no delta sender
                        hooks,
                        &combined_excluded,
                        None, // route_hint: executor uses resolved model directly
                    )
                    .await;

                    match result {
                        Ok(output) => ActionResult {
                            action_type,
                            group: action_group,
                            success: true,
                            summary: output.clone(),
                            raw_output: output,
                        },
                        Err(e) => {
                            tracing::warn!(
                                action_type = action_type.as_str(),
                                group = action_group,
                                budget = budget,
                                description = action_desc.as_str(),
                                "Action execution failed: {e}"
                            );
                            ActionResult {
                                action_type,
                                group: action_group,
                                success: false,
                                summary: e.to_string(),
                                raw_output: String::new(),
                            }
                        }
                    }
                }
            })
            .collect();

        let results = futures_util::future::join_all(futures).await;

        for result in &results {
            accumulated.push(result.to_accumulated_line());
            if !result.success {
                failed_group_ids.insert(result.group);
            }
        }
        if let Some(last_success) = results.iter().rev().find(|r| r.success) {
            last_output = last_success.summary.clone();
            any_succeeded = true;
        }
    }

    if !any_succeeded {
        let total_actions = groups.iter().map(|g| g.len()).sum::<usize>();
        let failed_groups: Vec<u32> = failed_group_ids.into_iter().collect();
        tracing::warn!(
            total_actions = total_actions,
            failed_groups = ?failed_groups,
            "All plan actions failed; consider whether this request should passthrough"
        );
    }

    Ok(PlanExecutionResult::Executed {
        output: last_output,
        action_results: accumulated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_passthrough_plan() {
        let json = r#"{"passthrough": true}"#;
        let plan = super::parse_plan(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_action_plan() {
        let json = r#"{
            "passthrough": false,
            "actions": [
                {"group": 1, "type": "create_issue", "description": "Create issue", "tools": ["linear"], "params": {}},
                {"group": 1, "type": "create_issue", "description": "Create issue 2", "tools": ["linear"], "params": {}},
                {"group": 2, "type": "reply", "description": "Reply with links", "tools": ["slack"], "params": {}}
            ]
        }"#;
        let plan = super::parse_plan(json).unwrap();
        assert!(!plan.is_passthrough());
        assert_eq!(plan.actions.len(), 3);
        let groups = plan.grouped_actions();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[1].len(), 1);
    }

    #[test]
    fn parse_empty_actions_is_passthrough() {
        let json = r#"{"passthrough": false, "actions": []}"#;
        let plan = super::parse_plan(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let result = super::parse_plan("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn extract_json_from_markdown_fences() {
        let response = "Here's the plan:\n```json\n{\"passthrough\": true}\n```\nDone.";
        let plan = super::parse_plan_from_response(response).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn extract_json_from_bare_fences() {
        let response = "Plan:\n```\n{\"passthrough\": false, \"actions\": [{\"group\": 1, \"type\": \"test\", \"description\": \"do thing\"}]}\n```";
        let plan = super::parse_plan_from_response(response).unwrap();
        assert!(!plan.is_passthrough());
        assert_eq!(plan.actions.len(), 1);
    }

    #[test]
    fn action_defaults_to_group_1() {
        let json = r#"{"actions": [{"type": "test", "description": "do thing"}]}"#;
        let plan = super::parse_plan(json).unwrap();
        assert_eq!(plan.actions[0].group, 1);
    }

    #[test]
    fn accumulated_result_format_success() {
        let result = super::ActionResult {
            action_type: "create_issue".to_string(),
            group: 1,
            success: true,
            summary: "Created ZCL-31 \u{2014} URL: https://example.com/issue/ZCL-31".to_string(),
            raw_output: "raw".to_string(),
        };
        let line = result.to_accumulated_line();
        assert!(line.contains("create_issue"));
        assert!(line.contains("group 1"));
        assert!(line.contains("ZCL-31"));
        assert!(!line.contains("FAILED"));
    }

    #[test]
    fn accumulated_result_format_failure() {
        let result = super::ActionResult {
            action_type: "create_issue".to_string(),
            group: 1,
            success: false,
            summary: "API returned 422".to_string(),
            raw_output: String::new(),
        };
        let line = result.to_accumulated_line();
        assert!(line.contains("FAILED"));
        assert!(line.contains("422"));
    }

    #[test]
    fn build_executor_prompt_includes_action_and_results() {
        let action = super::PlanAction {
            group: 2,
            action_type: "reply".to_string(),
            description: "Reply with issue links".to_string(),
            tools: vec!["slack_reply".to_string()],
            params: serde_json::json!({"content_hint": "Created 4 issues"}),
        };
        let accumulated = vec!["Action \"create_issue\" (group 1): Created ZCL-31".to_string()];
        let prompt = super::build_executor_prompt(&action, &accumulated);
        assert!(prompt.contains("Reply with issue links"));
        assert!(prompt.contains("Created ZCL-31"));
        assert!(prompt.contains("slack_reply"));
        assert!(prompt.contains("PARAMETERS"));
    }

    #[test]
    fn build_executor_prompt_without_results() {
        let action = super::PlanAction {
            group: 1,
            action_type: "lookup".to_string(),
            description: "Look up issue details".to_string(),
            tools: vec![],
            params: serde_json::Value::Null,
        };
        let prompt = super::build_executor_prompt(&action, &[]);
        assert!(prompt.contains("Look up issue details"));
        assert!(!prompt.contains("RESULTS FROM PRIOR"));
        assert!(!prompt.contains("PARAMETERS"));
    }

    #[test]
    fn filter_tool_names_with_wanted() {
        let all = vec![
            "linear".to_string(),
            "slack".to_string(),
            "shell".to_string(),
        ];
        let wanted = vec!["slack".to_string()];
        let filtered = super::filter_tool_names(&all, &wanted);
        assert_eq!(filtered, vec!["slack"]);
    }

    #[test]
    fn filter_tool_names_empty_wanted_returns_all() {
        let all = vec!["linear".to_string(), "slack".to_string()];
        let filtered = super::filter_tool_names(&all, &[]);
        assert_eq!(filtered.len(), 2);
    }

    // Verification tests for plan_then_execute (required by verification-before-completion).
    // The function exists but is dead code with no callers; these tests verify its contracts
    // before wiring it into Agent::turn().

    use async_trait::async_trait;
    use parking_lot::Mutex;

    struct MockPlannerProvider {
        responses: Mutex<Vec<crate::providers::ChatResponse>>,
    }

    #[async_trait]
    impl crate::providers::Provider for MockPlannerProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: crate::providers::ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> Result<crate::providers::ChatResponse> {
            let mut guard = self.responses.lock();
            if guard.is_empty() {
                return Ok(crate::providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    provider_parts: None,
                });
            }
            Ok(guard.remove(0))
        }
    }

    #[tokio::test]
    async fn plan_then_execute_passthrough_json_returns_passthrough() {
        let provider = MockPlannerProvider {
            responses: Mutex::new(vec![crate::providers::ChatResponse {
                text: Some(r#"{"passthrough": true}"#.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
                provider_parts: None,
            }]),
        };
        let observer = crate::observability::NoopObserver;
        let result = super::plan_then_execute(
            &provider,
            "hint:planner",
            "hint:complex",
            "You are a helpful assistant.",
            "What is 2+2?",
            "",
            &[],
            &[],
            &observer,
            "router",
            0.7,
            5,
            15,   // max_executor_iterations
            "",   // channel_name
            None, // cancellation_token
            None, // hooks
            &[],  // excluded_tools
        )
        .await
        .expect("should not error");
        assert!(matches!(result, super::PlanExecutionResult::Passthrough));
    }

    #[tokio::test]
    async fn plan_then_execute_invalid_json_returns_passthrough() {
        let provider = MockPlannerProvider {
            responses: Mutex::new(vec![crate::providers::ChatResponse {
                text: Some("Not valid JSON at all.".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
                provider_parts: None,
            }]),
        };
        let observer = crate::observability::NoopObserver;
        let result = super::plan_then_execute(
            &provider,
            "hint:planner",
            "hint:complex",
            "System.",
            "Hello",
            "",
            &[],
            &[],
            &observer,
            "router",
            0.7,
            5,
            15,   // max_executor_iterations
            "",   // channel_name
            None, // cancellation_token
            None, // hooks
            &[],  // excluded_tools
        )
        .await
        .expect("should not error on invalid JSON");
        assert!(matches!(result, super::PlanExecutionResult::Passthrough));
    }

    #[tokio::test]
    async fn plan_then_execute_empty_actions_returns_passthrough() {
        let provider = MockPlannerProvider {
            responses: Mutex::new(vec![crate::providers::ChatResponse {
                text: Some(r#"{"passthrough": false, "actions": []}"#.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
                provider_parts: None,
            }]),
        };
        let observer = crate::observability::NoopObserver;
        let result = super::plan_then_execute(
            &provider,
            "hint:planner",
            "hint:complex",
            "System.",
            "Hello",
            "",
            &[],
            &[],
            &observer,
            "router",
            0.7,
            5,
            15,   // max_executor_iterations
            "",   // channel_name
            None, // cancellation_token
            None, // hooks
            &[],  // excluded_tools
        )
        .await
        .expect("should not error");
        assert!(matches!(result, super::PlanExecutionResult::Passthrough));
    }

    #[tokio::test]
    async fn plan_then_execute_with_actions_returns_executed() {
        let provider = MockPlannerProvider {
            responses: Mutex::new(vec![
                crate::providers::ChatResponse {
                    text: Some(r#"{"actions": [{"group": 1, "type": "lookup", "description": "Look up the answer"}]}"#.into()),
                    tool_calls: vec![], usage: None, reasoning_content: None, provider_parts: None,
                },
                crate::providers::ChatResponse {
                    text: Some("The answer is 42.".into()),
                    tool_calls: vec![], usage: None, reasoning_content: None, provider_parts: None,
                },
            ]),
        };
        let observer = crate::observability::NoopObserver;
        let result = super::plan_then_execute(
            &provider,
            "hint:planner",
            "hint:complex",
            "System.",
            "Meaning of life?",
            "",
            &[],
            &[],
            &observer,
            "router",
            0.7,
            5,
            15,   // max_executor_iterations
            "",   // channel_name
            None, // cancellation_token
            None, // hooks
            &[],  // excluded_tools
        )
        .await
        .expect("should succeed");
        match result {
            super::PlanExecutionResult::Executed {
                output,
                action_results,
            } => {
                assert_eq!(output, "The answer is 42.");
                assert_eq!(action_results.len(), 1);
                assert!(action_results[0].contains("lookup"));
            }
            super::PlanExecutionResult::Passthrough => panic!("Expected Executed"),
        }
    }

    #[tokio::test]
    async fn plan_then_execute_emits_planner_observer_events() {
        struct CapturingObserver {
            events: Mutex<Vec<String>>,
        }
        impl crate::observability::Observer for CapturingObserver {
            fn record_event(&self, event: &crate::observability::ObserverEvent) {
                let name = match event {
                    crate::observability::ObserverEvent::PlannerRequest { .. } => "PlannerRequest",
                    crate::observability::ObserverEvent::PlannerResponse { .. } => {
                        "PlannerResponse"
                    }
                    _ => return,
                };
                self.events.lock().push(name.to_string());
            }
            fn record_metric(&self, _: &crate::observability::traits::ObserverMetric) {}
            fn name(&self) -> &str {
                "capturing"
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let provider = MockPlannerProvider {
            responses: Mutex::new(vec![crate::providers::ChatResponse {
                text: Some(r#"{"passthrough": true}"#.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
                provider_parts: None,
            }]),
        };
        let observer = CapturingObserver {
            events: Mutex::new(Vec::new()),
        };
        let _ = super::plan_then_execute(
            &provider,
            "hint:planner",
            "hint:complex",
            "System.",
            "Hello",
            "",
            &[],
            &[],
            &observer,
            "router",
            0.7,
            5,
            15,   // max_executor_iterations
            "",   // channel_name
            None, // cancellation_token
            None, // hooks
            &[],  // excluded_tools
        )
        .await
        .expect("should not error");
        let events = observer.events.lock().clone();
        assert!(events.contains(&"PlannerRequest".to_string()));
        assert!(events.contains(&"PlannerResponse".to_string()));
    }
}
