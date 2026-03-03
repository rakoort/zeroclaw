# Execution Mode Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add LLM-driven integration filtering to the classifier so each message only sees tools from relevant external integrations, and make both iteration budgets consumer-configurable.

**Architecture:** The weighted scorer runs first (unchanged), then an LLM classifier call produces tier + agentic_score + integration list. Tool registry filters integration tools before the planner gate. Both iteration caps move from constants to config fields.

**Tech Stack:** Rust, serde, tokio, existing Provider trait for LLM calls

**Design doc:** `docs/plans/2026-03-03-execution-mode-design.md`

---

### Task 1: Add `max_executor_action_iterations` to AgentConfig

**Files:**
- Modify: `src/config/integrations.rs:76-118` (AgentConfig struct + Default impl)
- Modify: `src/agent/planner.rs:14` (remove hardcoded constant)
- Modify: `src/agent/planner.rs:182-200` (plan_then_execute signature)
- Modify: `src/agent/agent.rs:556-574` (pass new config value to plan_then_execute)
- Test: `src/config/integrations.rs` (existing integration_config_tests module)
- Test: `src/agent/planner.rs` (existing planner tests)

**Step 1: Write the failing test**

Add to `src/config/integrations.rs` in the `integration_config_tests` module:

```rust
#[test]
fn agent_config_has_max_executor_action_iterations() {
    let config = AgentConfig::default();
    assert_eq!(config.max_executor_action_iterations, 15);
}

#[test]
fn agent_config_max_executor_action_iterations_deserializes() {
    let toml_str = r#"max_executor_action_iterations = 25"#;
    let config: AgentConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.max_executor_action_iterations, 25);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test agent_config_has_max_executor_action_iterations -- --nocapture`
Expected: FAIL — no field `max_executor_action_iterations` on `AgentConfig`

**Step 3: Add the field to AgentConfig**

In `src/config/integrations.rs`, add to `AgentConfig`:

```rust
/// Maximum tool-call iterations per executor action. Default: `15`.
#[serde(default = "default_agent_max_executor_action_iterations")]
pub max_executor_action_iterations: usize,
```

Add default function:

```rust
fn default_agent_max_executor_action_iterations() -> usize {
    15
}
```

Update `Default` impl to include the new field.

**Step 4: Run test to verify it passes**

Run: `cargo test agent_config_has_max_executor -- --nocapture`
Expected: PASS

**Step 5: Wire into planner**

In `src/agent/planner.rs`:
- Remove `const MAX_EXECUTOR_ACTION_ITERATIONS` and its compile-time asserts (lines 14-16).
- The `max_tool_iterations` parameter already exists on `plan_then_execute`. The caller in `agent.rs:568` already passes `self.config.max_tool_iterations`. Change the executor budget line (planner.rs:312) from `max_tool_iterations.min(MAX_EXECUTOR_ACTION_ITERATIONS)` to just use the dedicated parameter. Add a second parameter `max_executor_iterations: usize` to `plan_then_execute`.
- In `agent.rs`, pass `self.config.max_executor_action_iterations` as the new argument.

**Step 6: Update planner tests**

Update all `plan_then_execute` test calls to include the new parameter (value: `15`).

**Step 7: Run all tests**

Run: `cargo test`
Expected: PASS

**Step 8: Commit**

```
feat(config): make executor action iteration budget configurable

Moves MAX_EXECUTOR_ACTION_ITERATIONS from a hardcoded constant to
AgentConfig.max_executor_action_iterations (default: 15). Consumer
repos can now override iteration budgets per deployment.
```

---

### Task 2: Add `integrations` field to ClassificationDecision

**Files:**
- Modify: `src/agent/classifier.rs:676-701` (ClassificationDecision struct + Default impl)
- Test: `src/agent/classifier.rs` (existing tests module)

**Step 1: Write the failing test**

Add to classifier tests:

```rust
#[test]
fn classification_decision_has_integrations_field() {
    let decision = ClassificationDecision {
        integrations: vec!["linear".to_string()],
        ..Default::default()
    };
    assert_eq!(decision.integrations, vec!["linear"]);
}

#[test]
fn classification_decision_default_has_empty_integrations() {
    let decision = ClassificationDecision::default();
    assert!(decision.integrations.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test classification_decision_has_integrations -- --nocapture`
