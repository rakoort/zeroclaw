# Planner/Executor Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Upgrade the query classifier from 6 to 14 dimensions with agentic detection, add fallback chains with context filtering, then build a planner/executor split that routes agentic tasks through a reasoning model for planning and a fast model for execution.

**Architecture:** Three phases land independently. Phase 1 replaces the `WeightedScorer` in `classifier.rs` with a 14-dimension scorer plus sigmoid confidence and agentic score. Phase 2 extends `ModelRouteConfig` with fallback chains and context window filtering in `RouterProvider`. Phase 3 adds `planner.rs` above the existing agent loop — `plan_then_execute()` calls the planner model, parses the JSON plan, and executes actions one-by-one through `run_tool_call_loop()`.

**Tech Stack:** Rust, serde, tokio (for parallel group execution), existing Provider/Tool/Observer traits

**Design doc:** `docs/plans/2026-02-26-planner-executor-design.md`

---

## Phase 1: Classifier Upgrade (14 dimensions + agentic score + sigmoid confidence)

### Task 1: Extend config schema with new scoring types

**Files:**
- Modify: `src/config/schema.rs:2382-2448`
- Test: `src/config/schema.rs` (existing serde tests)

**Step 1: Write tests for the new config structs**

Add to the bottom of the existing test module in `schema.rs`:

```rust
#[test]
fn scoring_config_deserializes_with_defaults() {
    let config: ScoringConfig = toml::from_str("").unwrap();
    assert!((config.dimension_weights.token_count - 0.08).abs() < 0.001);
    assert!((config.confidence_steepness - 12.0).abs() < 0.001);
    assert!((config.confidence_threshold - 0.7).abs() < 0.001);
}

#[test]
fn tier_boundaries_deserialize_with_defaults() {
    let config: ScoringConfig = toml::from_str("").unwrap();
    assert!((config.tier_boundaries.simple_medium - 0.0).abs() < 0.001);
    assert!((config.tier_boundaries.medium_complex - 0.3).abs() < 0.001);
    assert!((config.tier_boundaries.complex_reasoning - 0.5).abs() < 0.001);
}

#[test]
fn overrides_config_deserializes_with_defaults() {
    let config: ScoringConfig = toml::from_str("").unwrap();
    assert_eq!(config.overrides.max_tokens_force_complex, 100_000);
    assert_eq!(config.overrides.ambiguous_default_tier, Tier::Medium);
}

#[test]
fn planning_config_deserializes_with_defaults() {
    let config: PlanningConfig = toml::from_str("").unwrap();
    assert!((config.skip_threshold - 0.3).abs() < 0.001);
    assert!((config.activate_threshold - 0.5).abs() < 0.001);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib scoring_config_deserializes -- --nocapture`
Expected: compilation error — `ScoringConfig` doesn't exist yet.

**Step 3: Add the new config types**

Add these structs after the existing `ClassificationWeights` block (after line ~2427) in `schema.rs`:

