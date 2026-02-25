# Smart Model Routing Design

Date: 2026-02-25

## Problem

ZeroClaw uses the same model for everything: heartbeats, cron jobs, casual conversation, and complex reasoning. This is expensive and fragile:

- Heartbeat/cron tasks that need 10 tokens of judgement burn a full reasoning-class call
- When Gemini returns 500s, retries exhaust on the same provider instead of failing over
- Simple "what time is it?" messages route to the same model as "design a distributed system"

## Solution: Three-Layer Routing

Build routing as three independent layers, each solving one problem. Each layer works standalone and composes with the others.

### Layer 1: Structural Model Overrides

**Problem:** Heartbeats and cron jobs always use `default_model`, even though they only need a cheap model.

**Change:** Add `model` field to `HeartbeatConfig` and `CronConfig`. Pass it as `model_override` to `agent::run`.

**Config schema:**

```toml
[heartbeat]
enabled = true
interval_minutes = 30
model = "gemini-2.0-flash-lite"   # new field

[cron]
enabled = true
model = "gemini-2.0-flash-lite"   # new field
```

**Code changes:**

- `src/config/schema.rs`: Add `model: Option<String>` to `HeartbeatConfig` and `CronConfig`
- `src/daemon/mod.rs` line ~201: Change `None` (3rd arg) to `config.heartbeat.model.clone()`
- Same pattern for cron invocations

**Blast radius:** Minimal. Only affects daemon paths. Falls back to `default_model` when `None`.

### Layer 2: Cross-Provider Failover

**Problem:** `ReliableProvider` already retries on transient errors and walks a model fallback chain, but all fallbacks stay on the same set of providers. When Gemini returns 500/502/503/504 and retries exhaust, there's no escape hatch to a different provider.

**Current architecture:**
- `ReliableProvider.providers: Vec<(String, Box<dyn Provider>)>` — already multi-provider
- `ReliableProvider.model_fallbacks: HashMap<String, Vec<String>>` — model chains within same provider set
- `model_chain()` returns `[original, fallback1, fallback2, ...]`
- Outer loop: model chain. Inner loop: provider list. Innermost: retry count.

**Change:** Add `provider_fallback_models` mapping that pairs fallback models with specific provider indices. When a model's retries exhaust on transient errors, the outer loop can try a different (provider_index, model) pair.

**Config schema:**

```toml
# Existing model_fallbacks (same provider set):
[provider]
model_fallbacks = { "gemini-2.5-pro" = ["gemini-2.0-flash"] }

# New cross-provider failover:
[[model_routes]]
hint = "failover-gemini"
provider = "openrouter"
model = "google/gemini-2.5-pro"
```

The implementation extends `model_chain()` to append cross-provider entries. When a transient error exhausts retries on provider A, the loop naturally falls through to provider B with a different model string.

**Transient error classification:**
- 500, 502, 503, 504 HTTP status codes
- Connection timeout / reset errors
- NOT: 401, 403 (auth — permanent), 400 (bad request — permanent), context window overflow

**Blast radius:** Medium. Changes retry logic in `ReliableProvider`. Existing behavior preserved when no cross-provider fallbacks configured.

### Layer 3: Content-Based Classification (Weighted Scoring)

**Problem:** Current classifier (`src/agent/classifier.rs`) uses single-rule-wins: first keyword/pattern match returns immediately. This can't express "short message with no code markers = simple" because a single keyword match overrides everything.

**Current architecture:**
- `QueryClassificationConfig { enabled, rules: Vec<ClassificationRule> }`
- `ClassificationRule { hint, keywords, patterns, min_length, max_length, priority }`
- `classify_with_decision()` sorts by priority, returns first match
- Result is `hint:XXX` string → resolved by `RouterProvider` to (provider, model)

**Change:** Add weighted multi-dimension scoring alongside the existing rule-based classifier. Config chooses mode: `"rules"` (default, backward-compatible) or `"weighted"`.