Expected: FAIL — no field `integrations`

**Step 3: Add the field**

In `src/agent/classifier.rs`, add to `ClassificationDecision`:

```rust
/// Integration names selected by the classifier (e.g. `["linear", "slack"]`).
/// Empty means no external integrations needed.
pub integrations: Vec<String>,
```

Add `integrations: Vec::new()` to the `Default` impl.

**Step 4: Run test to verify it passes**

Run: `cargo test classification_decision -- --nocapture`
Expected: PASS

**Step 5: Commit**

```
feat(classifier): add integrations field to ClassificationDecision

Prepares the classifier output to carry integration selection
alongside tier and agentic_score. Defaults to empty (no external
integrations). The LLM classifier call (next task) will populate it.
```

---

### Task 3: Build integration catalog summary for classifier prompt

**Files:**
- Modify: `src/integrations/mod.rs` (add `active_integration_summary` function)
- Test: `src/integrations/mod.rs` (existing tests module)

**Step 1: Write the failing test**

Add to `src/integrations/mod.rs` tests:

```rust
#[test]
fn active_integration_summary_returns_configured_integrations() {
    let mut config = crate::config::Config::default();
    config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
        api_key: "lin_api_test".into(),
    });
    let summary = active_integration_summary(&config);
    assert!(summary.contains("linear"));
    assert!(summary.contains("Issue tracking"));
}

#[test]
fn active_integration_summary_empty_when_none_configured() {
    let config = crate::config::Config::default();
    let summary = active_integration_summary(&config);
    assert!(summary.is_empty() || summary.contains("No external integrations"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test active_integration_summary -- --nocapture`
Expected: FAIL — function does not exist

**Step 3: Implement**

In `src/integrations/mod.rs`, add a public function:

```rust
/// Build a one-line-per-integration summary of active (configured) integrations
/// for use in the classifier prompt. Only includes integrations with runtime
/// tools (i.e., those returned by `collect_integrations`).
pub fn active_integration_summary(config: &Config) -> String {
    let integrations = collect_integrations(config);
    if integrations.is_empty() {
        return String::new();
    }
    let catalog = catalog_registry::all_integrations();
    let mut lines = Vec::new();
    for integration in &integrations {
        let name = integration.name();
        let description = catalog
            .iter()
            .find(|e| e.name.to_lowercase() == name.to_lowercase())
            .map(|e| e.description)
            .unwrap_or("External integration");
        lines.push(format!("- {name}: {description}"));
    }
    lines.join("\n")
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test active_integration_summary -- --nocapture`
Expected: PASS

**Step 5: Commit**

```
feat(integrations): add active_integration_summary for classifier prompt

Builds a concise catalog of configured integrations (name + description)
for injection into the LLM classifier prompt. Only includes integrations
that have runtime tools.
```

---

### Task 4: Add `classifier` model route support

**Files:**
- Modify: `src/agent/agent.rs` (classify_model method, ~line 449)
- Test: `src/agent/agent.rs` (existing tests)

**Step 1: Write the failing test**

Add to agent.rs tests:

```rust
#[tokio::test]
async fn classifier_model_route_is_used_when_configured() {
    use crate::config::schema::{ClassificationMode, ClassificationTiers};
    use crate::config::QueryClassificationConfig;

    let seen_models = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen_models.clone();

    // Build agent with classifier route
    let mut builder = Agent::builder();
    // ... (follow existing test patterns for building agents with model routes)
    // Add "classifier" to available_hints and route_model_by_hint
    // Verify that when classification runs, the classifier model route is available
    // This test validates the config wiring — the actual LLM call comes in Task 5
}
```

Note: This task is primarily config wiring. The actual LLM classifier call is Task 5. This task ensures the `classifier` hint is recognized and routable.

**Step 2: Verify the model route mechanism**

The existing `route_model_by_hint` HashMap already supports arbitrary hint keys. Adding `"classifier"` just requires the consumer config to include:

```toml
[[model_routes]]
hint = "classifier"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

No code change needed — the routing mechanism is generic. Verify with a test that checks the hint lookup works.

**Step 3: Commit**

```
test(agent): verify classifier model route is recognized