```rust
/// Tier classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Simple,
    Medium,
    Complex,
    Reasoning,
}

impl Default for Tier {
    fn default() -> Self {
        Tier::Medium
    }
}

/// Tier score boundaries for weighted classification.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TierBoundaries {
    #[serde(default = "default_simple_medium")]
    pub simple_medium: f64,
    #[serde(default = "default_medium_complex")]
    pub medium_complex: f64,
    #[serde(default = "default_complex_reasoning")]
    pub complex_reasoning: f64,
}

fn default_simple_medium() -> f64 { 0.0 }
fn default_medium_complex() -> f64 { 0.3 }
fn default_complex_reasoning() -> f64 { 0.5 }

impl Default for TierBoundaries {
    fn default() -> Self {
        Self {
            simple_medium: default_simple_medium(),
            medium_complex: default_medium_complex(),
            complex_reasoning: default_complex_reasoning(),
        }
    }
}

/// Scoring override rules applied before tier mapping.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoringOverrides {
    /// Estimated input tokens above this force COMPLEX tier.
    #[serde(default = "default_max_tokens_force_complex")]
    pub max_tokens_force_complex: usize,
    /// Minimum tier when structured output is detected.
    #[serde(default)]
    pub structured_output_min_tier: Tier,
    /// Default tier when confidence is below threshold (ambiguous).
    #[serde(default)]
    pub ambiguous_default_tier: Tier,
}

fn default_max_tokens_force_complex() -> usize { 100_000 }

impl Default for ScoringOverrides {
    fn default() -> Self {
        Self {
            max_tokens_force_complex: default_max_tokens_force_complex(),
            structured_output_min_tier: Tier::Medium,
            ambiguous_default_tier: Tier::Medium,
        }
    }
}

/// 14-dimension weights for the upgraded weighted scorer.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DimensionWeights {
    #[serde(default = "default_w_token_count")]
    pub token_count: f64,
    #[serde(default = "default_w_code_presence")]
    pub code_presence: f64,
    #[serde(default = "default_w_reasoning_markers")]
    pub reasoning_markers: f64,
    #[serde(default = "default_w_technical_terms")]
    pub technical_terms: f64,
    #[serde(default = "default_w_creative_markers")]
    pub creative_markers: f64,
    #[serde(default = "default_w_simple_indicators")]
    pub simple_indicators: f64,
    #[serde(default = "default_w_multi_step")]
    pub multi_step_patterns: f64,
    #[serde(default = "default_w_question_complexity")]
    pub question_complexity: f64,
    #[serde(default = "default_w_imperative_verbs")]
    pub imperative_verbs: f64,
    #[serde(default = "default_w_constraint_count")]
    pub constraint_count: f64,
    #[serde(default = "default_w_output_format")]
    pub output_format: f64,
    #[serde(default = "default_w_reference_complexity")]
    pub reference_complexity: f64,
    #[serde(default = "default_w_negation_complexity")]
    pub negation_complexity: f64,
    #[serde(default = "default_w_domain_specificity")]
    pub domain_specificity: f64,
    #[serde(default = "default_w_agentic_task")]
    pub agentic_task: f64,
}

fn default_w_token_count() -> f64 { 0.08 }
fn default_w_code_presence() -> f64 { 0.15 }
fn default_w_reasoning_markers() -> f64 { 0.18 }
fn default_w_technical_terms() -> f64 { 0.10 }
fn default_w_creative_markers() -> f64 { 0.05 }
fn default_w_simple_indicators() -> f64 { 0.02 }
fn default_w_multi_step() -> f64 { 0.12 }
fn default_w_question_complexity() -> f64 { 0.05 }
fn default_w_imperative_verbs() -> f64 { 0.03 }
fn default_w_constraint_count() -> f64 { 0.04 }
fn default_w_output_format() -> f64 { 0.03 }
fn default_w_reference_complexity() -> f64 { 0.02 }
fn default_w_negation_complexity() -> f64 { 0.01 }
fn default_w_domain_specificity() -> f64 { 0.02 }
fn default_w_agentic_task() -> f64 { 0.10 }

impl Default for DimensionWeights {
    fn default() -> Self {
        Self {
            token_count: default_w_token_count(),
            code_presence: default_w_code_presence(),
            reasoning_markers: default_w_reasoning_markers(),
            technical_terms: default_w_technical_terms(),
            creative_markers: default_w_creative_markers(),
            simple_indicators: default_w_simple_indicators(),
            multi_step_patterns: default_w_multi_step(),
            question_complexity: default_w_question_complexity(),
            imperative_verbs: default_w_imperative_verbs(),
            constraint_count: default_w_constraint_count(),
            output_format: default_w_output_format(),
            reference_complexity: default_w_reference_complexity(),
            negation_complexity: default_w_negation_complexity(),
            domain_specificity: default_w_domain_specificity(),
            agentic_task: default_w_agentic_task(),
        }
    }
}

/// Full scoring configuration for the 14-dimension classifier.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoringConfig {
    #[serde(default)]
    pub dimension_weights: DimensionWeights,
    #[serde(default)]
    pub tier_boundaries: TierBoundaries,
    #[serde(default)]
    pub overrides: ScoringOverrides,
    #[serde(default = "default_confidence_steepness")]
    pub confidence_steepness: f64,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
}

fn default_confidence_steepness() -> f64 { 12.0 }
fn default_confidence_threshold() -> f64 { 0.7 }

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            dimension_weights: DimensionWeights::default(),
            tier_boundaries: TierBoundaries::default(),
            overrides: ScoringOverrides::default(),
            confidence_steepness: default_confidence_steepness(),
            confidence_threshold: default_confidence_threshold(),
        }
    }
}

/// Planner activation thresholds based on agentic score.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanningConfig {
    /// Agentic score below this skips the planner.
    #[serde(default = "default_planning_skip")]
    pub skip_threshold: f64,
    /// Agentic score above this activates the planner.
    #[serde(default = "default_planning_activate")]
    pub activate_threshold: f64,
}

fn default_planning_skip() -> f64 { 0.3 }
fn default_planning_activate() -> f64 { 0.5 }

impl Default for PlanningConfig {
    fn default() -> Self {
        Self {
            skip_threshold: default_planning_skip(),
            activate_threshold: default_planning_activate(),
        }
    }
}
```

