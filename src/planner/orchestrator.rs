use anyhow::Result;
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

const COMPRESS_LINE_MAX: usize = 500;

/// Compress accumulated action result lines for inter-group executor context.
///
/// Two-stage:
/// 1. Truncate individual lines to COMPRESS_LINE_MAX characters.
/// 2. If total length exceeds `max_chars`, keep the most recent lines that
///    fit and prepend a count placeholder. The synthesis phase always receives
///    the full uncompressed results — this only controls inter-group context.
fn compress_accumulated_lines(lines: &[String], max_chars: usize) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }

    // Stage 1: truncate individual lines
    let truncated: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.chars().count() > COMPRESS_LINE_MAX {
                let byte_idx = line
                    .char_indices()
                    .nth(COMPRESS_LINE_MAX)
                    .map(|(i, _)| i)
                    .unwrap_or(line.len());
                format!("{}...", &line[..byte_idx])
            } else {
                line.clone()
            }
        })
        .collect();

    // Stage 2: rolling window if still over budget
    let total: usize = truncated.iter().map(|l| l.len()).sum();
    if total <= max_chars {
        return truncated;
    }

    // Keep as many recent lines as fit, from the end
    let mut kept: Vec<&String> = Vec::new();
    let mut used = 0usize;
    for line in truncated.iter().rev() {
        if used + line.len() + 1 > max_chars {
            break;
        }
        kept.push(line);
        used += line.len() + 1;
    }
    kept.reverse();

    let dropped = truncated.len() - kept.len();
    let mut result = Vec::with_capacity(kept.len() + 1);
    result.push(format!(
        "[{dropped} earlier actions completed — see synthesis for details]"
    ));
    result.extend(kept.into_iter().cloned());
    result
}

/// Resolve the iteration budget for a single action.
///
/// Precedence (highest to lowest):
/// 1. `action_max_iterations` — per-action override from the plan (if `> 0`).
/// 2. `global_max` — the caller's `max_executor_iterations`.
///
/// The result is then capped by `max_tool_iterations` so it never exceeds the
/// hard tool-loop ceiling.  A value of `0` in `action_max_iterations` is
/// treated as "unset" (zero-boundary guard).
fn resolve_action_budget(
    action_max_iterations: Option<u32>,
    global_max: usize,
    max_tool_iterations: usize,
) -> usize {
    let action_max = action_max_iterations
        .map(|n| n as usize)
        .filter(|&n| n > 0)
        .unwrap_or(global_max);
    max_tool_iterations.min(action_max)
}

