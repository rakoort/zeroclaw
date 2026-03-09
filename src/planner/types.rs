use anyhow::Result;
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
    #[serde(default)]
    pub require_synthesis: Option<bool>,
}

/// Wrapper for the TOML plan file format, where the plan is nested
/// under a `[plan]` top-level key.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanFile {
    pub plan: Plan,
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
    /// Action type label. JSON plans use `"type"`, TOML plans use `"action_type"`.
    #[serde(rename = "type", alias = "action_type")]
    pub action_type: String,
    pub description: String,
    /// Optional detailed prompt for the executor. When present, the executor
    /// uses this as the user message instead of `description`. Primarily used
    /// in TOML plan files where `description` is a short label and `prompt`
    /// carries the full instruction.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub model_hint: Option<String>,
    #[serde(default)]
    pub critical: bool,
    /// Per-action override for the tool iteration budget.
    /// `None` (default) uses the global `max_executor_iterations`.
    /// `0` is treated the same as `None` — use the global default.
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

impl PlanAction {
    /// Return the executor instruction: `prompt` if set, otherwise `description`.
    pub fn executor_instruction(&self) -> &str {
        self.prompt.as_deref().unwrap_or(&self.description)
    }
}

fn default_group() -> u32 {
    1
}

/// Resolve template variables in a TOML plan file string before parsing.
///
/// Supported variables:
/// - `{{date}}` — current date in YYYY-MM-DD format
/// - `{{job_name}}` — the cron job name
pub fn resolve_plan_template(toml_content: &str, job_name: &str) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    toml_content
        .replace("{{date}}", &today)
        .replace("{{job_name}}", job_name)
}

/// Deserialize a TOML plan file into a `Plan`.
///
/// The TOML format wraps the plan under a `[plan]` key:
/// ```toml
/// [plan]
/// require_synthesis = true
///
/// [[plan.actions]]
/// group = 1
/// action_type = "delegate"
/// description = "Gather data"
/// ```
pub fn parse_plan_toml(toml_content: &str) -> Result<Plan> {
    let plan_file: PlanFile =
        toml::from_str(toml_content).map_err(|e| anyhow::anyhow!("TOML plan parse error: {e}"))?;
    Ok(plan_file.plan)
}

