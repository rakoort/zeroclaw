# Smart Model Routing Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add three-layer smart model routing: structural overrides for heartbeat/cron, cross-provider failover on transient errors, and weighted content-based classification with tier mapping.

**Architecture:** Each layer is independent and composable. Layer 1 adds `model` to `HeartbeatConfig` (cron already has per-job model). Layer 2 extends `ReliableProvider` with cross-provider fallback entries in `model_chain()`. Layer 3 adds a `WeightedScorer` alongside the existing rule-based classifier, selected via `mode` field.

**Tech Stack:** Rust, serde, schemars (JsonSchema), tokio, anyhow. All changes are in existing crate — no new dependencies.

**Design doc:** `docs/plans/2026-02-25-smart-routing-design.md`

---

## Layer 1: Structural Model Overrides

### Task 1: Add `model` field to `HeartbeatConfig`

**Files:**
- Modify: `src/config/schema.rs:2399-2424` (HeartbeatConfig struct + Default impl)
- Test: `src/config/schema.rs` (existing test module)

**Step 1: Write the failing test**

Add to the existing test module in `src/config/schema.rs`. Find the test section (search for `#[cfg(test)]` near bottom of file). Add:

```rust
#[test]
fn heartbeat_config_model_field_parses() {
    let toml_str = r#"
        [heartbeat]
        enabled = true
        interval_minutes = 15
        model = "gemini-2.0-flash-lite"
    "#;
    // Parse just the heartbeat section using a wrapper
    #[derive(Deserialize)]
    struct Wrapper {
        heartbeat: HeartbeatConfig,
    }
    let w: Wrapper = toml::from_str(toml_str).expect("should parse heartbeat with model");
    assert_eq!(w.heartbeat.model, Some("gemini-2.0-flash-lite".to_string()));
}

#[test]
fn heartbeat_config_model_defaults_to_none() {
    let config = HeartbeatConfig::default();
    assert_eq!(config.model, None);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib heartbeat_config_model -- --nocapture 2>&1 | tail -20`
Expected: compilation error — `HeartbeatConfig` has no field `model`

**Step 3: Add `model` field to `HeartbeatConfig`**

In `src/config/schema.rs`, add to the `HeartbeatConfig` struct (after `to` field, around line 2412):

```rust
/// Optional model override for heartbeat tasks. Falls back to `default_model` when absent.
#[serde(default)]
pub model: Option<String>,
```

Update the `Default` impl (around line 2415) to include `model: None`.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib heartbeat_config_model -- --nocapture 2>&1 | tail -20`
Expected: both tests PASS

**Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add model field to HeartbeatConfig"
```

---

### Task 2: Wire heartbeat model override into daemon

**Files:**
- Modify: `src/daemon/mod.rs:201-205` (agent::run call)
- Test: `src/daemon/mod.rs` (add unit test)

**Step 1: Write the failing test**

The daemon `heartbeat_worker` function is async and deeply integrated. Instead, write a focused test that validates the model_override resolution logic. Add a test in `src/daemon/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_model_override_some_when_configured() {
        let mut config = Config::default();
        config.heartbeat.model = Some("gemini-2.0-flash-lite".into());
        assert_eq!(config.heartbeat.model, Some("gemini-2.0-flash-lite".into()));
    }

    #[test]
    fn heartbeat_model_override_none_when_unconfigured() {
        let config = Config::default();
        assert_eq!(config.heartbeat.model, None);
    }
}
```

**Step 2: Run tests to verify they pass (these are config-level)**

Run: `cargo test --lib daemon::tests -- --nocapture 2>&1 | tail -20`
Expected: PASS (these validate the config plumbing)

**Step 3: Wire model override into the agent::run call**

In `src/daemon/mod.rs` around line 201, change the 4th argument from `None` to `config.heartbeat.model.clone()`:

```rust
match crate::agent::run(
    config.clone(),
    Some(prompt),
    None,
    config.heartbeat.model.clone(),  // was: None
    temp,
    vec![],
    false,
)
```

**Step 4: Verify compilation**

Run: `cargo check 2>&1 | tail -10`
Expected: no errors

**Step 5: Run full test suite for Layer 1**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests pass

**Step 6: Commit**

```bash
git add src/daemon/mod.rs
git commit -m "feat(daemon): pass heartbeat model override to agent::run"
```

---

### Task 3: Add default `model` to `CronConfig`