Add `scoring` and `planning` fields to `QueryClassificationConfig`:

```rust
pub struct QueryClassificationConfig {
    // ... existing fields ...
    /// Scoring config for 14-dimension weighted mode.
    #[serde(default)]
    pub scoring: ScoringConfig,
    /// Planner activation thresholds.
    #[serde(default)]
    pub planning: PlanningConfig,
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib scoring_config_deserializes planning_config_deserializes tier_boundaries_deserialize overrides_config_deserializes`
Expected: all PASS

**Step 5: Commit**

```
feat(config): add 14-dimension scoring config types

Add ScoringConfig, DimensionWeights, TierBoundaries, ScoringOverrides,
PlanningConfig, and Tier enum to support the upgraded classifier and
planner/executor architecture.
```

---

### Task 2: Replace 6-dimension scorer with 14-dimension scorer

**Files:**
- Modify: `src/agent/classifier.rs` (replace `WeightedScorer`)
- Test: `src/agent/classifier.rs` (existing + new tests)

**Step 1: Write tests for the new scorer**

Add these tests alongside the existing test module in `classifier.rs`. Keep all existing tests — they must still pass (the public API `classify()` and `classify_with_context()` remain unchanged).

```rust
#[test]
fn scorer_v2_short_simple_message() {
    let config = make_weighted_config_v2();
    let result = classify_with_context(&config, "hi", 0).unwrap();
    assert_eq!(result.hint, "hint:simple");
    assert!(result.agentic_score < 0.3);
    assert!(result.confidence >= 0.7);
}

#[test]
fn scorer_v2_agentic_message() {
    let config = make_weighted_config_v2();
    let result = classify_with_context(
        &config,
        "edit the config file, deploy to staging, then verify the endpoint works",
        0,
    ).unwrap();
    assert!(result.agentic_score >= 0.5, "agentic_score={}", result.agentic_score);
}

#[test]
fn scorer_v2_reasoning_override() {
    let config = make_weighted_config_v2();
    let result = classify_with_context(
        &config,
        "prove this theorem step by step using chain of thought reasoning",
        0,
    ).unwrap();
    assert_eq!(result.hint, "hint:reasoning");
    assert!(result.confidence >= 0.85);
}

#[test]
fn scorer_v2_structured_output_bumps_tier() {
    let config = make_weighted_config_v2();
    // Short message asking for JSON — would normally be SIMPLE
    let result = classify_with_context(&config, "give me json", 0).unwrap();
    // Should be at least MEDIUM due to structured output override
    assert!(result.hint == "hint:medium" || result.hint == "hint:complex" || result.hint == "hint:reasoning");
}

#[test]
fn scorer_v2_multi_step_message() {
    let config = make_weighted_config_v2();
    let result = classify_with_context(
        &config,
        "first read the file, then update the database, step 3 is to notify the team",
        0,
    ).unwrap();
    // Multi-step + imperative + agentic signals → at least MEDIUM
    assert!(result.hint != "hint:simple");
}

#[test]
fn sigmoid_confidence_near_boundary_is_low() {
    // Score exactly at a boundary → confidence should be ~0.5 (ambiguous)
    let confidence = calibrate_confidence(0.0, 12.0);
    assert!((confidence - 0.5).abs() < 0.01);
}

#[test]
fn sigmoid_confidence_far_from_boundary_is_high() {
    let confidence = calibrate_confidence(0.5, 12.0);
    assert!(confidence > 0.99);
}
```

