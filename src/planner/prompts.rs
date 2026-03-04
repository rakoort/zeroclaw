use super::types::PlanAction;
use std::fmt::Write;

/// Build the planner system prompt (Phase 1).
///
/// The planner receives this + the original system prompt + user message.
/// It produces a JSON plan or passthrough decision.
pub fn build_planner_system_prompt(base_system_prompt: &str) -> String {
    let mut prompt = String::new();
    if !base_system_prompt.is_empty() {
        prompt.push_str(base_system_prompt);
        prompt.push_str("\n\n");
    }
    prompt.push_str(
        "You are in planning mode. Analyze the user's request and output a JSON action plan.\n\
        Do NOT call tools or write final content. Only output the plan.\n\n\
        Assess the complexity of the request:\n\
        - Simple (greeting, single question, casual chat): return {\"passthrough\": true}\n\
        - Moderate (1-3 tool calls, single concern): 1-3 actions, single group\n\
        - Complex (multi-step, multiple data sources, dependencies): 3-10 actions, multiple groups\n\
        - Ritual/sweep (structured multi-phase workflow): 5+ actions, ordered groups\n\n\
        Scale effort to match complexity. Do not over-plan simple tasks.\n\n\
        For multi-step tasks, break them into discrete actions with these fields:\n\
        - group: integer (independent actions share a group, dependent actions get higher numbers)\n\
        - type: short label for the action\n\
        - description: what the executor should do (be specific and complete)\n\
        - tools: list of tool names the executor should use\n\
        - params: any parameters the executor needs\n\
        - model_hint: optional (\"fast\" for simple tool calls, \"reasoning\" for complex analysis)\n\n\
        Rules:\n\
        - Ensure each action has non-overlapping responsibility\n\
        - Do not assign the same tool or data source to multiple actions in the same group\n\
        - Structure early groups for broad information gathering; later groups narrow focus\n\
        - Never fabricate data (URLs, IDs). Add a lookup action before any action that needs them\n\
        - Include all judgment calls in the plan. The executor follows instructions literally\n\
        - Include an \"analysis\" field explaining your reasoning about dependencies and ordering\n\n\
        Output ONLY valid JSON, no markdown fences, no commentary.",
    );
    prompt
}

/// Build the slim executor system prompt for a single action (Phase 2).
pub fn build_executor_prompt(
    action: &PlanAction,
    accumulated_results: &[String],
    plan_analysis: Option<&str>,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are executing a single action from a plan. ");
    prompt.push_str("Use the available tools to accomplish exactly what is described. ");
    prompt.push_str("Do not add, skip, or modify the action. ");
    prompt.push_str("Do not make judgment calls \u{2014} follow the instructions exactly.\n\n");

    if let Some(analysis) = plan_analysis {
        let _ = writeln!(prompt, "PLAN CONTEXT: {analysis}");
        prompt.push('\n');
    }

    let _ = writeln!(prompt, "ACTION TYPE: {}", action.action_type);
    let _ = writeln!(prompt, "DESCRIPTION: {}", action.description);

    if !action.params.is_null()
        && action.params != serde_json::Value::Object(serde_json::Map::default())
        && action.params != serde_json::Value::Array(vec![])
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

/// Build the synthesis prompt (Phase 3).
///
/// Called after all action groups complete. Produces a coherent summary.
pub fn build_synthesis_prompt(
    user_message: &str,
    analysis: Option<&str>,
    accumulated_results: &[String],
) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You executed a multi-step plan. Synthesize the results into a concise summary.\n\n",
    );

    let _ = writeln!(prompt, "ORIGINAL TASK: {user_message}");

    if let Some(analysis) = analysis {
        let _ = writeln!(prompt, "\nPLAN ANALYSIS: {analysis}");
    }

    prompt.push_str("\nACTION RESULTS:\n");
    for line in accumulated_results {
        let _ = writeln!(prompt, "- {line}");
    }

    prompt.push_str(
        "\nProduce a clear, factual summary of what was accomplished. \
        Include concrete outputs (issue URLs, message links, counts). \
        Note any failures. Do not fabricate details.",
    );

    prompt
}