The existing model routing mechanism already supports arbitrary hint
keys. Consumer repos add a "classifier" model route to control which
model performs classification. No code change needed.
```

---

### Task 5: Implement LLM classifier call

This is the core task. The classifier calls an LLM after weighted scoring to produce tier + integrations.

**Files:**
- Modify: `src/agent/classifier.rs` (add `classify_with_llm` function)
- Modify: `src/agent/agent.rs` (call `classify_with_llm` from `classify_model`)
- Test: `src/agent/classifier.rs`

**Step 1: Write the failing test**

Add to classifier tests:

```rust
#[test]
fn parse_llm_classification_response_valid() {
    let json = r#"{"tier": "medium", "agentic_score": 0.4, "integrations": ["linear"], "reasoning": "user asking about tasks"}"#;
    let result = parse_llm_classification(json).unwrap();
    assert_eq!(result.tier, Tier::Medium);
    assert!((result.agentic_score - 0.4).abs() < 0.01);
    assert_eq!(result.integrations, vec!["linear"]);
}

#[test]
fn parse_llm_classification_response_empty_integrations() {
    let json = r#"{"tier": "simple", "agentic_score": 0.0, "integrations": [], "reasoning": "greeting"}"#;
    let result = parse_llm_classification(json).unwrap();
    assert_eq!(result.tier, Tier::Simple);
    assert!(result.integrations.is_empty());
}

#[test]
fn parse_llm_classification_response_invalid_json() {
    let result = parse_llm_classification("not json");
    assert!(result.is_err());
}