The helper `make_weighted_config_v2()` constructs a `QueryClassificationConfig` with `mode: Weighted` and all tier hints set, using the new `ScoringConfig` defaults.

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib scorer_v2_ sigmoid_confidence`
Expected: compilation errors — new fields and functions don't exist yet.

**Step 3: Implement the 14-dimension scorer**

Replace the body of `WeightedScorer` in `classifier.rs`. Keep the existing `classify()`, `classify_with_decision()`, and `classify_with_context()` public functions — they remain the entry points. The changes are internal to `WeightedScorer`:

- Replace the 6 `score_*` methods with 14 dimension scorers (see design doc for keyword lists)
- Add `score_agentic_task()` that returns both a dimension score and a separate `agentic_score: f64`
- Add `calibrate_confidence(distance: f64, steepness: f64) -> f64` (sigmoid)
- Apply overrides: large context, structured output, reasoning keyword override
- Map weighted score → tier using configurable `TierBoundaries`
- If confidence < threshold → use `ambiguous_default_tier`
- Extend `ClassificationDecision` with `tier`, `confidence`, `agentic_score`, `signals`

The `classify_with_context()` function gains access to the new `ScoringConfig` via the existing `QueryClassificationConfig.scoring` field.

Backward compatibility: the `ClassificationWeights` (old 6-dimension) struct remains in `schema.rs` for deserialization compatibility. When `mode == Weighted`, the scorer now reads from `config.scoring.dimension_weights` instead of `config.weights`.

**Step 4: Run all classifier tests**

Run: `cargo test --lib classifier`
Expected: all old tests + new tests PASS.

**Step 5: Commit**

```
feat(classifier): upgrade to 14-dimension scorer with sigmoid confidence

Replace the 6-dimension WeightedScorer with a 14-dimension scorer
inspired by ClawRouter. Adds agentic score detection, sigmoid confidence
calibration, and scoring overrides for large context, structured output,
and reasoning keywords.

