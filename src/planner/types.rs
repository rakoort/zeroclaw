use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub analysis: Option<String>,
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
    #[serde(default)]
    pub model_hint: Option<String>,
}

fn default_group() -> u32 {
    1
}

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
    /// Planner deemed the task simple — caller runs flat tool loop.
    Passthrough,
    /// Plan was executed action-by-action with synthesized output.
    Executed {
        output: String,
        action_results: Vec<String>,
        analysis: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_passthrough_flag() {
        let json = r#"{"passthrough": true}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn plan_empty_actions_is_passthrough() {
        let json = r#"{"passthrough": false, "actions": []}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn plan_with_analysis() {
        let json = r#"{"analysis": "test reasoning", "passthrough": false, "actions": [{"type": "read", "description": "read data"}]}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.analysis.as_deref(), Some("test reasoning"));
        assert!(!plan.is_passthrough());
    }

    #[test]
    fn plan_action_with_model_hint() {
        let json = r#"{"type": "enrich", "description": "enrich data", "model_hint": "reasoning"}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert_eq!(action.model_hint.as_deref(), Some("reasoning"));
    }

    #[test]
    fn plan_action_model_hint_defaults_to_none() {
        let json = r#"{"type": "read", "description": "read data"}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert!(action.model_hint.is_none());
    }

    #[test]
    fn grouped_actions_sorts_by_group() {
        let json = r#"{
            "actions": [
                {"group": 2, "type": "b", "description": "second"},
                {"group": 1, "type": "a", "description": "first"},
                {"group": 1, "type": "c", "description": "first-parallel"}
            ]
        }"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        let groups = plan.grouped_actions();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2); // group 1: two actions
        assert_eq!(groups[1].len(), 1); // group 2: one action
    }

    #[test]
    fn action_result_accumulated_line_success() {
        let result = ActionResult {
            action_type: "read".into(),
            group: 1,
            success: true,
            summary: "Read 10 messages".into(),
            raw_output: String::new(),
        };
        assert_eq!(
            result.to_accumulated_line(),
            r#"Action "read" (group 1): Read 10 messages"#
        );
    }

    #[test]
    fn action_result_accumulated_line_failure() {
        let result = ActionResult {
            action_type: "create".into(),
            group: 2,
            success: false,
            summary: "API error".into(),
            raw_output: String::new(),
        };
        assert!(result.to_accumulated_line().contains("FAILED"));
    }
}