**Scoring dimensions (6):**

| Dimension | Signal | Weight (default) |
|-----------|--------|-------------------|
| `length` | Character count, normalized to [0,1] at 2000 chars | 0.20 |
| `code_density` | Ratio of code markers (```, fn, def, class, ->, =>) to total tokens | 0.25 |
| `question_complexity` | Multi-clause questions, "explain", "compare", "design" | 0.20 |
| `conversation_depth` | Turn count in current thread (normalized at 20) | 0.10 |
| `tool_hint` | Message references tools, APIs, or system commands | 0.10 |
| `domain_specificity` | Domain jargon density (configurable keyword sets) | 0.15 |

**Tier boundaries:**

| Tier | Score range | Default model hint |
|------|-------------|-------------------|
| Simple | < -0.1 | `hint:simple` |
| Medium | -0.1 to 0.2 | `hint:medium` |
| Complex | 0.2 to 0.4 | `hint:complex` |
| Reasoning | > 0.4 | `hint:reasoning` |

**Config schema:**

```toml
[query_classification]
enabled = true
mode = "weighted"   # new field; default "rules" for backward compat

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

[[model_routes]]
hint = "simple"
provider = "gemini"
model = "gemini-2.0-flash-lite"

[[model_routes]]
hint = "medium"
provider = "gemini"
model = "gemini-2.0-flash"

[[model_routes]]
hint = "complex"
provider = "gemini"
model = "gemini-2.5-pro"

[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-opus-4-20250514"
```

**Code changes:**

- `src/config/schema.rs`: Add `mode`, `tiers`, `weights` to `QueryClassificationConfig`
- `src/agent/classifier.rs`: Add `WeightedScorer` struct with `score()` method returning tier
- `classify_with_decision()` dispatches to rules or weighted based on `mode`
- `src/agent/agent.rs`: Pass turn count to classifier for `conversation_depth`

**Blast radius:** Medium. New code path only active when `mode = "weighted"`. Existing `"rules"` mode untouched.

## Architecture Diagram

```
                    ┌──────────────┐
                    │ Agent Turn   │
                    └──────┬───────┘
                           │
              ┌────────────▼────────────┐
              │ Layer 1: Structural     │
              │ model_override from     │
              │ heartbeat/cron config   │
              └────────────┬────────────┘
                           │ (if no override)
              ┌────────────▼────────────┐
              │ Layer 3: Classifier     │
              │ rules or weighted       │
              │ → hint:XXX              │
              └────────────┬────────────┘
                           │
              ┌────────────▼────────────┐
              │ RouterProvider          │
              │ resolve hint → provider │
              └────────────┬────────────┘
                           │
              ┌────────────▼────────────┐
              │ Layer 2: Failover       │
              │ ReliableProvider        │
              │ retry → model chain     │
              │ → cross-provider        │
              └────────────┬────────────┘
                           │
                     ┌─────▼─────┐
                     │ Response  │
                     └───────────┘
```

## Non-Goals

- **Runtime model switching mid-conversation** — out of scope; each turn is independently classified
- **Cost tracking/budgets** — useful but separate concern
- **Automatic weight tuning** — start with sensible defaults, tune manually
- **Tool loop detection** — separate issue, not part of routing

## Rollback

Each layer is independently revertable:
- Layer 1: Remove `model` fields from config, daemon passes `None` again
- Layer 2: Don't configure cross-provider fallbacks, existing retry behavior preserved
- Layer 3: Set `mode = "rules"` or leave `mode` unset, weighted scoring never activates

## Testing Strategy

- Layer 1: Unit test that daemon passes model_override when configured
- Layer 2: Integration tests with mock providers that fail transiently, verify failover to different provider
- Layer 3: Unit tests for `WeightedScorer` with known inputs/outputs, tier boundary tests
- All layers: Existing test suite must pass unchanged (backward compatibility)