Existing classification API (classify/classify_with_context) unchanged.
ClassificationDecision now carries tier, confidence, agentic_score, and
signals fields.
```

---

### Task 3: Wire upgraded classifier into agent

**Files:**
- Modify: `src/agent/agent.rs:443-473` (`classify_model` method)
- Test: `src/agent/agent.rs` (existing tests must pass)

**Step 1: Write test for classification logging with new fields**

```rust
#[tokio::test]
async fn classify_model_logs_agentic_score() {
    // Build agent with weighted classification enabled
    // Send an agentic message
    // Verify classify_model returns the correct hint
    // (This tests that the new ClassificationDecision fields flow through)
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib classify_model_logs_agentic`
Expected: FAIL

**Step 3: Update `classify_model` to log new fields**

In `agent.rs:443`, update the `classify_model` method to log `agentic_score`, `confidence`, and `tier` from the new `ClassificationDecision` fields. The routing logic stays the same — it still returns `format!("hint:{}", decision.hint)`.

Store `agentic_score` and `confidence` on the `Agent` struct so `plan_then_execute()` can read them later (Phase 3). Add two fields:

```rust
last_agentic_score: f64,
last_confidence: f64,
```

Set them in `classify_model()` after classification.

**Step 4: Run all agent tests**

Run: `cargo test --lib agent`
Expected: all PASS

**Step 5: Commit**

```
feat(agent): wire upgraded classifier and store agentic score

classify_model now logs tier, confidence, agentic_score, and signals.
Stores last_agentic_score and last_confidence on Agent for downstream
planner activation decisions.
```

---

### Task 4: Add observer events for classification

**Files:**
- Modify: `src/observability/traits.rs:10-68`
- Test: `src/observability/traits.rs` (add test)

**Step 1: Write test**

```rust
#[test]
fn classification_result_event_is_cloneable() {
    let event = ObserverEvent::ClassificationResult {
        tier: "medium".into(),
        confidence: 0.85,
        agentic_score: 0.3,
        signals: vec!["code".into(), "technical".into()],
    };
    let cloned = event.clone();
    assert!(matches!(cloned, ObserverEvent::ClassificationResult { .. }));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib classification_result_event`
Expected: compilation error

**Step 3: Add the event variant**

Add to `ObserverEvent` enum:

```rust
/// Result of query classification scoring.
ClassificationResult {
    tier: String,
    confidence: f64,
    agentic_score: f64,
    signals: Vec<String>,
},
```

**Step 4: Run test + full observer tests**

Run: `cargo test --lib observability`
Expected: all PASS

**Step 5: Emit the event in `classify_model`**

In `agent.rs`, after classification, call `self.observer.record_event(&ObserverEvent::ClassificationResult { ... })`.

**Step 6: Run agent tests**

Run: `cargo test --lib agent`
Expected: all PASS

**Step 7: Commit**

```
feat(observability): add ClassificationResult event

Emits tier, confidence, agentic_score, and scoring signals after
each query classification for tracing and diagnostics.
```

---

## Phase 2: Fallback Chains + Context Window Filtering

### Task 5: Extend ModelRouteConfig with fallbacks and context_window

**Files:**
- Modify: `src/config/schema.rs:2314-2324`
- Test: `src/config/schema.rs`

**Step 1: Write tests**

```rust
#[test]
fn model_route_config_with_fallbacks_deserializes() {
    let toml_str = r#"
        hint = "fast"
        provider = "groq"
        model = "llama-3-70b"
        context_window = 131072
        [[fallbacks]]
        provider = "openrouter"
        model = "deepseek-chat"
        context_window = 131072
    "#;
    let config: ModelRouteConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.fallbacks.len(), 1);
    assert_eq!(config.context_window, Some(131072));
}

#[test]
fn model_route_config_without_fallbacks_still_works() {
    let toml_str = r#"
        hint = "fast"
        provider = "groq"
        model = "llama-3-70b"
    "#;
    let config: ModelRouteConfig = toml::from_str(toml_str).unwrap();
    assert!(config.fallbacks.is_empty());
    assert!(config.context_window.is_none());
}
```

**Step 2: Run tests to verify they fail**

Expected: FAIL — `fallbacks` and `context_window` don't exist

**Step 3: Add fields to `ModelRouteConfig`**

```rust
pub struct ModelRouteConfig {
    pub hint: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Ordered fallback models tried when primary fails.
    #[serde(default)]
    pub fallbacks: Vec<FallbackModelConfig>,
    /// Context window size in tokens. Used for filtering.
    #[serde(default)]
    pub context_window: Option<usize>,
}

/// A fallback model entry within a route.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FallbackModelConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub context_window: Option<usize>,
}
```

**Step 4: Run tests**

Run: `cargo test --lib model_route_config`
Expected: all PASS

**Step 5: Commit**

```
feat(config): add fallback chains and context_window to ModelRouteConfig

Routes can now define ordered fallback models and context window sizes
for resilient model selection and context-aware filtering.
```

---

### Task 6: Add fallback resolution and context filtering to RouterProvider

**Files:**
- Modify: `src/providers/router.rs`
- Test: `src/providers/router.rs` (existing + new)

**Step 1: Write tests**

```rust
#[tokio::test]
async fn fallback_used_when_primary_fails() {
    // Create router with a primary that returns Err and a fallback that succeeds
    // Call chat_with_system
    // Assert fallback response received
}

#[test]
fn resolve_with_fallbacks_returns_ordered_chain() {
    // Create router with primary + 2 fallbacks
    // Call resolve_with_fallbacks
    // Assert returns [primary, fallback1, fallback2]
}

#[test]
fn context_filter_removes_small_context_models() {
    // Create fallback chain with 128k and 1M models
    // Filter with 200k estimated tokens
    // Assert 128k model excluded, 1M model included
}
```

**Step 2: Run tests to verify they fail**

Expected: FAIL — `resolve_with_fallbacks` doesn't exist

**Step 3: Implement**

Add to `RouterProvider`:

```rust
/// Resolve a hint to an ordered list of (provider_index, model, context_window).
fn resolve_with_fallbacks(&self, model: &str) -> Vec<(usize, String, Option<usize>)> {
    // Returns primary + fallbacks in order
}