Cron jobs already support per-job `model` via `CronJob.model` (see `src/cron/types.rs:114`). Add a config-level default so cron jobs without explicit model inherit from `CronConfig.model`.

**Files:**
- Modify: `src/config/schema.rs:2431-2451` (CronConfig struct + Default impl)
- Modify: `src/cron/scheduler.rs:166` (model_override resolution)
- Test: `src/config/schema.rs` + `src/cron/scheduler.rs`

**Step 1: Write the failing test for CronConfig**

In `src/config/schema.rs` test module:

```rust
#[test]
fn cron_config_model_field_parses() {
    #[derive(Deserialize)]
    struct Wrapper {
        cron: CronConfig,
    }
    let toml_str = r#"
        [cron]
        enabled = true
        model = "gemini-2.0-flash-lite"
    "#;
    let w: Wrapper = toml::from_str(toml_str).expect("should parse cron with model");
    assert_eq!(w.cron.model, Some("gemini-2.0-flash-lite".to_string()));
}

#[test]
fn cron_config_model_defaults_to_none() {
    let config = CronConfig::default();
    assert_eq!(config.model, None);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib cron_config_model -- --nocapture 2>&1 | tail -20`
Expected: compilation error — `CronConfig` has no field `model`

**Step 3: Add `model` field to `CronConfig`**

In `src/config/schema.rs`, add to the `CronConfig` struct (after `max_run_history`, around line 2437):

```rust
/// Default model for cron jobs that don't specify their own. Falls back to `default_model`.
#[serde(default)]
pub model: Option<String>,
```

Update the `Default` impl (around line 2444) to include `model: None`.

**Step 4: Wire cron config model into scheduler**

In `src/cron/scheduler.rs` around line 166, change:

```rust
let model_override = job.model.clone();
```

to:

```rust
let model_override = job.model.clone().or_else(|| config.cron.model.clone());
```

This means: per-job model takes priority, then cron config default, then agent::run falls back to `default_model`.

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib cron_config_model -- --nocapture 2>&1 | tail -5`
Expected: PASS

**Step 6: Run full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests pass

**Step 7: Commit**

```bash
git add src/config/schema.rs src/cron/scheduler.rs
git commit -m "feat(config): add default model to CronConfig, wire into scheduler"
```

---

## Layer 2: Cross-Provider Failover

### Task 4: Add `provider_fallbacks` config field

**Files:**
- Modify: `src/config/schema.rs:2189-2258` (ReliabilityConfig)
- Test: `src/config/schema.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn reliability_provider_fallbacks_parses() {
    #[derive(Deserialize)]
    struct Wrapper {
        reliability: ReliabilityConfig,
    }
    let toml_str = r#"
        [reliability]
        provider_retries = 3

        [reliability.provider_fallbacks]
        "gemini-2.5-pro" = [{ provider = "openrouter", model = "google/gemini-2.5-pro" }]
    "#;
    let w: Wrapper = toml::from_str(toml_str).expect("should parse provider_fallbacks");
    let chain = w.reliability.provider_fallbacks.get("gemini-2.5-pro").unwrap();
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].provider, "openrouter");
    assert_eq!(chain[0].model, "google/gemini-2.5-pro");
}

#[test]
fn reliability_provider_fallbacks_defaults_to_empty() {
    let config = ReliabilityConfig::default();
    assert!(config.provider_fallbacks.is_empty());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib reliability_provider_fallbacks -- --nocapture 2>&1 | tail -20`
Expected: compilation error

**Step 3: Add config types**

In `src/config/schema.rs`, add a new struct near `ReliabilityConfig`:

```rust
/// A cross-provider fallback entry: try this model on this provider when the primary fails.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderFallbackEntry {
    /// Provider name (must match a key in provider config, e.g. "openrouter").
    pub provider: String,
    /// Model identifier for this provider.
    pub model: String,
}
```

Add to `ReliabilityConfig` struct (after `model_fallbacks`):

```rust
/// Cross-provider fallback chains: when retries exhaust on the primary provider,
/// try these (provider, model) pairs in order.
/// Key = primary model name, Value = list of fallback entries.
#[serde(default)]
pub provider_fallbacks: HashMap<String, Vec<ProviderFallbackEntry>>,
```

Update `ReliabilityConfig::default()` to include `provider_fallbacks: HashMap::new()`.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib reliability_provider_fallbacks -- --nocapture 2>&1 | tail -10`
Expected: PASS

**Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add provider_fallbacks to ReliabilityConfig"
```

---

### Task 5: Wire provider fallbacks into `ReliableProvider`

**Files:**
- Modify: `src/providers/reliable.rs:225-270` (struct + model_chain)
- Modify: `src/providers/mod.rs:1337-1345` (factory wiring)
- Test: `src/providers/reliable.rs`

**Step 1: Write the failing test**

Add to `src/providers/reliable.rs` test module:

```rust
#[tokio::test]
async fn cross_provider_failover_on_transient_error() {
    // Primary provider always fails with 500
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));

    let provider = ReliableProvider::new(
        vec![
            (
                "gemini".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&primary_calls),
                    fail_until_attempt: usize::MAX,
                    response: "",
                    error: "500 Internal Server Error",
                }) as Box<dyn Provider>,
            ),
            (
                "openrouter".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&fallback_calls),
                    fail_until_attempt: 0,
                    response: "ok from fallback",
                    error: "",
                }) as Box<dyn Provider>,
            ),
        ],
        1, // max 1 retry
        10,
    )
    .with_provider_fallbacks(vec![
        ("gemini-2.5-pro".to_string(), 1, "google/gemini-2.5-pro".to_string()),
    ]);

    let result = provider
        .simple_chat("hello", "gemini-2.5-pro", 0.0)
        .await
        .unwrap();
    assert_eq!(result, "ok from fallback");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib cross_provider_failover -- --nocapture 2>&1 | tail -20`
Expected: compilation error — `with_provider_fallbacks` doesn't exist

**Step 3: Implement cross-provider fallback in ReliableProvider**

Add a new field to `ReliableProvider`:

```rust
/// Cross-provider fallback entries: (primary_model, provider_index, fallback_model)
/// Appended to model_chain when primary retries exhaust on transient errors.
provider_fallback_models: Vec<(String, usize, String)>,
```

Initialize to `vec![]` in `new()`.

Add builder method:

```rust
/// Set cross-provider fallback entries.
/// Each entry: (primary_model, target_provider_index, fallback_model_on_that_provider)
pub fn with_provider_fallbacks(mut self, entries: Vec<(String, usize, String)>) -> Self {
    self.provider_fallback_models = entries;
    self
}
```

Modify `model_chain()` to append cross-provider entries. The key insight: the existing retry loop already iterates `for current_model in &models { for (provider_name, provider) in &self.providers { ... } }`. Cross-provider entries work by adding models that are only valid on specific providers. We need a different approach — instead of returning `Vec<&str>`, return entries that pair model with optional provider index constraint.

Actually, simpler approach: extend `model_chain()` to return `Vec<(Option<usize>, &str)>` where `None` means "try all providers" and `Some(idx)` means "only try this provider". This keeps the retry loop structure.

Define:

```rust
fn model_chain_with_providers<'a>(&'a self, model: &'a str) -> Vec<(Option<usize>, &'a str)> {
    let mut chain: Vec<(Option<usize>, &str)> = vec![(None, model)];
    // Same-provider model fallbacks
    if let Some(fallbacks) = self.model_fallbacks.get(model) {
        chain.extend(fallbacks.iter().map(|s| (None, s.as_str())));
    }
    // Cross-provider fallbacks
    for (primary, provider_idx, fallback_model) in &self.provider_fallback_models {
        if primary == model {
            chain.push((Some(*provider_idx), fallback_model.as_str()));
        }
    }
    chain
}
```

Update the retry loop in `simple_chat`, `chat_with_history`, `chat_with_tools`, `chat`, and `stream_chat_with_system` to use `model_chain_with_providers`. The inner provider loop checks: if `provider_constraint` is `Some(idx)`, only try that provider index; if `None`, try all providers as before.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib cross_provider_failover -- --nocapture 2>&1 | tail -10`
Expected: PASS

**Step 5: Run full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests pass (existing behavior preserved — `model_chain` still used for non-cross-provider cases)

**Step 6: Commit**

```bash
git add src/providers/reliable.rs
git commit -m "feat(reliable): add cross-provider failover on transient errors"
```

---

### Task 6: Wire provider fallbacks from config to ReliableProvider

**Files:**
- Modify: `src/providers/mod.rs:1337-1345` (reliable provider factory)
- Test: integration-level — run full suite

**Step 1: Convert config entries to provider index tuples**

