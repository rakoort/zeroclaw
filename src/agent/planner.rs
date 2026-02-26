use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write;

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

#[cfg(test)]
mod tests {
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
}