/// Filter a fallback chain by estimated token count.
fn filter_by_context(
    chain: &[(usize, String, Option<usize>)],
    estimated_tokens: usize,
) -> Vec<(usize, String)> {
    // Exclude models whose context_window < estimated_tokens * 1.1
    // If all filtered out, return full chain
}
```

Update the `chat` method (and `chat_with_tools`, `chat_with_system`, `chat_with_history`) to try the fallback chain on error. On primary failure, log a warning and try the next model in the chain.

Add a `FallbackTriggered` observer event (add the variant to `ObserverEvent`):

```rust
FallbackTriggered {
    hint: String,
    failed_model: String,
    fallback_model: String,
    reason: String,
},
```

**Step 4: Run all router tests**

Run: `cargo test --lib router`
Expected: all PASS (old + new)

**Step 5: Commit**

```
feat(router): add fallback chains with context window filtering

RouterProvider now tries fallback models in order when the primary
fails. Models whose context window cannot handle the estimated input
size are filtered out before attempting. Emits FallbackTriggered
observer event on failover.
```

---

## Phase 3: Planner/Executor

### Task 7: Create planner module with plan parsing

**Files:**
- Create: `src/agent/planner.rs`
- Modify: `src/agent/mod.rs` (add `pub mod planner;`)
- Test: `src/agent/planner.rs`

**Step 1: Write tests for plan parsing**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_passthrough_plan() {
        let json = r#"{"passthrough": true}"#;
        let plan = parse_plan(json).unwrap();
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
        let plan = parse_plan(json).unwrap();
        assert!(!plan.is_passthrough());
        assert_eq!(plan.actions.len(), 3);
        let groups = plan.grouped_actions();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2); // group 1
        assert_eq!(groups[1].len(), 1); // group 2
    }

    #[test]
    fn parse_empty_actions_is_passthrough() {
        let json = r#"{"passthrough": false, "actions": []}"#;
        let plan = parse_plan(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let result = parse_plan("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn extract_json_from_markdown_fences() {
        let response = "Here's the plan:\n```json\n{\"passthrough\": true}\n```\nDone.";
        let plan = parse_plan_from_response(response).unwrap();
        assert!(plan.is_passthrough());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib planner`
Expected: compilation error — module doesn't exist

**Step 3: Implement plan types and parsing**

Create `src/agent/planner.rs`:

```rust
use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

/// A parsed action plan from the planner model.
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

fn default_group() -> u32 { 1 }

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
    // Try extracting from ``` ... ``` fences
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
```

Add `pub mod planner;` to `src/agent/mod.rs`.

**Step 4: Run tests**

Run: `cargo test --lib planner`
Expected: all PASS

**Step 5: Commit**

```
feat(planner): add plan types and JSON parsing

Introduces Plan, PlanAction structs with group-based ordering.
Parses JSON plans from LLM responses, handling markdown fences.
Empty actions treated as passthrough.
```

---

### Task 8: Implement action-by-action executor orchestration

**Files:**
- Modify: `src/agent/planner.rs`
- Test: `src/agent/planner.rs`

**Step 1: Write test for result accumulation**

```rust
#[test]
fn accumulated_results_format() {
    let result = ActionResult {
        action_type: "create_issue".to_string(),
        group: 1,
        success: true,
        summary: "Created issue SPO-31 — URL: https://linear.app/spore/issue/SPO-31".to_string(),
        raw_output: "...".to_string(),
    };
    let formatted = result.to_accumulated_line();
    assert!(formatted.contains("create_issue"));
    assert!(formatted.contains("group 1"));
    assert!(formatted.contains("SPO-31"));
}

#[test]
fn build_executor_prompt_includes_action_and_results() {
    let action = PlanAction {
        group: 2,
        action_type: "reply".to_string(),
        description: "Reply with issue links".to_string(),
        tools: vec!["slack_reply".to_string()],
        params: serde_json::json!({"content_hint": "Created 4 issues"}),
    };
    let accumulated = vec![
        "Action \"create_issue\" (group 1): Created SPO-31".to_string(),
    ];
    let prompt = build_executor_prompt(&action, &accumulated);
    assert!(prompt.contains("Reply with issue links"));
    assert!(prompt.contains("Created SPO-31"));
    assert!(prompt.contains("slack_reply"));
}
```

**Step 2: Run tests to verify they fail**

Expected: FAIL — `ActionResult`, `build_executor_prompt` don't exist

**Step 3: Implement executor support types**

Add to `planner.rs`:

```rust
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
        let status = if self.success { "" } else { "FAILED — " };
        format!("Action \"{}\" (group {}): {}{}", self.action_type, self.group, status, self.summary)
    }
}