#[test]
fn build_classifier_prompt_includes_integrations() {
    let weighted_score = 0.05;
    let signals = vec!["simple_indicators".to_string()];
    let integration_catalog = "- linear: Issue tracking\n- slack: Workspace apps";
    let prompt = build_classifier_prompt(weighted_score, &signals, 3, integration_catalog);
    assert!(prompt.contains("linear"));
    assert!(prompt.contains("Issue tracking"));
    assert!(prompt.contains("0.05"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test parse_llm_classification -- --nocapture`
Expected: FAIL — functions don't exist

**Step 3: Implement parsing and prompt building**

In `src/agent/classifier.rs`, add:

```rust
use serde::Deserialize;

/// LLM classifier response (parsed from JSON).
#[derive(Debug, Clone, Deserialize)]
pub struct LlmClassificationResponse {
    pub tier: Tier,
    pub agentic_score: f64,
    #[serde(default)]
    pub integrations: Vec<String>,
    #[serde(default)]
    pub reasoning: String,
}

/// Parse the LLM classifier JSON response.
pub fn parse_llm_classification(json: &str) -> Result<LlmClassificationResponse, serde_json::Error> {
    serde_json::from_str(json.trim())
}

/// Build the system prompt for the LLM classifier call.
pub fn build_classifier_prompt(
    weighted_score: f64,
    signals: &[String],
    token_estimate: usize,
    integration_catalog: &str,
) -> String {
    let tier_suggestion = if weighted_score < 0.0 {
        "simple"
    } else if weighted_score < 0.3 {
        "medium"
    } else if weighted_score < 0.5 {
        "complex"
    } else {
        "reasoning"
    };

    let signals_str = if signals.is_empty() {
        "none".to_string()
    } else {
        signals.join(", ")
    };

    let integrations_block = if integration_catalog.is_empty() {
        "No external integrations configured.".to_string()
    } else {
        format!("Available integrations:\n{integration_catalog}")
    };

    format!(
        "You are a message classifier. Given a user message, a preliminary analysis, \
        and available integrations, output a JSON classification.\n\n\
        Preliminary analysis:\n\
        - Score: {weighted_score:.2} (leans {tier_suggestion})\n\
        - Signals: {signals_str}\n\
        - Token estimate: {token_estimate}\n\n\
        {integrations_block}\n\n\
        Rules:\n\
        - tier: simple, medium, complex, or reasoning\n\
        - agentic_score: 0.0 to 1.0 (how much multi-step tool use is needed)\n\
        - integrations: list of integration names the message needs (empty if none)\n\
        - reasoning: one sentence explaining your classification\n\n\
        Output ONLY valid JSON, no markdown fences:\n\
        {{\"tier\": \"...\", \"agentic_score\": 0.0, \"integrations\": [], \"reasoning\": \"...\"}}"
    )
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test parse_llm_classification -- --nocapture && cargo test build_classifier_prompt -- --nocapture`
Expected: PASS

**Step 5: Commit**

```
feat(classifier): add LLM classification response parsing and prompt builder

Adds parse_llm_classification() for deserializing the LLM classifier
JSON response, and build_classifier_prompt() for constructing the
classifier system prompt with weighted signals and integration catalog.
```

---

### Task 6: Wire LLM classifier call into Agent::classify_model

**Files:**
- Modify: `src/agent/agent.rs:449-470` (classify_model method)
- Modify: `src/agent/agent.rs` (Agent struct — needs provider access in classify_model)
- Test: `src/agent/agent.rs`

**Step 1: Understand the challenge**

`classify_model` is currently sync (`fn`), but making an LLM call requires `async`. The method needs to become async, or the LLM call needs to happen separately. Since `classify_model` is called from `turn()` which is already async, changing it to async is cleanest.

The LLM classifier call needs:
1. The weighted score + signals (from current scorer)
2. The integration catalog (from `active_integration_summary`)
3. A provider + classifier model to call

**Step 2: Write the test**

```rust
#[tokio::test]
async fn classify_model_with_llm_populates_integrations() {
    // Build agent with:
    // - weighted classification enabled
    // - classifier model route configured
    // - mock provider that returns classifier JSON
    // Verify ClassificationDecision.integrations is populated
}
```

**Step 3: Make classify_model async**

Change `fn classify_model` to `async fn classify_model`. Update the call site in `turn()` to `.await` it.

After weighted scoring produces a `ClassificationDecision`, if a `"classifier"` model route exists:
1. Build the classifier prompt using `build_classifier_prompt`
2. Call the provider with the classifier model
3. Parse the response with `parse_llm_classification`
4. Override the decision's tier, agentic_score, and integrations with the LLM response
5. If the LLM call fails, fall back to the weighted-only decision (log warning)

**Step 4: Implement**

In `classify_model`, after the existing weighted scoring block:

```rust
// If classifier model route exists, refine with LLM call
if self.route_model_by_hint.contains_key("classifier") {
    let classifier_model = self.route_model_by_hint["classifier"].clone();
    let catalog = crate::integrations::active_integration_summary(&self.root_config);
    let prompt = crate::agent::classifier::build_classifier_prompt(
        decision.confidence, // or raw weighted score
        &decision.signals,
        estimated_tokens,
        &catalog,
    );
    // Make LLM call, parse response, override decision fields
    // Fall back to weighted decision on error
}
```

**Step 5: Run tests**

Run: `cargo test`
Expected: PASS

**Step 6: Commit**

```
feat(classifier): wire LLM classifier call into classify_model

When a "classifier" model route is configured, classify_model now
makes an LLM call after weighted scoring. The LLM receives the
weighted signals and integration catalog, and outputs tier,
agentic_score, and integration selection. Falls back to weighted-only
classification if the LLM call fails.
```

---

### Task 7: Filter tool registry by selected integrations

**Files:**
- Modify: `src/agent/agent.rs` (after classify_model, before planner gate)
- Modify: `src/integrations/mod.rs` (add `filter_tools_by_integrations`)
- Test: `src/integrations/mod.rs`

**Step 1: Write the failing test**

Add to `src/integrations/mod.rs` tests:

```rust
#[test]
fn filter_tools_by_integrations_keeps_matching() {
    let mut config = crate::config::Config::default();
    config.integrations.slack = Some(crate::config::SlackIntegrationConfig {
        bot_token: "xoxb-test".into(),
        app_token: "xapp-test".into(),
        channel_id: None,
        allowed_users: vec![],
        mention_only: true,
        mention_regex: None,
        triage_model: None,
    });
    config.integrations.linear = Some(crate::config::LinearIntegrationConfig {
        api_key: "lin_api_test".into(),
    });

    let integrations = collect_integrations(&config);
    let selected = vec!["linear".to_string()];
    let filtered = filter_tools_by_integrations(&integrations, &selected);

    // Should have linear tools (14) but not slack tools (9)
    assert_eq!(filtered.len(), 14);
    for tool in &filtered {
        assert!(
            tool.spec().name.starts_with("linear_"),
            "Expected linear tool, got: {}",
            tool.spec().name
        );
    }
}

#[test]
fn filter_tools_by_integrations_empty_selection_returns_empty() {
    let config = crate::config::Config::default();
    let integrations = collect_integrations(&config);
    let filtered = filter_tools_by_integrations(&integrations, &[]);
    assert!(filtered.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test filter_tools_by_integrations -- --nocapture`
Expected: FAIL — function doesn't exist

**Step 3: Implement**

In `src/integrations/mod.rs`:

```rust
/// Filter integration tools to only those from the selected integrations.
/// Returns tools from integrations whose `name()` appears in `selected`.
pub fn filter_tools_by_integrations(
    integrations: &[Arc<dyn Integration>],
    selected: &[String],
) -> Vec<Arc<dyn Tool>> {
    integrations
        .iter()
        .filter(|i| selected.iter().any(|s| s.eq_ignore_ascii_case(i.name())))
        .flat_map(|i| i.tools())
        .collect()
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test filter_tools_by_integrations -- --nocapture`
Expected: PASS

**Step 5: Wire into Agent::turn**

In `agent.rs`, after `classify_model` and before the planner gate, use the `integrations` from `ClassificationDecision` to build a filtered excluded_tools list. The existing `excluded_tools` parameter on `plan_then_execute` and the tool loop can be leveraged: compute the set of integration tool names NOT in the selected integrations and add them to excluded_tools.

Alternatively, build a new tool set with only internal tools + selected integration tools, and replace `self.tools` / `self.tool_specs` for this turn.

The cleaner approach: build the excluded list from the classifier's integration selection and pass it through the existing `excluded_tools` mechanism.

**Step 6: Run all tests**

Run: `cargo test`
Expected: PASS

**Step 7: Commit**

```
feat(agent): filter tools by classifier-selected integrations

After classification, tools from unselected integrations are excluded
from both the planner and the tool loop. Internal tools remain always
available. Uses the existing excluded_tools mechanism.
```

---

### Task 8: Integration test — end-to-end classification with integration filtering

**Files:**
- Test: `src/agent/agent.rs` (new integration test)

**Step 1: Write integration test**

```rust
#[tokio::test]
async fn turn_filters_tools_by_classifier_integrations() {
    // Build agent with:
    // - Weighted classification enabled
    // - Classifier model route configured
    // - Mock provider: classifier returns {"tier":"simple","agentic_score":0.0,"integrations":[]}
    // - Linear integration configured (so linear tools exist)
    // - Second mock response for the actual agent turn
    //
    // Send "hey" -> classifier says no integrations
    // Verify that tool calls don't include linear tools
    // (check via observer events or mock tool invocations)
}

#[tokio::test]
async fn turn_includes_tools_for_selected_integrations() {
    // Same setup but classifier returns {"integrations":["linear"]}
    // Verify linear tools ARE available
}
```

**Step 2: Implement and run**

Run: `cargo test turn_filters_tools -- --nocapture`
Expected: PASS

**Step 3: Commit**

```
test(agent): end-to-end integration filtering via classifier

Verifies that classifier-selected integrations control which tools
the agent sees during a turn. Simple messages get no integration
tools; messages needing Linear get Linear tools.
```

---

### Task 9: Full validation

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

**Step 3: Run fmt**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues

**Step 4: Commit any fixups**

If clippy or fmt found issues, fix and commit.

---

## Task dependency graph

```
Task 1 (config: max_executor_action_iterations)
Task 2 (classifier: integrations field)
Task 3 (integrations: catalog summary)
  ↓
Task 4 (config: classifier model route) — can run parallel with 1-3
  ↓
Task 5 (classifier: LLM call parsing + prompt) — depends on 2, 3
  ↓
Task 6 (agent: wire LLM classifier) — depends on 4, 5
  ↓
Task 7 (integrations: filter tools) — depends on 2
  ↓
Task 8 (integration test) — depends on 6, 7
  ↓
Task 9 (full validation) — depends on all
```

Tasks 1, 2, 3, 4 are independent and can be done in parallel.