In `src/providers/mod.rs`, where `ReliableProvider` is constructed (around line 1337), after `.with_model_fallbacks(...)`, add `.with_provider_fallbacks(...)`. The conversion needs to resolve provider names to indices in the `providers` vec.

```rust
// Build cross-provider fallback entries
let provider_fallback_entries: Vec<(String, usize, String)> = reliability
    .provider_fallbacks
    .iter()
    .flat_map(|(primary_model, entries)| {
        entries.iter().filter_map(|entry| {
            let idx = providers
                .iter()
                .position(|(name, _)| name == &entry.provider);
            match idx {
                Some(i) => Some((primary_model.clone(), i, entry.model.clone())),
                None => {
                    tracing::warn!(
                        primary_model = primary_model.as_str(),
                        fallback_provider = entry.provider.as_str(),
                        "Provider fallback references unknown provider, skipping"
                    );
                    None
                }
            }
        })
    })
    .collect();

let reliable = ReliableProvider::new(
    providers,
    reliability.provider_retries,
    reliability.provider_backoff_ms,
)
.with_api_keys(reliability.api_keys.clone())
.with_model_fallbacks(reliability.model_fallbacks.clone())
.with_provider_fallbacks(provider_fallback_entries);
```

**Step 2: Verify compilation**

Run: `cargo check 2>&1 | tail -10`
Expected: no errors

**Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests pass

**Step 4: Commit**

```bash
git add src/providers/mod.rs
git commit -m "feat(providers): wire provider_fallbacks config into ReliableProvider factory"
```

---

## Layer 3: Weighted Content-Based Classification

### Task 7: Add weighted classifier config fields

**Files:**
- Modify: `src/config/schema.rs:2364-2397` (QueryClassificationConfig)
- Test: `src/config/schema.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn query_classification_weighted_mode_parses() {
    #[derive(Deserialize)]
    struct Wrapper {
        query_classification: QueryClassificationConfig,
    }
    let toml_str = r#"
        [query_classification]
        enabled = true
        mode = "weighted"

        [query_classification.tiers]
        simple = "hint:simple"
        medium = "hint:medium"
        complex = "hint:complex"
        reasoning = "hint:reasoning"

        [query_classification.weights]
        length = 0.20
        code_density = 0.25
        question_complexity = 0.20
        conversation_depth = 0.10
        tool_hint = 0.10
        domain_specificity = 0.15
    "#;
    let w: Wrapper = toml::from_str(toml_str).expect("should parse weighted mode");
    assert_eq!(w.query_classification.mode, ClassificationMode::Weighted);
    assert_eq!(w.query_classification.tiers.simple, Some("hint:simple".into()));
    assert!(w.query_classification.weights.length > 0.0);
}

#[test]
fn query_classification_mode_defaults_to_rules() {
    let config = QueryClassificationConfig::default();
    assert_eq!(config.mode, ClassificationMode::Rules);
    assert!(config.tiers.simple.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib query_classification_weighted -- --nocapture 2>&1 | tail -20`
Expected: compilation error

**Step 3: Add config types**

Add near `QueryClassificationConfig`:

```rust
/// Classification mode: rule-based (default, backward-compatible) or weighted scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClassificationMode {
    Rules,
    Weighted,
}

impl Default for ClassificationMode {
    fn default() -> Self {
        Self::Rules
    }
}

/// Tier-to-hint mapping for weighted classification.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClassificationTiers {
    #[serde(default)]
    pub simple: Option<String>,
    #[serde(default)]
    pub medium: Option<String>,
    #[serde(default)]
    pub complex: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Dimension weights for weighted classification scoring.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClassificationWeights {
    #[serde(default = "default_weight_length")]
    pub length: f64,
    #[serde(default = "default_weight_code_density")]
    pub code_density: f64,
    #[serde(default = "default_weight_question_complexity")]
    pub question_complexity: f64,
    #[serde(default = "default_weight_conversation_depth")]
    pub conversation_depth: f64,
    #[serde(default = "default_weight_tool_hint")]
    pub tool_hint: f64,
    #[serde(default = "default_weight_domain_specificity")]
    pub domain_specificity: f64,
}

fn default_weight_length() -> f64 { 0.20 }
fn default_weight_code_density() -> f64 { 0.25 }
fn default_weight_question_complexity() -> f64 { 0.20 }
fn default_weight_conversation_depth() -> f64 { 0.10 }
fn default_weight_tool_hint() -> f64 { 0.10 }
fn default_weight_domain_specificity() -> f64 { 0.15 }

impl Default for ClassificationWeights {
    fn default() -> Self {
        Self {
            length: default_weight_length(),
            code_density: default_weight_code_density(),
            question_complexity: default_weight_question_complexity(),
            conversation_depth: default_weight_conversation_depth(),
            tool_hint: default_weight_tool_hint(),
            domain_specificity: default_weight_domain_specificity(),
        }
    }
}
```