/// Build the slim executor system prompt for a single action.
pub fn build_executor_prompt(action: &PlanAction, accumulated_results: &[String]) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are executing a single action from a plan. ");
    prompt.push_str("Use the available tools to accomplish exactly what is described. ");
    prompt.push_str("Do not add, skip, or modify the action. ");
    prompt.push_str("Do not make judgment calls — follow the instructions exactly.\n\n");

    prompt.push_str(&format!("ACTION TYPE: {}\n", action.action_type));
    prompt.push_str(&format!("DESCRIPTION: {}\n", action.description));

    if !action.params.is_null() {
        prompt.push_str(&format!("PARAMETERS: {}\n", action.params));
    }

    if !action.tools.is_empty() {
        prompt.push_str(&format!("TOOLS TO USE: {}\n", action.tools.join(", ")));
    }

    if !accumulated_results.is_empty() {
        prompt.push_str("\nRESULTS FROM PRIOR ACTIONS:\n");
        for line in accumulated_results {
            prompt.push_str(&format!("- {line}\n"));
        }
        prompt.push_str("\nUse these results (URLs, IDs) in your action when referenced.\n");
    }

    prompt
}
```

**Step 4: Run tests**

Run: `cargo test --lib planner`
Expected: all PASS

**Step 5: Commit**

```
feat(planner): add action result types and executor prompt builder

ActionResult tracks per-action outcomes for accumulation.
build_executor_prompt constructs the slim per-action system prompt
with filtered context — no persona, just the action and prior results.
```

---

### Task 9: Implement `plan_then_execute()` orchestration function

**Files:**
- Modify: `src/agent/planner.rs`
- Modify: `src/agent/agent.rs` (call `plan_then_execute` from `turn()`)
- Test: integration test with mock provider

This is the largest task. It wires everything together.

**Step 1: Write integration test**

```rust
#[tokio::test]
async fn plan_then_execute_with_passthrough() {
    // Mock provider returns {"passthrough": true} for planner call
    // Verify agent falls through to normal agent_turn behavior
}

#[tokio::test]
async fn plan_then_execute_with_action_plan() {
    // Mock provider returns a 2-action plan for planner call
    // Mock provider returns tool call responses for executor calls
    // Verify both actions executed, results accumulated
}
```

**Step 2: Run tests to verify they fail**

Expected: FAIL — `plan_then_execute` doesn't exist

**Step 3: Implement `plan_then_execute()`**

Add to `planner.rs`:

```rust
/// Two-phase planner/executor flow.
///
/// 1. Calls the planner model (hint:planner) with context but no tools
/// 2. Parses the JSON plan
/// 3. If passthrough, delegates to the standard agent_turn flow
/// 4. Otherwise, executes actions group-by-group through run_tool_call_loop
pub async fn plan_then_execute(
    provider: &dyn Provider,
    planner_model: &str,
    executor_model: &str,
    planner_messages: &[ChatMessage],  // system + context + user message
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    temperature: f64,
    max_tool_iterations: usize,
    // ... other params matching run_tool_call_loop signature
) -> Result<PlanExecutionResult> {
    // Step 1: Call planner (no tools)
    let planner_request = ChatRequest {
        messages: planner_messages,
        tools: None,
    };
    observer.record_event(&ObserverEvent::PlannerRequest { model: planner_model.to_string() });

    let response = provider.chat(planner_request, planner_model, temperature).await?;
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

    // Step 4: Execute action-by-action
    let groups = plan.grouped_actions();
    let mut accumulated: Vec<String> = Vec::new();
    let mut last_output = String::new();

    for group in &groups {
        // Filter tools per action, run in parallel within group
        let group_futures: Vec<_> = group.iter().map(|action| {
            let filtered_tools = filter_tools(tools_registry, &action.tools);
            let prompt = build_executor_prompt(action, &accumulated);
            let messages = vec![
                ChatMessage::system(prompt),
                ChatMessage::user(action.description.clone()),
            ];
            execute_single_action(
                provider,
                executor_model,
                &messages,
                &filtered_tools,
                observer,
                provider_name,
                temperature,
                max_tool_iterations.min(5), // cap at 5 for single actions
            )
        }).collect();

        let results = futures_util::future::join_all(group_futures).await;

        for (action, result) in group.iter().zip(results) {
            match result {
                Ok(output) => {
                    let action_result = ActionResult {
                        action_type: action.action_type.clone(),
                        group: action.group,
                        success: true,
                        summary: output.clone(),
                        raw_output: output.clone(),
                    };
                    accumulated.push(action_result.to_accumulated_line());
                    last_output = output;
                }
                Err(e) => {
                    let action_result = ActionResult {
                        action_type: action.action_type.clone(),
                        group: action.group,
                        success: false,
                        summary: e.to_string(),
                        raw_output: String::new(),
                    };
                    accumulated.push(action_result.to_accumulated_line());
                }
            }
        }
    }

    Ok(PlanExecutionResult::Executed {
        output: last_output,
        action_results: accumulated,
    })
}