/// Look for a `.plan.toml` file alongside a `.md` context file.
///
/// Given a list of context file paths and a workspace directory, finds
/// the first `.md` file that has a sibling `.plan.toml` file. Returns
/// the plan file path if found.
pub fn find_plan_file(
    context_files: &[String],
    workspace_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    for path_str in context_files {
        let path = std::path::Path::new(path_str);
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        // Build sibling .plan.toml path
        let stem = path.file_stem()?;
        let plan_filename = format!("{}.plan.toml", stem.to_string_lossy());
        let plan_path = if path.is_absolute() {
            path.with_file_name(&plan_filename)
        } else {
            workspace_dir.join(path).with_file_name(&plan_filename)
        };

        if plan_path.exists() {
            return Some(plan_path);
        }
    }
    None
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

    #[test]
    fn plan_action_critical_defaults_to_false() {
        let json = r#"{"type": "read", "description": "read data"}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert!(!action.critical);
    }

    #[test]
    fn plan_action_critical_true_deserializes() {
        let json = r#"{"type": "read", "description": "read data", "critical": true}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert!(action.critical);
    }

    #[test]
    fn plan_action_max_iterations_defaults_to_none() {
        let json = r#"{"type": "read", "description": "read data"}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert!(action.max_iterations.is_none());
    }

    #[test]
    fn plan_action_max_iterations_deserializes() {
        let json = r#"{"type": "write", "description": "write data", "max_iterations": 40}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert_eq!(action.max_iterations, Some(40));
    }

    #[test]
    fn plan_require_synthesis_defaults_to_none() {
        let json = r#"{"passthrough": false, "actions": [{"type": "a", "description": "b"}]}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert!(plan.require_synthesis.is_none());
    }

    #[test]
    fn plan_require_synthesis_false_deserializes() {
        let json =
            r#"{"require_synthesis": false, "actions": [{"type": "a", "description": "b"}]}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.require_synthesis, Some(false));
    }

    #[test]
    fn plan_require_synthesis_true_deserializes() {
        let json = r#"{"require_synthesis": true, "actions": [{"type": "a", "description": "b"}]}"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.require_synthesis, Some(true));
    }

    // ── TOML plan file deserialization tests ──

    #[test]
    fn toml_plan_file_deserializes_full_fixture() {
        let toml_content = r#"
[plan]
require_synthesis = true

[[plan.actions]]
group = 1
description = "Gather Linear cycle status"
prompt = "Query Linear for current active cycle. List all issues with status and assignee."
action_type = "delegate"
tools = ["linear_search", "linear_get_issue"]
model_hint = "fast"
max_iterations = 5

[[plan.actions]]
group = 1
description = "Gather Slack updates"
prompt = "Read recent messages from engineering channel. Summarize key updates."
action_type = "delegate"
tools = ["read_slack_channel"]
model_hint = "fast"
max_iterations = 3

[[plan.actions]]
group = 2
description = "Write standup report"
action_type = "synthesize"
model_hint = "default"
max_iterations = 3
"#;

        let plan = parse_plan_toml(toml_content).unwrap();
        assert_eq!(plan.require_synthesis, Some(true));
        assert_eq!(plan.actions.len(), 3);

        // First action
        assert_eq!(plan.actions[0].group, 1);
        assert_eq!(plan.actions[0].action_type, "delegate");
        assert_eq!(plan.actions[0].description, "Gather Linear cycle status");
        assert_eq!(
            plan.actions[0].prompt.as_deref(),
            Some(
                "Query Linear for current active cycle. List all issues with status and assignee."
            )
        );
        assert_eq!(
            plan.actions[0].tools,
            vec!["linear_search", "linear_get_issue"]
        );
        assert_eq!(plan.actions[0].model_hint.as_deref(), Some("fast"));
        assert_eq!(plan.actions[0].max_iterations, Some(5));

        // Second action
        assert_eq!(plan.actions[1].group, 1);
        assert_eq!(plan.actions[1].action_type, "delegate");
        assert_eq!(plan.actions[1].tools, vec!["read_slack_channel"]);
        assert_eq!(plan.actions[1].max_iterations, Some(3));

        // Third action (group 2, no prompt)
        assert_eq!(plan.actions[2].group, 2);
        assert_eq!(plan.actions[2].action_type, "synthesize");
        assert!(plan.actions[2].prompt.is_none());
        assert_eq!(plan.actions[2].model_hint.as_deref(), Some("default"));
    }

    #[test]
    fn toml_plan_file_action_type_field_works() {
        // TOML uses action_type (alias), JSON uses type (primary).
        // Verify both work.
        let toml_content = r#"
[plan]
[[plan.actions]]
action_type = "delegate"
description = "test action"
"#;
        let plan = parse_plan_toml(toml_content).unwrap();
        assert_eq!(plan.actions[0].action_type, "delegate");
    }

    #[test]
    fn toml_plan_file_minimal_action() {
        let toml_content = r#"
[plan]
[[plan.actions]]
action_type = "read"
description = "read data"
"#;
        let plan = parse_plan_toml(toml_content).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].group, 1); // default group
        assert!(plan.actions[0].tools.is_empty());
        assert!(plan.actions[0].model_hint.is_none());
        assert!(plan.actions[0].max_iterations.is_none());
        assert!(!plan.actions[0].critical);
    }

    #[test]
    fn toml_plan_file_with_critical_action() {
        let toml_content = r#"
[plan]
[[plan.actions]]
action_type = "fetch"
description = "fetch critical data"
critical = true
"#;
        let plan = parse_plan_toml(toml_content).unwrap();
        assert!(plan.actions[0].critical);
    }

    #[test]
    fn toml_plan_file_empty_actions_is_passthrough() {
        let toml_content = r#"
[plan]
"#;
        let plan = parse_plan_toml(toml_content).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn toml_plan_file_invalid_toml_fails() {
        let result = parse_plan_toml("this is not valid toml {{{");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("TOML plan parse error"));
    }

    #[test]
    fn toml_plan_grouped_actions_work() {
        let toml_content = r#"
[plan]
[[plan.actions]]
group = 2
action_type = "write"
description = "write output"

[[plan.actions]]
group = 1
action_type = "read"
description = "read input"

[[plan.actions]]
group = 1
action_type = "fetch"
description = "fetch data"
"#;
        let plan = parse_plan_toml(toml_content).unwrap();
        let groups = plan.grouped_actions();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2); // group 1
        assert_eq!(groups[1].len(), 1); // group 2
    }

    // ── Template variable resolution tests ──

    #[test]
    fn resolve_template_replaces_date() {
        let template = "Report for {{date}}";
        let resolved = resolve_plan_template(template, "standup");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert_eq!(resolved, format!("Report for {today}"));
    }

    #[test]
    fn resolve_template_replaces_job_name() {
        let template = "Job: {{job_name}}";
        let resolved = resolve_plan_template(template, "morning-standup");
        assert_eq!(resolved, "Job: morning-standup");
    }

    #[test]
    fn resolve_template_replaces_both_variables() {
        let template = "Running {{job_name}} on {{date}}";
        let resolved = resolve_plan_template(template, "standup");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert_eq!(resolved, format!("Running standup on {today}"));
    }

    #[test]
    fn resolve_template_no_variables_unchanged() {
        let template = "No variables here";
        let resolved = resolve_plan_template(template, "test");
        assert_eq!(resolved, "No variables here");
    }

    #[test]
    fn resolve_template_multiple_occurrences() {
        let template = "{{date}} and {{date}} again";
        let resolved = resolve_plan_template(template, "test");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert_eq!(resolved, format!("{today} and {today} again"));
    }

    #[test]
    fn resolve_template_in_toml_context() {
        let template = r#"
[plan]
require_synthesis = true

[[plan.actions]]
action_type = "delegate"
description = "Report for {{date}}"
prompt = "Generate {{job_name}} report for {{date}}."
"#;
        let resolved = resolve_plan_template(template, "standup");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let plan = parse_plan_toml(&resolved).unwrap();
        assert_eq!(plan.actions[0].description, format!("Report for {today}"));
        assert_eq!(
            plan.actions[0].prompt.as_deref(),
            Some(format!("Generate standup report for {today}.").as_str())
        );
    }

    // ── executor_instruction tests ──

    #[test]
    fn executor_instruction_returns_prompt_when_set() {
        let action = PlanAction {
            group: 1,
            action_type: "delegate".into(),
            description: "Short label".into(),
            prompt: Some("Detailed instruction for the executor.".into()),
            tools: vec![],
            params: serde_json::Value::Null,
            model_hint: None,
            critical: false,
            max_iterations: None,
        };
        assert_eq!(
            action.executor_instruction(),
            "Detailed instruction for the executor."
        );
    }

    #[test]
    fn executor_instruction_falls_back_to_description() {
        let action = PlanAction {
            group: 1,
            action_type: "read".into(),
            description: "Read and summarize data".into(),
            prompt: None,
            tools: vec![],
            params: serde_json::Value::Null,
            model_hint: None,
            critical: false,
            max_iterations: None,
        };
        assert_eq!(action.executor_instruction(), "Read and summarize data");
    }

    // ── find_plan_file tests ──

    #[test]
    fn find_plan_file_returns_none_when_no_md_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = find_plan_file(&["data.json".into()], tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn find_plan_file_returns_none_when_no_sibling_plan() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("standup.md"), "instructions").unwrap();

        let result = find_plan_file(&["standup.md".into()], tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn find_plan_file_finds_sibling_plan_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("standup.md"), "instructions").unwrap();
        std::fs::write(
            tmp.path().join("standup.plan.toml"),
            "[plan]\n[[plan.actions]]\naction_type = \"read\"\ndescription = \"test\"\n",
        )
        .unwrap();

        let result = find_plan_file(&["standup.md".into()], tmp.path());
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.ends_with("standup.plan.toml"));
    }

    #[test]
    fn find_plan_file_works_with_subdirectory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("ritual.md"), "instructions").unwrap();
        std::fs::write(
            skills_dir.join("ritual.plan.toml"),
            "[plan]\n[[plan.actions]]\naction_type = \"read\"\ndescription = \"test\"\n",
        )
        .unwrap();

        let result = find_plan_file(&["skills/ritual.md".into()], tmp.path());
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.ends_with("ritual.plan.toml"));
    }

    #[test]
    fn find_plan_file_uses_first_matching_md() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.md"), "a").unwrap();
        std::fs::write(tmp.path().join("b.md"), "b").unwrap();
        std::fs::write(
            tmp.path().join("b.plan.toml"),
            "[plan]\n[[plan.actions]]\naction_type = \"read\"\ndescription = \"test\"\n",
        )
        .unwrap();

        // a.md has no plan, b.md has a plan
        let result = find_plan_file(&["a.md".into(), "b.md".into()], tmp.path());
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.ends_with("b.plan.toml"));
    }

    #[test]
    fn find_plan_file_skips_non_md_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("data.txt"), "data").unwrap();
        std::fs::write(
            tmp.path().join("data.plan.toml"),
            "[plan]\n[[plan.actions]]\naction_type = \"read\"\ndescription = \"test\"\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("ritual.md"), "instructions").unwrap();

        let result = find_plan_file(&["data.txt".into(), "ritual.md".into()], tmp.path());
        // data.txt is skipped (not .md), ritual.md has no sibling plan
        assert!(result.is_none());
    }

    // ── JSON prompt field backward compatibility ──

    #[test]
    fn json_plan_action_prompt_defaults_to_none() {
        let json = r#"{"type": "read", "description": "read data"}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert!(action.prompt.is_none());
    }

    #[test]
    fn json_plan_action_prompt_deserializes() {
        let json =
            r#"{"type": "read", "description": "Short label", "prompt": "Detailed instruction"}"#;
        let action: PlanAction = serde_json::from_str(json).unwrap();
        assert_eq!(action.prompt.as_deref(), Some("Detailed instruction"));
    }
}