/// Filter tool names to only those matching the action's tools list.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_prompt_contains_effort_scaling() {
        let prompt = build_planner_system_prompt("");
        assert!(prompt.contains("Simple"));
        assert!(prompt.contains("Complex"));
        assert!(prompt.contains("passthrough"));
        assert!(prompt.contains("non-overlapping"));
    }

    #[test]
    fn planner_prompt_prepends_base_system_prompt() {
        let prompt = build_planner_system_prompt("You are a helpful agent.");
        assert!(prompt.starts_with("You are a helpful agent."));
    }

    #[test]
    fn executor_prompt_includes_analysis() {
        let action = PlanAction {
            group: 1,
            action_type: "read".into(),
            description: "read data".into(),
            tools: vec![],
            params: serde_json::Value::Null,
            model_hint: None,
        };
        let prompt = build_executor_prompt(&action, &[], Some("multi-step sweep"));
        assert!(prompt.contains("PLAN CONTEXT: multi-step sweep"));
    }

    #[test]
    fn executor_prompt_without_analysis() {
        let action = PlanAction {
            group: 1,
            action_type: "read".into(),
            description: "read data".into(),
            tools: vec!["slack".into()],
            params: serde_json::Value::Null,
            model_hint: None,
        };
        let prompt = build_executor_prompt(&action, &[], None);
        assert!(!prompt.contains("PLAN CONTEXT"));
        assert!(prompt.contains("TOOLS TO USE: slack"));
    }

    #[test]
    fn executor_prompt_includes_accumulated_results() {
        let action = PlanAction {
            group: 2,
            action_type: "create".into(),
            description: "create issues".into(),
            tools: vec![],
            params: serde_json::Value::Null,
            model_hint: None,
        };
        let prior = vec!["Action \"read\" (group 1): Found 5 messages".into()];
        let prompt = build_executor_prompt(&action, &prior, None);
        assert!(prompt.contains("RESULTS FROM PRIOR ACTIONS"));
        assert!(prompt.contains("Found 5 messages"));
    }

    #[test]
    fn synthesis_prompt_includes_all_sections() {
        let results = vec![
            "Action \"read\" (group 1): Read 10 messages".into(),
            "Action \"create\" (group 2): Created 3 issues".into(),
        ];
        let prompt = build_synthesis_prompt("Run the sweep", Some("5-phase sweep"), &results);
        assert!(prompt.contains("ORIGINAL TASK: Run the sweep"));
        assert!(prompt.contains("PLAN ANALYSIS: 5-phase sweep"));
        assert!(prompt.contains("Read 10 messages"));
        assert!(prompt.contains("Created 3 issues"));
        assert!(prompt.contains("Do not fabricate"));
    }

    #[test]
    fn executor_prompt_includes_params_when_present() {
        let action = PlanAction {
            group: 1,
            action_type: "fetch".into(),
            description: "fetch data".into(),
            tools: vec![],
            params: serde_json::json!({"url": "https://example.com"}),
            model_hint: None,
        };
        let prompt = build_executor_prompt(&action, &[], None);
        assert!(prompt.contains("PARAMETERS:"));
        assert!(prompt.contains("example.com"));
    }

    #[test]
    fn executor_prompt_skips_params_when_empty_object_or_array() {
        let action_empty_obj = PlanAction {
            group: 1,
            action_type: "read".into(),
            description: "read data".into(),
            tools: vec![],
            params: serde_json::Value::Object(serde_json::Map::default()),
            model_hint: None,
        };
        assert!(!build_executor_prompt(&action_empty_obj, &[], None).contains("PARAMETERS:"));

        let action_empty_arr = PlanAction {
            group: 1,
            action_type: "read".into(),
            description: "read data".into(),
            tools: vec![],
            params: serde_json::Value::Array(vec![]),
            model_hint: None,
        };
        assert!(!build_executor_prompt(&action_empty_arr, &[], None).contains("PARAMETERS:"));
    }

    #[test]
    fn filter_tool_names_returns_all_when_wanted_empty() {
        let all = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(filter_tool_names(&all, &[]), all);
    }

    #[test]
    fn filter_tool_names_filters_to_wanted() {
        let all = vec!["a".into(), "b".into(), "c".into()];
        let wanted = vec!["b".into()];
        assert_eq!(filter_tool_names(&all, &wanted), vec!["b".to_string()]);
    }
}