/// Filter tools_registry to only tools matching the action's tools list.
fn filter_tools<'a>(registry: &'a [Box<dyn Tool>], wanted: &[String]) -> Vec<&'a dyn Tool> {
    if wanted.is_empty() {
        // No filter specified — give all tools
        return registry.iter().map(|t| t.as_ref()).collect();
    }
    registry.iter()
        .filter(|t| wanted.iter().any(|w| t.name() == w))
        .map(|t| t.as_ref())
        .collect()
}

pub enum PlanExecutionResult {
    Passthrough,
    Executed {
        output: String,
        action_results: Vec<String>,
    },
}
```

**Step 4: Wire into `agent.rs`**

In `Agent::turn()`, after `classify_model()`, check:

```rust
let should_plan = self.available_hints.contains(&"planner".to_string())
    && self.last_agentic_score >= self.classification_config.planning.skip_threshold;

if should_plan {
    // Build planner messages (system + context + user, no tool specs)
    // Call plan_then_execute()
    // If Passthrough, fall through to existing tool loop
    // If Executed, return the output
}
// ... existing tool loop code ...
```

**Step 5: Run all tests**

Run: `cargo test --lib planner agent`
Expected: all PASS

**Step 6: Commit**

```
feat(planner): implement plan_then_execute orchestration

Adds the core planner/executor flow: calls planner model without tools,
parses JSON plan, executes actions group-by-group with parallel
execution within groups. Falls back to passthrough on parse failure
or when plan is empty.

Wired into Agent::turn() — activates when hint:planner route exists
and agentic_score exceeds skip_threshold.
```

---

### Task 10: Add PlannerRequest/PlannerResponse observer events

**Files:**
- Modify: `src/observability/traits.rs`
- Test: `src/observability/traits.rs`

**Step 1: Write test**

```rust
#[test]
fn planner_events_are_cloneable() {
    let req = ObserverEvent::PlannerRequest { model: "claude-sonnet".into() };
    let resp = ObserverEvent::PlannerResponse {
        model: "claude-sonnet".into(),
        plan_text: "{\"passthrough\": true}".into(),
    };
    let _ = req.clone();
    let _ = resp.clone();
}
```

**Step 2: Run test to verify it fails**

**Step 3: Add event variants**

```rust
/// Planner model request (before planning call).
PlannerRequest { model: String },
/// Planner model response (plan JSON).
PlannerResponse { model: String, plan_text: String },
```

**Step 4: Run tests**

Run: `cargo test --lib observability`
Expected: all PASS

**Step 5: Commit**

```
feat(observability): add PlannerRequest/PlannerResponse events

Enables tracing of the planner model call and its JSON plan output
for diagnostics and cost tracking.
```

---

### Task 11: Full integration validation

**Files:**
- All modified files

**Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests PASS

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings

**Step 3: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: clean

**Step 4: Commit any fixups**

```
chore: fix clippy warnings and formatting from planner/executor work
```

---

## Summary

| Phase | Tasks | What it delivers |
|-------|-------|-----------------|
| 1: Classifier | Tasks 1-4 | 14-dimension scoring, agentic detection, sigmoid confidence, observer events |
| 2: Fallback chains | Tasks 5-6 | Resilient model routing with context-aware filtering |
| 3: Planner/executor | Tasks 7-10 | Two-phase conversation flow with action-by-action execution |
| Validation | Task 11 | Full suite green, clippy clean, fmt clean |

Each phase lands independently and delivers value on its own. Phase 3 depends on Phase 1 for the agentic score signal.