/// Three-phase planner/executor/synthesizer orchestration.
///
/// 1. Calls the planner model (no tools) to produce a JSON action plan.
/// 2. Executes actions group-by-group via `run_tool_call_loop`.
/// 3. Synthesizes all action results into a coherent summary.
#[allow(clippy::too_many_arguments, clippy::implicit_hasher)]
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

    let all_tool_names: Vec<String> = tool_specs.iter().map(|s| s.name.clone()).collect();

    for group in &groups {
        let group_accumulated = compress_accumulated_lines(&accumulated, 3000);

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
                let budget = resolve_action_budget(
                    action.max_iterations,
                    max_executor_iterations,
                    max_tool_iterations,
                );
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

        for (action, result) in group.iter().zip(results.iter()) {
            accumulated.push(result.to_accumulated_line());
            if result.success {
                succeeded_count += 1;
            }
            if action.critical && !result.success {
                return Err(anyhow::anyhow!(
                    "Critical action '{}' (group {}) failed: {}",
                    action.action_type,
                    action.group,
                    result.summary
                ));
            }
        }
        if let Some(last_success) = results.iter().rev().find(|r| r.success) {
            last_output = last_success.summary.clone();
            any_succeeded = true;
        }
    }

    let total_actions: usize = groups.iter().map(|g| g.len()).sum();

    // ── Phase 3: Synthesize ──────────────────────────────────────────

    let should_synthesize = match plan.require_synthesis {
        Some(true) => true,
        Some(false) => false,
        None => succeeded_count >= 2,
    };

    let output = if should_synthesize {
        // Multiple actions or forced — synthesize results
        let synthesis_system = build_synthesis_prompt(user_message, plan_analysis, &accumulated);

        let synthesis_messages = vec![
            ChatMessage::system(synthesis_system),
            ChatMessage::user("Synthesize the results.".to_string()),
        ];

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
    } else {
        // Skip synthesis — use raw last output
        last_output
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
            "synthesized": should_synthesize,
        }),
    );

    Ok(PlanExecutionResult::Executed {
        output,
        action_results: accumulated,
        analysis: plan.analysis,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_empty_returns_empty_vec() {
        let result = compress_accumulated_lines(&[], 3000);
        assert!(result.is_empty());
    }

    #[test]
    fn compress_under_budget_returns_lines_unchanged() {
        let lines = vec![
            "Action \"read\" (group 1): Found 5 messages".to_string(),
            "Action \"create\" (group 2): Created 3 issues".to_string(),
        ];
        let result = compress_accumulated_lines(&lines, 3000);
        assert_eq!(result, lines);
    }

    #[test]
    fn compress_truncates_long_lines() {
        let long_summary = "x".repeat(600);
        let line = format!("Action \"read\" (group 1): {long_summary}");
        let result = compress_accumulated_lines(&[line], 3000);
        assert_eq!(result.len(), 1);
        assert!(result[0].len() <= 503); // 500 chars + "..."
        assert!(result[0].ends_with("..."));
    }

    #[test]
    fn compress_applies_rolling_window_over_budget() {
        // 20 lines x ~147 chars each approx 2940 chars total; budget of 1000 forces rolling window
        let lines: Vec<String> = (0..20)
            .map(|i| {
                format!(
                    "Action \"step{}\" (group {}): {}",
                    i,
                    i,
                    "result data ".repeat(10)
                )
            })
            .collect();
        let result = compress_accumulated_lines(&lines, 1000);
        assert!(result.len() < lines.len());
        assert!(result[0].contains("earlier actions completed"));
    }

    #[test]
    fn compress_single_line_under_budget_unchanged() {
        let lines = vec!["Action \"read\" (group 1): short result".to_string()];
        let result = compress_accumulated_lines(&lines, 3000);
        assert_eq!(result, lines);
    }

    // adaptive synthesis logic

    #[test]
    fn adaptive_synthesis_none_with_zero_successes_skips() {
        let require_synthesis: Option<bool> = None;
        let succeeded_count: usize = 0;
        let should_synthesize = match require_synthesis {
            Some(true) => true,
            Some(false) => false,
            None => succeeded_count >= 2,
        };
        assert!(!should_synthesize);
    }

    #[test]
    fn adaptive_synthesis_none_with_one_success_skips() {
        let require_synthesis: Option<bool> = None;
        let succeeded_count: usize = 1;
        let should_synthesize = match require_synthesis {
            Some(true) => true,
            Some(false) => false,
            None => succeeded_count >= 2,
        };
        assert!(!should_synthesize);
    }

    #[test]
    fn adaptive_synthesis_none_with_two_successes_synthesizes() {
        let require_synthesis: Option<bool> = None;
        let succeeded_count: usize = 2;
        let should_synthesize = match require_synthesis {
            Some(true) => true,
            Some(false) => false,
            None => succeeded_count >= 2,
        };
        assert!(should_synthesize);
    }

    #[test]
    fn adaptive_synthesis_force_true_synthesizes_regardless() {
        let require_synthesis: Option<bool> = Some(true);
        let succeeded_count: usize = 0;
        let should_synthesize = match require_synthesis {
            Some(true) => true,
            Some(false) => false,
            None => succeeded_count >= 2,
        };
        assert!(should_synthesize);
    }

    #[test]
    fn adaptive_synthesis_force_false_skips_regardless() {
        let require_synthesis: Option<bool> = Some(false);
        let succeeded_count: usize = 5;
        let should_synthesize = match require_synthesis {
            Some(true) => true,
            Some(false) => false,
            None => succeeded_count >= 2,
        };
        assert!(!should_synthesize);
    }

    // per-action budget

    #[test]
    fn per_action_budget_uses_action_max_iterations_when_set() {
        let budget = resolve_action_budget(Some(10), 30, 50);
        assert_eq!(budget, 10);
    }

    #[test]
    fn per_action_budget_falls_back_to_global_when_none() {
        let budget = resolve_action_budget(None, 30, 50);
        assert_eq!(budget, 30);
    }
}