Add fields to `QueryClassificationConfig`:

```rust
pub struct QueryClassificationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: ClassificationMode,
    #[serde(default)]
    pub rules: Vec<ClassificationRule>,
    #[serde(default)]
    pub tiers: ClassificationTiers,
    #[serde(default)]
    pub weights: ClassificationWeights,
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib query_classification_weighted -- --nocapture 2>&1 | tail -10`
Expected: PASS

**Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add weighted classification mode, tiers, and weights"
```

---

### Task 8: Implement `WeightedScorer`

**Files:**
- Modify: `src/agent/classifier.rs` (add WeightedScorer + score dimensions)
- Test: `src/agent/classifier.rs`

**Step 1: Write the failing tests**

Add to `src/agent/classifier.rs` test module:

```rust
#[test]
fn weighted_scorer_short_simple_message() {
    let weights = ClassificationWeights::default();
    let tiers = ClassificationTiers {
        simple: Some("hint:simple".into()),
        medium: Some("hint:medium".into()),
        complex: Some("hint:complex".into()),
        reasoning: Some("hint:reasoning".into()),
    };
    let scorer = WeightedScorer::new(&weights, &tiers);
    // Short greeting — should score as Simple
    let result = scorer.classify("hi", 0);
    assert_eq!(result, Some("hint:simple".to_string()));
}

#[test]
fn weighted_scorer_code_heavy_message() {
    let weights = ClassificationWeights::default();
    let tiers = ClassificationTiers {
        simple: Some("hint:simple".into()),
        medium: Some("hint:medium".into()),
        complex: Some("hint:complex".into()),
        reasoning: Some("hint:reasoning".into()),
    };
    let scorer = WeightedScorer::new(&weights, &tiers);
    // Code-heavy message with markers
    let msg = "```rust\nfn main() {\n    let x = 42;\n    println!(\"{x}\");\n}\n```\nPlease explain this code and refactor it for better error handling";
    let result = scorer.classify(msg, 5);
    // Should be at least Complex tier due to code density + length + question complexity
    assert!(result == Some("hint:complex".into()) || result == Some("hint:reasoning".into()));
}

#[test]
fn weighted_scorer_medium_question() {
    let weights = ClassificationWeights::default();
    let tiers = ClassificationTiers {
        simple: Some("hint:simple".into()),
        medium: Some("hint:medium".into()),
        complex: Some("hint:complex".into()),
        reasoning: Some("hint:reasoning".into()),
    };
    let scorer = WeightedScorer::new(&weights, &tiers);
    // Medium-length question, no code
    let result = scorer.classify("What's the weather like in London today?", 2);
    assert!(result == Some("hint:simple".into()) || result == Some("hint:medium".into()));
}

#[test]
fn weighted_scorer_reasoning_message() {
    let weights = ClassificationWeights::default();
    let tiers = ClassificationTiers {
        simple: Some("hint:simple".into()),
        medium: Some("hint:medium".into()),
        complex: Some("hint:complex".into()),
        reasoning: Some("hint:reasoning".into()),
    };
    let scorer = WeightedScorer::new(&weights, &tiers);
    // Long, complex reasoning request
    let msg = "Compare and contrast the tradeoffs between using a microservices architecture versus a monolithic architecture for a high-traffic e-commerce platform. Consider scalability, deployment complexity, data consistency, and team organization. Explain which approach you would recommend and why, with specific design patterns for each critical subsystem.";
    let result = scorer.classify(msg, 15);
    assert_eq!(result, Some("hint:reasoning".to_string()));
}

