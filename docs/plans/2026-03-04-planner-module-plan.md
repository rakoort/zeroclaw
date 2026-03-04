# Planner Module Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extract the planner into a first-class `src/planner/` module implementing Anthropic's orchestrator-worker pattern with three-phase execution (plan → execute → synthesize), wire all execution paths through it, and delete the old conditional gating code.

**Architecture:** A new `src/planner/` module owns planning types, prompt construction, the three-phase orchestration loop, and a `PlannerRuntime` struct that encapsulates provider/tools/observer construction. The `Agent` struct wraps `PlannerRuntime`. The cron scheduler calls `PlannerRuntime` directly. The channel orchestrator calls `PlannerRuntime` directly. The old `src/agent/planner.rs`, three-zone agentic-score gate, `PlanningConfig`, and `score_agentic_task()` keyword classifier are deleted.

**Tech Stack:** Rust, tokio, serde, futures-util, anyhow

**Design doc:** `docs/plans/2026-03-04-planner-module-design.md`

---

### Task 1: Create `src/planner/types.rs` — data types

**Files:**
- Create: `src/planner/types.rs`
- Create: `src/planner/mod.rs`
- Modify: `src/lib.rs` — add `pub mod planner;`

**Step 1: Write the failing test**

Create `src/planner/types.rs` with the types and a test module. The types are migrated from `src/agent/planner.rs:12-116` with the addition of `analysis: Option<String>` on `Plan` and `model_hint: Option<String>` on `PlanAction`.

```rust
// src/planner/types.rs

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
        assert_eq!(result.to_accumulated_line(), r#"Action "read" (group 1): Read 10 messages"#);
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
```

**Step 2: Create `src/planner/mod.rs`**

```rust
// src/planner/mod.rs
pub mod types;

pub use types::{ActionResult, Plan, PlanAction, PlanExecutionResult};
```

**Step 3: Add module to `src/lib.rs`**

Add `pub mod planner;` after the existing module declarations in `src/lib.rs`.

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib planner::types`
Expected: all 8 tests pass.

**Step 5: Commit**

```
feat(planner): add planner types module with Plan, PlanAction, ActionResult
```

---

### Task 2: Create `src/planner/parser.rs` — JSON extraction

**Files:**
- Create: `src/planner/parser.rs`
- Modify: `src/planner/mod.rs` — add `pub mod parser;`

**Step 1: Write parser with tests**

Migrate `parse_plan()` and `parse_plan_from_response()` from `src/agent/planner.rs:52-81`. These functions extract JSON from LLM responses that may contain markdown fences.

```rust
// src/planner/parser.rs

