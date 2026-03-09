//! Classifier-based fast-path routing.
//!
//! Determines whether an incoming message should bypass the planner (fast path)
//! or go through the full planner pipeline, based on the classifier decision.

use crate::agent::classifier::ClassificationDecision;
use crate::config::schema::Tier;
use crate::providers::ChatMessage;
use std::fmt::Write;

/// Routing decision produced by [`route_decision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Classifier says Simple with high confidence — run flat tool loop
    /// with a tight iteration budget, skipping the planner.
    FastPath,
    /// Use the full planner pipeline.
    PlannerPath,
    /// No planner configured — go directly to the flat tool loop with the
    /// normal iteration budget.
    DirectLoop,
}

/// Decide the execution route for a message based on classifier output.
///
/// Returns [`RouteDecision::FastPath`] when all of:
/// - A classifier decision is available
/// - The tier is `Simple`
/// - The confidence meets or exceeds `confidence_threshold`
/// - A planner model is configured (`has_planner` is true)
///
/// Returns [`RouteDecision::PlannerPath`] when a planner is configured but
/// the fast-path conditions are not met.
///
/// Returns [`RouteDecision::DirectLoop`] when no planner is configured.
pub fn route_decision(
    decision: Option<&ClassificationDecision>,
    confidence_threshold: f64,
    has_planner: bool,
) -> RouteDecision {
    if !has_planner {
        return RouteDecision::DirectLoop;
    }

    if let Some(d) = decision {
        if d.tier == Tier::Simple && d.confidence >= confidence_threshold {
            return RouteDecision::FastPath;
        }
    }

    RouteDecision::PlannerPath
}

/// Maximum characters retained per tool result snippet in the fast-path summary.
const SUMMARY_RESULT_MAX_CHARS: usize = 120;