#[test]
fn weighted_scorer_no_tiers_returns_none() {
    let weights = ClassificationWeights::default();
    let tiers = ClassificationTiers::default(); // all None
    let scorer = WeightedScorer::new(&weights, &tiers);
    let result = scorer.classify("hello", 0);
    assert_eq!(result, None);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib weighted_scorer -- --nocapture 2>&1 | tail -20`
Expected: compilation error — `WeightedScorer` doesn't exist

**Step 3: Implement `WeightedScorer`**

Add to `src/agent/classifier.rs`:

```rust
use crate::config::schema::{ClassificationTiers, ClassificationWeights};

/// Tier boundaries (score thresholds).
const TIER_SIMPLE_MAX: f64 = -0.1;
const TIER_MEDIUM_MAX: f64 = 0.2;
const TIER_COMPLEX_MAX: f64 = 0.4;

/// Weighted multi-dimension scorer for content-based classification.
pub struct WeightedScorer<'a> {
    weights: &'a ClassificationWeights,
    tiers: &'a ClassificationTiers,
}

impl<'a> WeightedScorer<'a> {
    pub fn new(weights: &'a ClassificationWeights, tiers: &'a ClassificationTiers) -> Self {
        Self { weights, tiers }
    }

    /// Classify a message into a tier and return the corresponding hint string.
    /// Returns `None` if the resolved tier has no hint configured.
    pub fn classify(&self, message: &str, turn_count: usize) -> Option<String> {
        let score = self.score(message, turn_count);
        let tier_hint = if score < TIER_SIMPLE_MAX {
            &self.tiers.simple
        } else if score < TIER_MEDIUM_MAX {
            &self.tiers.medium
        } else if score < TIER_COMPLEX_MAX {
            &self.tiers.complex
        } else {
            &self.tiers.reasoning
        };
        tier_hint.clone()
    }

    /// Compute the weighted score across all dimensions. Range roughly [-1, 1].
    fn score(&self, message: &str, turn_count: usize) -> f64 {
        let length = self.score_length(message);
        let code = self.score_code_density(message);
        let question = self.score_question_complexity(message);
        let depth = self.score_conversation_depth(turn_count);
        let tool = self.score_tool_hint(message);
        let domain = self.score_domain_specificity(message);

        // Weighted sum, centered around 0
        (length * self.weights.length
            + code * self.weights.code_density
            + question * self.weights.question_complexity
            + depth * self.weights.conversation_depth
            + tool * self.weights.tool_hint
            + domain * self.weights.domain_specificity)
            / self.total_weight()
    }

    fn total_weight(&self) -> f64 {
        let sum = self.weights.length
            + self.weights.code_density
            + self.weights.question_complexity
            + self.weights.conversation_depth
            + self.weights.tool_hint
            + self.weights.domain_specificity;
        if sum <= 0.0 { 1.0 } else { sum }
    }

    /// Length: 0.0 at 0 chars, 1.0 at 2000+ chars, normalized linearly.
    fn score_length(&self, message: &str) -> f64 {
        let len = message.len() as f64;
        (len / 2000.0).min(1.0) * 2.0 - 1.0 // maps [0, 2000+] to [-1, 1]
    }

    /// Code density: ratio of code markers to total word count.
    fn score_code_density(&self, message: &str) -> f64 {
        let markers = ["```", "fn ", "def ", "class ", "->", "=>", "import ", "use ",
                        "pub ", "async ", "struct ", "impl ", "return ", "const ", "let "];
        let word_count = message.split_whitespace().count().max(1) as f64;
        let marker_count: f64 = markers.iter()
            .map(|m| message.matches(m).count() as f64)
            .sum();
        let density = (marker_count / word_count).min(1.0);
        density * 2.0 - 1.0 // maps [0, 1] to [-1, 1]
    }

    /// Question complexity: presence of complex question words.
    fn score_question_complexity(&self, message: &str) -> f64 {
        let lower = message.to_lowercase();
        let complex_words = ["explain", "compare", "contrast", "design", "architect",
                             "analyze", "evaluate", "tradeoff", "trade-off", "recommend",
                             "why", "how does", "what are the implications"];
        let simple_words = ["what", "when", "where", "who", "yes", "no", "ok", "thanks"];

        let complex_hits: f64 = complex_words.iter()
            .filter(|w| lower.contains(*w))
            .count() as f64;
        let simple_hits: f64 = simple_words.iter()
            .filter(|w| lower.contains(*w))
            .count() as f64;

        let net = complex_hits - simple_hits * 0.5;
        (net / 3.0).clamp(-1.0, 1.0)
    }