use anyhow::{bail, Result};
use super::types::Plan;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_json() {
        let json = r#"{"passthrough": true}"#;
        let plan = parse_plan(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_from_fenced_json() {
        let response = "Here is the plan:\n```json\n{\"passthrough\": false, \"actions\": [{\"type\": \"read\", \"description\": \"read data\"}]}\n```";
        let plan = parse_plan_from_response(response).unwrap();
        assert!(!plan.is_passthrough());
        assert_eq!(plan.actions.len(), 1);
    }

    #[test]
    fn parse_from_bare_fences() {
        let response = "```\n{\"passthrough\": true}\n```";
        let plan = parse_plan_from_response(response).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_with_analysis_field() {
        let json = r#"{"analysis": "multi-step task", "passthrough": false, "actions": [{"type": "a", "description": "do a"}]}"#;
        let plan = parse_plan(json).unwrap();
        assert_eq!(plan.analysis.as_deref(), Some("multi-step task"));
    }

    #[test]
    fn parse_invalid_json_fails() {
        let result = parse_plan_from_response("not json at all");
        assert!(result.is_err());
    }
}
```

**Step 2: Update `src/planner/mod.rs`**

Add `pub mod parser;` and re-export: `pub use parser::{parse_plan, parse_plan_from_response};`

**Step 3: Run tests**

Run: `cargo test -p zeroclaw --lib planner::parser`
Expected: all 5 tests pass.

**Step 4: Commit**

```
feat(planner): add plan JSON parser with markdown fence extraction
```

---

### Task 3: Create `src/planner/prompts.rs` — prompt construction

**Files:**
- Create: `src/planner/prompts.rs`
- Modify: `src/planner/mod.rs` — add `pub mod prompts;`

**Step 1: Write prompts module with tests**

Migrate `build_executor_prompt()` from `src/agent/planner.rs:122-152` and add the new planner system prompt and synthesis prompt. Add `plan_analysis` parameter to executor prompt. Add effort scaling heuristics to planner prompt per Anthropic guidelines.

```rust
// src/planner/prompts.rs

use std::fmt::Write;
use super::types::PlanAction;

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
```

**Step 2: Update `src/planner/mod.rs`**

Add `pub mod prompts;` and re-exports.

**Step 3: Run tests**

Run: `cargo test -p zeroclaw --lib planner::prompts`
Expected: all 8 tests pass.

**Step 4: Commit**

```
feat(planner): add prompt builders for planner, executor, and synthesis phases
```

---

### Task 4: Create `src/planner/orchestrator.rs` — three-phase execution

**Files:**
- Create: `src/planner/orchestrator.rs`
- Modify: `src/planner/mod.rs` — add `pub mod orchestrator;`

**Step 1: Write the orchestrator**

This is the core of the refactor. Migrate the execution logic from `src/agent/planner.rs:176-528` into the new three-phase structure. Key changes from the old code:

1. Use `build_planner_system_prompt()` from prompts.rs (adds effort scaling)
2. Pass `plan_analysis` to `build_executor_prompt()`
3. Add Phase 3: synthesis LLM call after execution
4. Resolve `model_hint` per action against `model_routes`
5. Add `analysis` field to `PlanExecutionResult::Executed`

The function signature stays similar to the old `plan_then_execute()` but adds `model_routes: &HashMap<String, String>` for per-action model resolution.

```rust
// src/planner/orchestrator.rs

use anyhow::Result;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::observability::{runtime_trace, Observer, ObserverEvent};
use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::tools::{Tool, ToolSpec};

use super::parser::parse_plan_from_response;
use super::prompts::{build_executor_prompt, build_planner_system_prompt, build_synthesis_prompt, filter_tool_names};
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
                None, None,
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
            None, None, None,
            serde_json::json!({ "passthrough": true, "reason": if plan.passthrough { "passthrough_flag" } else { "empty_actions" } }),
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
        None, None, None,
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
                let executor_system = build_executor_prompt(action, &group_accumulated, plan_analysis);
                let wanted_tools = filter_tool_names(&all_tool_names, &action.tools);

                // Resolve executor model for this action
                let action_model = action
                    .model_hint
                    .as_ref()
                    .and_then(|hint| model_routes.get(hint))
                    .map(String::as_str)
                    .unwrap_or(executor_model);

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
                        Some(action_model),
                        None, None, None,
                        serde_json::json!({
                            "action_index": action_index,
                            "action_type": &action_type,
                            "group": action_group,
                            "description": &action_desc,
                            "model_hint": action.model_hint,
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
                        action_model,
                        temperature,
                        true,  // silent
                        None,  // no approval manager
                        channel_name,
                        &crate::config::MultimodalConfig::default(),
                        budget,
                        ct,
                        None,  // no delta sender
                        hooks,
                        &combined_excluded,
                        None,  // route_hint: executor uses resolved model directly
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
                        Some(action_model),
                        None,
                        Some(action_result.success),
                        if action_result.success { None } else { Some(&action_result.summary) },
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
        let synthesis_system = build_synthesis_prompt(
            user_message,
            plan_analysis,
            &accumulated,
        );

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
```

Note: `crate::agent::loop_::run_tool_call_loop` is `pub(crate)` so it's accessible from `src/planner/` within the same crate. No visibility change needed.

**Step 2: Update `src/planner/mod.rs`**

```rust
pub mod orchestrator;
pub mod parser;
pub mod prompts;
pub mod types;

pub use orchestrator::plan_then_execute;
pub use parser::{parse_plan, parse_plan_from_response};
pub use prompts::{build_executor_prompt, build_planner_system_prompt, build_synthesis_prompt, filter_tool_names};
pub use types::{ActionResult, Plan, PlanAction, PlanExecutionResult};
```

**Step 3: Run compilation check**

Run: `cargo check`
Expected: compiles (the orchestrator references `crate::agent::loop_::run_tool_call_loop` which exists).

**Step 4: Commit**

```
feat(planner): add three-phase orchestrator (plan, execute, synthesize)
```

---

### Task 5: Create `src/planner/runtime.rs` — PlannerRuntime

**Files:**
- Create: `src/planner/runtime.rs`
- Modify: `src/planner/mod.rs` — add `pub mod runtime;`

**Step 1: Write PlannerRuntime**

Extract the provider, tools, observer, and model construction from `Agent::from_config()` (agent.rs:289-396) into a reusable struct. This is the critical extraction — both `Agent` and the cron scheduler will use this.

```rust
// src/planner/runtime.rs

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
        let rt: Arc<dyn RuntimeAdapter> =
            Arc::from(runtime::create_runtime(&config.runtime)?);
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
```

**Step 2: Update `src/planner/mod.rs`**

Add `pub mod runtime;` and `pub use runtime::PlannerRuntime;`.

**Step 3: Run compilation check**

Run: `cargo check`
Expected: compiles.

**Step 4: Commit**

```
feat(planner): add PlannerRuntime with from_config() extraction
```

---

### Task 6: Rewire cron scheduler to use PlannerRuntime

**Files:**
- Modify: `src/cron/scheduler.rs:138-194` — rewrite `run_agent_job()`

**Step 1: Rewrite `run_agent_job()`**

Replace the call to `crate::agent::run()` with `PlannerRuntime::from_config()` + `plan_then_execute()`. The security checks at lines 143-162 stay unchanged. Delete the `agent::run()` call and its surrounding match.

In `src/cron/scheduler.rs`, replace lines 138-194 with:

```rust
async fn run_agent_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }

    let name = job.name.clone().unwrap_or_else(|| "cron-job".to_string());
    let prompt = job.prompt.clone().unwrap_or_default();
    let prefixed_prompt = format!("[cron:{} {name}] {prompt}", job.id);

    let runtime = match crate::planner::PlannerRuntime::from_config(config) {
        Ok(rt) => rt,
        Err(e) => return (false, format!("failed to build planner runtime: {e}")),
    };

    let executor_model = job
        .model
        .clone()
        .or_else(|| config.cron.model.clone())
        .unwrap_or_else(|| runtime.executor_model.clone());

    let planner_model = runtime
        .planner_model
        .as_deref()
        .unwrap_or(&executor_model);

    let result = crate::planner::plan_then_execute(
        runtime.provider.as_ref(),
        planner_model,
        &executor_model,
        "",  // system prompt — the ritual prompt IS the user message
        &prefixed_prompt,
        "",  // memory context
        &runtime.tools,
        &runtime.tool_specs,
        runtime.observer.as_ref(),
        "cron",
        runtime.temperature,
        runtime.max_tool_iterations,
        runtime.max_executor_iterations,
        "cron",
        None,  // no cancellation token
        None,  // no hooks
        &[],   // no excluded tools
        &runtime.model_routes,
    )
    .await;

    match result {
        Ok(crate::planner::PlanExecutionResult::Executed { output, .. }) => (
            true,
            if output.trim().is_empty() {
                "agent job executed".to_string()
            } else {
                output
            },
        ),
        Ok(crate::planner::PlanExecutionResult::Passthrough) => {
            // Passthrough: fall back to flat agent::run for simple tasks
            match crate::agent::loop_::run(
                config.clone(),
                Some(prefixed_prompt),
                None,
                Some(executor_model),
                runtime.temperature,
            )
            .await
            {
                Ok(()) => (true, "agent job executed".to_string()),
                Err(e) => (false, format!("agent job failed: {e}")),
            }
        }
        Err(e) => {
            tracing::warn!("Planner failed for cron job: {e}, falling back to flat run");
            match crate::agent::loop_::run(
                config.clone(),
                Some(prefixed_prompt),
                None,
                Some(executor_model),
                runtime.temperature,
            )
            .await
            {
                Ok(()) => (true, "agent job executed".to_string()),
                Err(e2) => (false, format!("agent job failed: {e2}")),
            }
        }
    }
}
```

Note: The `crate::agent::loop_::run` function at `loop_.rs:2753` is the existing flat-loop entry point. We keep it as the passthrough/error fallback for now. This will be cleaned up when Agent is refactored in Task 8.

**Step 2: Update imports at top of scheduler.rs**

Remove any unused imports. The file should no longer import from `crate::agent` for the agent run path (it now uses `crate::planner` and `crate::agent::loop_::run` for fallback).

**Step 3: Run tests**

Run: `cargo test -p zeroclaw --lib cron::scheduler`
Expected: existing scheduler tests pass. The `run_agent_job` tests (readonly, rate-limited, model resolution) should still pass since security checks are unchanged.

**Step 4: Commit**

```
feat(cron): wire run_agent_job through PlannerRuntime
```

---

### Task 7: Rewire channel orchestrator to use `crate::planner`

**Files:**
- Modify: `src/channels/orchestrator.rs:1454-1514` — change planner call
- Modify: `src/channels/orchestrator.rs:2837-2842` — delete `resolve_planner_model()`
- Modify: `src/channels/types.rs:153` — keep `planner_model` field (still needed)

**Step 1: Update planner call in orchestrator**

Replace `crate::agent::planner::plan_then_execute` with `crate::planner::plan_then_execute` at lines 1464-1513. Add `model_routes` parameter. The `resolve_planner_model()` function at line 2837 stays for now (it resolves the planner model from routes for the context struct) but update its return to use the same pattern.

Change the import from `crate::agent::planner::*` to `crate::planner::*`.

At lines 1464-1513, the call changes to add one parameter:

```rust
match crate::planner::plan_then_execute(
    active_provider.as_ref(),
    planner_model,
    route.model.as_str(),
    &system_prompt_str,
    &enriched_content,
    "",
    ctx.tools_registry.as_ref(),
    &tool_specs,
    ctx.observer.as_ref(),
    route.provider.as_str(),
    runtime_defaults.temperature,
    ctx.max_tool_iterations,
    ctx.max_executor_action_iterations,
    msg.channel.as_str(),
    Some(cancellation_token.clone()),
    ctx.hooks.as_deref(),
    &channel_excluded_tools,
    &ctx.model_routes,  // NEW parameter
)
```

Also update the match arms — `PlanExecutionResult::Executed` now has an `analysis` field:

```rust
Ok(crate::planner::PlanExecutionResult::Executed {
    output,
    action_results,
    analysis: _,
}) => { ... }
```

**Step 2: Add `model_routes` to `ChannelRuntimeContext`**

In `src/channels/types.rs`, add to the struct:

```rust
pub(crate) model_routes: HashMap<String, String>,
```

Wire it in the orchestrator's context construction (where `planner_model` is set around line 2819).

**Step 3: Run tests**

Run: `cargo test -p zeroclaw --lib channels::orchestrator`
Expected: `resolve_planner_model` tests pass. Channel message processing compiles.

**Step 4: Commit**

```
refactor(channels): use crate::planner instead of crate::agent::planner
```

---

### Task 8: Rewire Agent to use `crate::planner` and delete three-zone gate

**Files:**
- Modify: `src/agent/agent.rs:593-666` — delete three-zone gate, call `crate::planner::plan_then_execute`
- Modify: `src/agent/agent.rs:289-396` — refactor `from_config()` to use `PlannerRuntime`
- Modify: `src/agent/agent.rs:19-50` — remove `last_agentic_score` and planner gate fields

**Step 1: Delete the three-zone planner gate**

In `agent.rs`, replace lines 593-666 (the `has_planner_route && self.last_agentic_score >= ...` block) with a direct planner call:

```rust
// In turn(), after classify_model():

let planner_model = self
    .route_model_by_hint
    .get("planner")
    .cloned()
    .unwrap_or_else(|| effective_model.clone());

let system_prompt = self.build_system_prompt()?;

let plan_result = crate::planner::plan_then_execute(
    self.provider.as_ref(),
    &planner_model,
    &effective_model,
    &system_prompt,
    user_message,
    &context,
    &self.tools,
    &self.tool_specs,
    self.observer.as_ref(),
    "router",
    self.temperature,
    self.config.max_tool_iterations,
    self.config.max_executor_action_iterations,
    "cli",
    None,
    None,
    &excluded_integration_tools,
    &self.route_model_by_hint,
)
.await;

match plan_result {
    Ok(crate::planner::PlanExecutionResult::Passthrough) => {
        // Fall through to existing flat tool loop below
    }
    Ok(crate::planner::PlanExecutionResult::Executed {
        output,
        action_results: _,
        analysis: _,
    }) => {
        self.history
            .push(ConversationMessage::Chat(ChatMessage::assistant(output.clone())));
        self.trim_history();
        return Ok(super::sanitize::sanitize_model_response(&output));
    }
    Err(e) => {
        tracing::warn!("Planner failed ({e}), falling back to flat agent loop");
        // Fall through to existing flat tool loop below
    }
}
```

**Step 2: Remove unused fields from Agent struct**

Delete from the Agent struct (lines 19-50):
- `last_agentic_score: f64` (line 41)
- `last_confidence: f64` (line 43)

These were only used by the three-zone gate. The `classification_config`, `available_hints`, `route_model_by_hint`, `last_integrations`, `integration_catalog`, `integration_tool_names` fields stay — they're still used for model routing and integration filtering.

Remove the corresponding builder fields and initialization code:
- `AgentBuilder` field and method for `last_agentic_score`
- `Agent::from_config()` initialization at line 250-251

**Step 3: Update `classify_model()` (agent.rs:493-556)**

The function still runs for model routing and integration selection, but remove the `self.last_agentic_score = decision.agentic_score;` assignments at lines 506 and 521. The agentic_score is no longer consumed by anything.

**Step 4: Delete planner gate tests**

Delete tests that test the three-zone gate logic:
- `plan_then_execute_should_activate_planner` (around line 1536)
- Any test checking `last_agentic_score` threshold behavior for planner gating

Keep tests that verify classification itself (model routing, integration selection).

**Step 5: Run tests**

Run: `cargo test -p zeroclaw --lib agent`
Expected: remaining tests pass. Planner gate tests are gone.

**Step 6: Commit**

```
refactor(agent): delete three-zone planner gate, route all turns through planner
```

---

### Task 9: Delete `src/agent/planner.rs` and `PlanningConfig`

**Files:**
- Delete: `src/agent/planner.rs`
- Modify: `src/agent/mod.rs:7` — remove `pub mod planner;`
- Modify: `src/config/provider.rs:564-589` — delete `PlanningConfig` struct
- Modify: `src/config/provider.rs:615` — remove `planning` field from `QueryClassificationConfig`
- Modify: `src/config/schema.rs` — remove `PlanningConfig` test and default
- Modify: `src/agent/classifier.rs` — remove `score_agentic_task()` function

**Step 1: Delete `src/agent/planner.rs`**

Remove the entire file. All its functionality now lives in `src/planner/`.

**Step 2: Remove `pub mod planner;` from `src/agent/mod.rs`**

Line 7: delete `pub mod planner;`

**Step 3: Delete `PlanningConfig` from `src/config/provider.rs`**

Delete lines 564-589 (the `PlanningConfig` struct, its defaults, its `Default` impl).

Remove the `planning: PlanningConfig` field from `QueryClassificationConfig` at line 615.

**Step 4: Delete `score_agentic_task()` from classifier.rs**

Delete `score_agentic_task()` at lines 428-482. Remove its call site at line 528 in `score_v2()`. The `agentic_score` field on the scoring result struct can stay for now (the LLM classifier still produces it) but it's no longer used for planner gating.

**Step 5: Clean up config tests**

In `src/config/schema.rs`, delete the `PlanningConfig` test at lines 4446-4448. Remove any `PlanningConfig::default()` references in test fixtures.

In `src/agent/classifier.rs`, remove tests that assert on `agentic_score` for planning threshold purposes.

**Step 6: Search for any remaining references**

Run: `cargo check`
Fix any remaining compile errors from deleted types/functions.

**Step 7: Run full test suite**

Run: `cargo test`
Expected: all tests pass.

**Step 8: Commit**

```
refactor: delete src/agent/planner.rs, PlanningConfig, score_agentic_task
```

---

### Task 10: Verify and clean up

**Files:**
- All modified files from Tasks 1-9

**Step 1: Run full validation**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Fix any formatting, lint, or test issues.

**Step 2: Verify no references to old planner remain**

Search for any remaining references:
- `agent::planner` — should be zero occurrences
- `PlanningConfig` — should be zero occurrences
- `skip_threshold` — should be zero occurrences (in context of planning)
- `activate_threshold` — should be zero occurrences
- `score_agentic_task` — should be zero occurrences
- `last_agentic_score` — should be zero occurrences (for planner gating)

**Step 3: Commit any cleanup**

```
chore: clean up lint and formatting after planner module refactor
```

---

## Summary of Deletions

| What | Where | Why |
|------|-------|-----|
| `src/agent/planner.rs` | entire file | Superseded by `src/planner/` |
| `pub mod planner;` | `src/agent/mod.rs:7` | Module moved |
| `PlanningConfig` struct | `src/config/provider.rs:564-589` | Thresholds no longer used |
| `planning: PlanningConfig` | `src/config/provider.rs:615` | Field removed from config |
| `score_agentic_task()` | `src/agent/classifier.rs:428-482` | Keyword classifier replaced by planner |
| Three-zone gate | `src/agent/agent.rs:593-666` | Replaced by direct planner call |
| `last_agentic_score` field | `src/agent/agent.rs:41` | No longer used for gating |
| `last_confidence` field | `src/agent/agent.rs:43` | No longer used |
| `PlanningConfig` tests | `src/config/schema.rs:4446-4448` | Struct deleted |
| Planner gate tests | `src/agent/agent.rs:~1536-1700` | Gate deleted |
| `resolve_planner_model` tests | `src/channels/orchestrator.rs:6388-6428` | Function may be simplified |

## New Files

| File | Purpose |
|------|---------|
| `src/planner/mod.rs` | Module root, re-exports |
| `src/planner/types.rs` | Plan, PlanAction, ActionResult, PlanExecutionResult |
| `src/planner/parser.rs` | JSON/fenced extraction |
| `src/planner/prompts.rs` | Planner, executor, synthesis prompt builders |
| `src/planner/orchestrator.rs` | Three-phase plan_then_execute |
| `src/planner/runtime.rs` | PlannerRuntime with from_config() |