/// Summarize the tool calls made during a fast-path attempt.
///
/// Walks the history slice `history[start_idx..]` and extracts tool call names
/// from assistant messages and brief result snippets from tool/user result
/// messages.  Returns a compact context block the planner can use to avoid
/// re-doing work.
///
/// Returns an empty string if no tool calls were found in the slice.
pub fn summarize_fast_path_history(history: &[ChatMessage], start_idx: usize) -> String {
    if start_idx >= history.len() {
        return String::new();
    }

    let mut entries: Vec<String> = Vec::new();

    for msg in &history[start_idx..] {
        if msg.role == "assistant" {
            // Try to extract tool call names from JSON-structured assistant messages.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                if let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array()) {
                    for call in calls {
                        let name = call
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let args_hint = call
                            .get("arguments")
                            .and_then(|a| {
                                // For shell: show command; for file ops: show path.
                                a.get("command")
                                    .or_else(|| a.get("path"))
                                    .or_else(|| a.get("query"))
                                    .or_else(|| a.get("action"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("");
                        let hint = crate::util::truncate_with_ellipsis(args_hint, 60);
                        if hint.is_empty() {
                            entries.push(format!("- Called `{name}`"));
                        } else {
                            entries.push(format!("- Called `{name}`: {hint}"));
                        }
                    }
                }
            }
        } else if msg.role == "tool" {
            // Extract brief result snippet from tool result messages.
            let content_str = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg.content)
            {
                v.get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or(&msg.content)
                    .to_string()
            } else {
                msg.content.clone()
            };
            let snippet =
                crate::util::truncate_with_ellipsis(&content_str, SUMMARY_RESULT_MAX_CHARS);
            if !snippet.is_empty() && !snippet.starts_with("[Cleared:") {
                entries.push(format!("  Result: {snippet}"));
            }
        } else if msg.role == "user" && msg.content.starts_with("[Tool results]") {
            // Prompt-mode tool results (XML-based format).
            let snippet =
                crate::util::truncate_with_ellipsis(&msg.content, SUMMARY_RESULT_MAX_CHARS);
            entries.push(format!("  Result: {snippet}"));
        }
    }

    if entries.is_empty() {
        return String::new();
    }

    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "\n## Fast-Path Context\nThe following tool calls were already made during a fast-path attempt \
         ({} iterations) but the task was not completed:",
        entries.iter().filter(|e| e.starts_with("- Called")).count()
    );
    for entry in &entries {
        let _ = writeln!(summary, "{entry}");
    }
    let _ = writeln!(
        summary,
        "Build on these results rather than re-doing the same calls.\n"
    );
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::classifier::ClassificationDecision;
    use crate::config::schema::Tier;
    use crate::providers::ChatMessage;

    fn simple_decision(confidence: f64) -> ClassificationDecision {
        ClassificationDecision {
            tier: Tier::Simple,
            confidence,
            ..ClassificationDecision::default()
        }
    }

    fn complex_decision(confidence: f64) -> ClassificationDecision {
        ClassificationDecision {
            tier: Tier::Complex,
            confidence,
            ..ClassificationDecision::default()
        }
    }

    fn medium_decision(confidence: f64) -> ClassificationDecision {
        ClassificationDecision {
            tier: Tier::Medium,
            confidence,
            ..ClassificationDecision::default()
        }
    }

    // ── route_decision tests ─────────────────────────────────────

    #[test]
    fn simple_high_confidence_routes_to_fast_path() {
        let d = simple_decision(0.9);
        assert_eq!(route_decision(Some(&d), 0.8, true), RouteDecision::FastPath,);
    }

    #[test]
    fn simple_at_threshold_routes_to_fast_path() {
        let d = simple_decision(0.8);
        assert_eq!(route_decision(Some(&d), 0.8, true), RouteDecision::FastPath,);
    }

    #[test]
    fn simple_below_threshold_routes_to_planner() {
        let d = simple_decision(0.79);
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn medium_tier_routes_to_planner() {
        let d = medium_decision(0.95);
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn complex_tier_routes_to_planner() {
        let d = complex_decision(0.95);
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn reasoning_tier_routes_to_planner() {
        let d = ClassificationDecision {
            tier: Tier::Reasoning,
            confidence: 0.99,
            ..ClassificationDecision::default()
        };
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn no_planner_routes_to_direct_loop() {
        let d = simple_decision(0.95);
        assert_eq!(
            route_decision(Some(&d), 0.8, false),
            RouteDecision::DirectLoop,
        );
    }

    #[test]
    fn no_planner_no_decision_routes_to_direct_loop() {
        assert_eq!(route_decision(None, 0.8, false), RouteDecision::DirectLoop,);
    }

    #[test]
    fn no_decision_with_planner_routes_to_planner() {
        assert_eq!(route_decision(None, 0.8, true), RouteDecision::PlannerPath,);
    }

    #[test]
    fn zero_confidence_threshold_routes_all_simple_to_fast_path() {
        let d = simple_decision(0.01);
        assert_eq!(route_decision(Some(&d), 0.0, true), RouteDecision::FastPath,);
    }

    #[test]
    fn threshold_of_one_rejects_near_certain_simple() {
        let d = simple_decision(0.999);
        assert_eq!(
            route_decision(Some(&d), 1.0, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn threshold_of_one_accepts_exact_one() {
        let d = simple_decision(1.0);
        assert_eq!(route_decision(Some(&d), 1.0, true), RouteDecision::FastPath,);
    }

    // ── Config threading tests ───────────────────────────────────
    //
    // Verify that route_decision produces correct results for the default
    // config values (simple_routing_confidence=0.8) and custom overrides,
    // ensuring the orchestrator threads config values correctly.

    #[test]
    fn default_config_threshold_routes_simple_high_confidence() {
        let default_cfg = crate::config::AgentConfig::default();
        let d = simple_decision(0.85);
        assert_eq!(
            route_decision(Some(&d), default_cfg.simple_routing_confidence, true),
            RouteDecision::FastPath,
        );
    }

    #[test]
    fn default_config_threshold_rejects_simple_low_confidence() {
        let default_cfg = crate::config::AgentConfig::default();
        let d = simple_decision(0.75);
        assert_eq!(
            route_decision(Some(&d), default_cfg.simple_routing_confidence, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn custom_config_threshold_respected() {
        // Simulate a config with a lower threshold (0.5) and verify
        // that a Simple decision with confidence=0.6 qualifies.
        let d = simple_decision(0.6);
        let custom_threshold = 0.5;
        assert_eq!(
            route_decision(Some(&d), custom_threshold, true),
            RouteDecision::FastPath,
        );
        // Same decision would NOT qualify at the default threshold.
        let default_cfg = crate::config::AgentConfig::default();
        assert_eq!(
            route_decision(Some(&d), default_cfg.simple_routing_confidence, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn default_simple_max_iterations_is_three() {
        let cfg = crate::config::AgentConfig::default();
        assert_eq!(cfg.simple_max_iterations, 3);
    }

    // ── summarize_fast_path_history tests ─────────────────────────

    #[test]
    fn summarize_empty_history_returns_empty() {
        let history: Vec<ChatMessage> = vec![];
        assert_eq!(summarize_fast_path_history(&history, 0), "");
    }

    #[test]
    fn summarize_start_beyond_history_returns_empty() {
        let history = vec![ChatMessage::user("hello")];
        assert_eq!(summarize_fast_path_history(&history, 5), "");
    }

    #[test]
    fn summarize_no_tool_calls_returns_empty() {
        let history = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];
        assert_eq!(summarize_fast_path_history(&history, 1), "");
    }

    #[test]
    fn summarize_native_tool_calls_extracts_names() {
        let assistant_content = serde_json::json!({
            "content": null,
            "tool_calls": [
                {"id": "call_1", "name": "shell", "arguments": {"command": "ls -la"}},
                {"id": "call_2", "name": "file_read", "arguments": {"path": "/tmp/foo.txt"}}
            ]
        })
        .to_string();
        let tool_result = serde_json::json!({
            "tool_call_id": "call_1",
            "content": "total 8\ndrwxr-xr-x 2 user user 4096 file1.txt"
        })
        .to_string();

        let history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("list files"),
            ChatMessage::assistant(assistant_content),
            ChatMessage::tool(tool_result),
        ];

        let summary = summarize_fast_path_history(&history, 2);
        assert!(summary.contains("## Fast-Path Context"));
        assert!(summary.contains("Called `shell`: ls -la"));
        assert!(summary.contains("Called `file_read`: /tmp/foo.txt"));
        assert!(summary.contains("Result:"));
        assert!(summary.contains("Build on these results"));
    }

    #[test]
    fn summarize_prompt_mode_tool_results() {
        let history = vec![
            ChatMessage::user("do something"),
            ChatMessage::assistant("I'll run a command"),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"shell\">\ncommand output\n</tool_result>",
            ),
        ];

        let summary = summarize_fast_path_history(&history, 0);
        assert!(summary.contains("Result:"));
        assert!(summary.contains("[Tool results]"));
    }

    #[test]
    fn summarize_skips_cleared_results() {
        let tool_result = serde_json::json!({
            "tool_call_id": "call_1",
            "content": "[Cleared: shell returned 500 bytes]"
        })
        .to_string();
        let history = vec![ChatMessage::tool(tool_result)];

        let summary = summarize_fast_path_history(&history, 0);
        // Cleared results should not appear in the summary.
        assert_eq!(summary, "");
    }

    #[test]
    fn summarize_respects_start_index() {
        let early_assistant = serde_json::json!({
            "content": null,
            "tool_calls": [{"id": "old", "name": "memory_recall", "arguments": {"query": "prefs"}}]
        })
        .to_string();
        let late_assistant = serde_json::json!({
            "content": null,
            "tool_calls": [{"id": "new", "name": "shell", "arguments": {"command": "date"}}]
        })
        .to_string();

        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("what time"),
            ChatMessage::assistant(early_assistant),
            ChatMessage::assistant(late_assistant),
        ];

        // Start at index 3: only the late assistant message is included.
        let summary = summarize_fast_path_history(&history, 3);
        assert!(summary.contains("Called `shell`: date"));
        assert!(!summary.contains("memory_recall"));
    }

    #[test]
    fn summarize_counts_tool_calls_not_results() {
        let assistant_content = serde_json::json!({
            "content": null,
            "tool_calls": [
                {"id": "c1", "name": "shell", "arguments": {"command": "echo a"}},
                {"id": "c2", "name": "shell", "arguments": {"command": "echo b"}}
            ]
        })
        .to_string();

        let history = vec![ChatMessage::assistant(assistant_content)];

        let summary = summarize_fast_path_history(&history, 0);
        // The header should count 2 tool calls (iterations).
        assert!(summary.contains("(2 iterations)"));
    }

    // ── Orchestrator integration gap ─────────────────────────────
    //
    // The following behaviors are exercised by the orchestrator at runtime
    // but cannot be unit-tested here without a full mock provider + channel
    // stack:
    //
    //   1. Planner is actually skipped when route_decision returns FastPath
    //      (verified by: tracing log "Classifier fast path: Simple tier,
    //      skipping planner" and absence of plan_then_execute call)
    //
    //   2. simple_max_iterations is enforced as the iteration cap on the
    //      fast-path run_tool_call_loop call
    //      (verified by: ctx.simple_max_iterations passed as max_tool_iterations
    //      parameter at orchestrator.rs ~line 1493)
    //
    //   3. Budget exhaustion triggers escalation to planner with accumulated
    //      context via summarize_fast_path_history
    //      (verified by: escalation block at orchestrator.rs ~line 1594 checks
    //      `used_fast_path && planner_response.is_none()` and passes
    //      escalation_content to plan_then_execute)
    //
    // These are covered by code-path analysis and tracing assertions. If a
    // mock-provider test harness is added in the future, these should be
    // promoted to proper integration tests.
}