    /// Conversation depth: 0 at turn 0, 1.0 at turn 20+.
    fn score_conversation_depth(&self, turn_count: usize) -> f64 {
        let normalized = (turn_count as f64 / 20.0).min(1.0);
        normalized * 2.0 - 1.0
    }

    /// Tool hint: message references tools, APIs, or system commands.
    fn score_tool_hint(&self, message: &str) -> f64 {
        let lower = message.to_lowercase();
        let tool_words = ["api", "endpoint", "curl", "http", "webhook", "command",
                          "shell", "execute", "run", "deploy", "docker", "database",
                          "query", "mutation", "graphql", "rest"];
        let hits: f64 = tool_words.iter()
            .filter(|w| lower.contains(*w))
            .count() as f64;
        ((hits / 3.0) * 2.0 - 1.0).clamp(-1.0, 1.0)
    }

    /// Domain specificity: density of technical/domain jargon.
    fn score_domain_specificity(&self, message: &str) -> f64 {
        let lower = message.to_lowercase();
        let domain_words = ["architecture", "microservice", "monolith", "scalab",
                            "latency", "throughput", "consistency", "partition",
                            "replication", "sharding", "caching", "load balanc",
                            "circuit breaker", "backpressure", "idempoten"];
        let word_count = message.split_whitespace().count().max(1) as f64;
        let hits: f64 = domain_words.iter()
            .filter(|w| lower.contains(*w))
            .count() as f64;
        let density = (hits / word_count.sqrt()).min(1.0);
        density * 2.0 - 1.0
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib weighted_scorer -- --nocapture 2>&1 | tail -20`
Expected: all 5 tests PASS

**Step 5: Commit**

```bash
git add src/agent/classifier.rs
git commit -m "feat(classifier): add WeightedScorer with 6-dimension content scoring"
```

---

### Task 9: Wire weighted mode into `classify_with_decision`

**Files:**
- Modify: `src/agent/classifier.rs` (dispatch on mode)
- Test: `src/agent/classifier.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn classify_weighted_mode_returns_tier_hint() {
    let config = QueryClassificationConfig {
        enabled: true,
        mode: ClassificationMode::Weighted,
        rules: vec![],
        tiers: ClassificationTiers {
            simple: Some("hint:simple".into()),
            medium: Some("hint:medium".into()),
            complex: Some("hint:complex".into()),
            reasoning: Some("hint:reasoning".into()),
        },
        weights: ClassificationWeights::default(),
    };
    // Short message should classify as simple
    let result = classify(&config, "hi");
    assert_eq!(result, Some("hint:simple".to_string()));
}

#[test]
fn classify_rules_mode_still_works() {
    let config = QueryClassificationConfig {
        enabled: true,
        mode: ClassificationMode::Rules,
        rules: vec![ClassificationRule {
            hint: "fast".into(),
            keywords: vec!["hello".into()],
            ..Default::default()
        }],
        tiers: ClassificationTiers::default(),
        weights: ClassificationWeights::default(),
    };
    assert_eq!(classify(&config, "hello"), Some("fast".into()));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib classify_weighted_mode -- --nocapture 2>&1 | tail -20`
Expected: compilation error — `QueryClassificationConfig` constructor missing new fields

**Step 3: Update `classify_with_decision` to dispatch on mode**

Modify `classify_with_decision` in `src/agent/classifier.rs`:

```rust
pub fn classify_with_decision(
    config: &QueryClassificationConfig,
    message: &str,
) -> Option<ClassificationDecision> {
    classify_with_context(config, message, 0)
}

/// Classify with conversation context (turn count for weighted mode).
pub fn classify_with_context(
    config: &QueryClassificationConfig,
    message: &str,
    turn_count: usize,
) -> Option<ClassificationDecision> {
    if !config.enabled {
        return None;
    }

    match config.mode {
        ClassificationMode::Weighted => {
            let scorer = WeightedScorer::new(&config.weights, &config.tiers);
            scorer.classify(message, turn_count).map(|hint| ClassificationDecision {
                hint,
                priority: 0, // weighted mode doesn't use priority
            })
        }
        ClassificationMode::Rules => {
            if config.rules.is_empty() {
                return None;
            }
            // existing rule-based logic
            let lower = message.to_lowercase();
            let len = message.len();
            let mut rules: Vec<_> = config.rules.iter().collect();
            rules.sort_by(|a, b| b.priority.cmp(&a.priority));
            for rule in rules {
                if let Some(min) = rule.min_length {
                    if len < min { continue; }
                }
                if let Some(max) = rule.max_length {
                    if len > max { continue; }
                }
                let keyword_hit = rule.keywords.iter().any(|kw| lower.contains(&kw.to_lowercase()));
                let pattern_hit = rule.patterns.iter().any(|pat| message.contains(pat.as_str()));
                if keyword_hit || pattern_hit {
                    return Some(ClassificationDecision {
                        hint: rule.hint.clone(),
                        priority: rule.priority,
                    });
                }
            }
            None
        }
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib classifier -- --nocapture 2>&1 | tail -20`
Expected: all classifier tests PASS (both old and new)

**Step 5: Commit**

```bash
git add src/agent/classifier.rs
git commit -m "feat(classifier): wire weighted mode into classify dispatch"
```

---

### Task 10: Pass turn count from agent to classifier

**Files:**
- Modify: `src/agent/agent.rs:443-465` (classify_model method)
- Test: existing tests should still pass

**Step 1: Examine the classify_model call site**

The `classify_model` method at `src/agent/agent.rs:443` currently calls `classify_with_decision`. It needs to call `classify_with_context` and pass the turn count.

**Step 2: Find where turn count is tracked**

Search for turn counter / message count in agent.rs. The agent likely tracks this in its state.

**Step 3: Update `classify_model` to pass turn count**

In `src/agent/agent.rs`, update the `classify_model` method to accept and pass a turn count:

```rust
fn classify_model(&self, user_message: &str, turn_count: usize) -> String {
    if let Some(decision) =
        super::classifier::classify_with_context(&self.classification_config, user_message, turn_count)
    {
        // ... rest unchanged
    }
    // ... rest unchanged
}
```

Update all call sites of `classify_model` to pass the current turn count. If the agent doesn't track turn count, use `messages.len() / 2` as a reasonable approximation.

**Step 4: Verify compilation and tests**

Run: `cargo check 2>&1 | tail -10`
Run: `cargo test 2>&1 | tail -5`
Expected: all pass

**Step 5: Commit**

```bash
git add src/agent/agent.rs
git commit -m "feat(agent): pass turn count to classifier for weighted scoring"
```

---

### Task 11: Integration test — full routing stack

**Files:**
- Test: `src/agent/classifier.rs` or `tests/` if integration tests exist

**Step 1: Write an end-to-end classification test**

```rust
#[test]
fn full_weighted_classification_flow() {
    let config = QueryClassificationConfig {
        enabled: true,
        mode: ClassificationMode::Weighted,
        rules: vec![],
        tiers: ClassificationTiers {
            simple: Some("hint:simple".into()),
            medium: Some("hint:medium".into()),
            complex: Some("hint:complex".into()),
            reasoning: Some("hint:reasoning".into()),
        },
        weights: ClassificationWeights::default(),
    };

    // Short greeting → simple
    assert_eq!(classify(&config, "hi"), Some("hint:simple".into()));

    // Long complex reasoning request → reasoning
    let reasoning_msg = "Compare and contrast the tradeoffs between microservices and monolithic architecture for a high-traffic e-commerce platform. Consider scalability, deployment complexity, data consistency, team organization, and latency implications. Explain your recommendation with specific design patterns.";
    let result = classify_with_context(&config, reasoning_msg, 15);
    assert!(
        result.as_ref().map(|d| d.hint.as_str()) == Some("hint:reasoning")
            || result.as_ref().map(|d| d.hint.as_str()) == Some("hint:complex"),
        "Long reasoning request should be complex or reasoning tier, got: {:?}", result
    );
}
```

**Step 2: Run test**

Run: `cargo test --lib full_weighted_classification -- --nocapture 2>&1 | tail -10`
Expected: PASS

**Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests pass

**Step 4: Commit**

```bash
git add src/agent/classifier.rs
git commit -m "test(classifier): add integration test for full weighted classification flow"
```

---

### Task 12: Final verification

**Step 1: Run format check**

Run: `cargo fmt --all -- --check 2>&1 | tail -10`
Expected: no formatting issues

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings

**Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -10`
Expected: all tests pass

**Step 4: Fix any issues found in steps 1-3**

If clippy or fmt finds issues, fix them and re-run.

**Step 5: Final commit (if fixes needed)**

```bash
git add -A
git commit -m "chore: fix clippy/fmt issues from smart routing implementation"
```
