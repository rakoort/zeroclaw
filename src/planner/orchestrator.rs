use anyhow::Result;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::observability::{runtime_trace, Observer, ObserverEvent};
use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::tools::{Tool, ToolSpec};

use super::parser::parse_plan_from_response;
use super::prompts::{
    build_executor_prompt, build_planner_system_prompt, build_synthesis_prompt, filter_tool_names,
};
use super::types::{ActionResult, PlanExecutionResult};

/// Three-phase planner/executor/synthesizer orchestration.
///
/// 1. Calls the planner model (no tools) to produce a JSON action plan.
/// 2. Executes actions group-by-group via `run_tool_call_loop`.
/// 3. Synthesizes all action results into a coherent summary.
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
    channel_name: &str,
    cancellation_token: Option<CancellationToken>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    model_routes: &HashMap<String, String>,
) -> Result<PlanExecutionResult> {
    // ── Phase 1: Plan ────────────────────────────────────────────────

    let planner_system = build_planner_system_prompt(system_prompt);

    let planner_user = if memory_context.is_empty() {
        user_message.to_string()
    } else {
        format!("{memory_context}\n{user_message}")
    };

    let planner_messages = vec![
        ChatMessage::system(planner_system),
        ChatMessage::user(planner_user),
    ];

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

    // Parse plan
    let plan = match parse_plan_from_response(&response_text) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!("Plan parse failed ({e}), falling back to passthrough");
            runtime_trace::record_event(
                "plan_end",
                Some(channel_name),
                Some(provider_name),
                Some(planner_model),
                None,
                None,
                Some(&e.to_string()),
                serde_json::json!({ "passthrough": true, "reason": "parse_error" }),
            );
            return Ok(PlanExecutionResult::Passthrough);
        }
    };

    // Check passthrough
    if plan.is_passthrough() {
        runtime_trace::record_event(
            "plan_end",
            Some(channel_name),
            Some(provider_name),
            Some(planner_model),
            None,
            None,
            None,
            serde_json::json!({
                "passthrough": true,
                "reason": if plan.passthrough { "passthrough_flag" } else { "empty_actions" },
            }),
        );
        return Ok(PlanExecutionResult::Passthrough);
    }

    // ── Phase 2: Execute ─────────────────────────────────────────────

    let groups = plan.grouped_actions();
    let plan_started = Instant::now();
    let plan_analysis = plan.analysis.as_deref();

    runtime_trace::record_event(
        "plan_start",
        Some(channel_name),
        Some(provider_name),
        Some(executor_model),
        None,
        None,
        None,
        serde_json::json!({
            "action_count": plan.actions.len(),
            "group_count": groups.len(),
            "actions": plan.actions.iter().map(|a| serde_json::json!({
                "action_type": &a.action_type,
                "group": a.group,
                "description": &a.description,
                "model_hint": &a.model_hint,
            })).collect::<Vec<_>>(),
            "planner_model": planner_model,
            "executor_model": executor_model,
            "analysis": plan_analysis,
        }),
    );

    let mut accumulated: Vec<String> = Vec::new();
    let mut last_output = String::new();
    let mut any_succeeded = false;
    let mut succeeded_count: usize = 0;
    let mut failed_group_ids: BTreeMap<u32, bool> = BTreeMap::new();

    let all_tool_names: Vec<String> = tool_specs.iter().map(|s| s.name.clone()).collect();

    for group in &groups {
        let group_accumulated = accumulated.clone();

        let futures: Vec<_> = group
            .iter()
            .enumerate()
            .map(|(action_index, action)| {
                let executor_system =
                    build_executor_prompt(action, &group_accumulated, plan_analysis);
                let wanted_tools = filter_tool_names(&all_tool_names, &action.tools);

                // Resolve executor model for this action.
                // Clone model_hint before entering async block to avoid holding
                // a borrow on `action` (&&PlanAction) across the await point.
                let action_model_hint = action.model_hint.clone();
                let action_model = action_model_hint
                    .as_ref()
                    .and_then(|hint| model_routes.get(hint))
                    .map(String::as_str)
                    .unwrap_or(executor_model)
                    .to_string();

                let mut combined_excluded = excluded_tools.to_vec();
                if !action.tools.is_empty() {
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

                let action_type = action.action_type.clone();
                let action_group = action.group;
                let action_desc = action.description.clone();
                let budget = max_tool_iterations.min(max_executor_iterations);
                let ct = cancellation_token.clone();

                async move {
                    runtime_trace::record_event(
                        "action_start",
                        Some(channel_name),
                        Some(provider_name),
                        Some(&action_model),
                        None,
                        None,
                        None,
                        serde_json::json!({
                            "action_index": action_index,
                            "action_type": &action_type,
                            "group": action_group,
                            "description": &action_desc,
                            "model_hint": &action_model_hint,
                            "iteration_budget": budget,
                        }),
                    );

                    let action_started = Instant::now();

                    let result = crate::agent::loop_::run_tool_call_loop(
                        provider,
                        &mut action_messages,
                        tools_registry,
                        observer,
                        provider_name,
                        &action_model,
                        temperature,
                        true, // silent
                        None, // no approval manager
                        channel_name,
                        &crate::config::MultimodalConfig::default(),
                        budget,
                        ct,
                        None, // no delta sender
                        hooks,
                        &combined_excluded,
                        None, // route_hint: executor uses resolved model directly
                    )
                    .await;

                    let action_result = match result {
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
                    };

                    runtime_trace::record_event(
                        "action_end",
                        Some(channel_name),
                        Some(provider_name),
                        Some(&action_model),
                        None,
                        Some(action_result.success),
                        if action_result.success {
                            None
                        } else {
                            Some(&action_result.summary)
                        },
                        serde_json::json!({
                            "action_index": action_index,
                            "action_type": &action_result.action_type,
                            "group": action_result.group,
                            "duration_ms": action_started.elapsed().as_millis(),
                        }),
                    );

                    action_result
                }
            })
            .collect();

        let results = futures_util::future::join_all(futures).await;

        for result in &results {
            accumulated.push(result.to_accumulated_line());
            if result.success {
                succeeded_count += 1;
            } else {
                failed_group_ids.insert(result.group, true);
            }
        }
        if let Some(last_success) = results.iter().rev().find(|r| r.success) {
            last_output = last_success.summary.clone();
            any_succeeded = true;
        }
    }

    let total_actions: usize = groups.iter().map(|g| g.len()).sum();

    // ── Phase 3: Synthesize ──────────────────────────────────────────

    let output = if succeeded_count <= 1 {
        // Single action or all failed — skip synthesis, use raw output
        last_output
    } else {
        // Multiple actions — synthesize results
        let synthesis_system = build_synthesis_prompt(user_message, plan_analysis, &accumulated);

        let synthesis_messages = vec![ChatMessage::system(synthesis_system)];

        match provider
            .chat(
                ChatRequest {
                    messages: &synthesis_messages,
                    tools: None,
                    route_hint: Some("planner"),
                },
                planner_model,
                temperature,
            )
            .await
        {
            Ok(resp) => resp.text.unwrap_or(last_output),
            Err(e) => {
                tracing::warn!("Synthesis failed ({e}), using last action output");
                last_output
            }
        }
    };

    runtime_trace::record_event(
        "plan_end",
        Some(channel_name),
        Some(provider_name),
        Some(executor_model),
        None,
        Some(any_succeeded),
        None,
        serde_json::json!({
            "total_actions": total_actions,
            "succeeded": succeeded_count,
            "failed": total_actions.saturating_sub(succeeded_count),
            "duration_ms": plan_started.elapsed().as_millis(),
            "passthrough": false,
            "synthesized": succeeded_count > 1,
        }),
    );

    Ok(PlanExecutionResult::Executed {
        output,
        action_results: accumulated,
        analysis: plan.analysis,
    })
}

#[cfg(test)]
mod tests {}
