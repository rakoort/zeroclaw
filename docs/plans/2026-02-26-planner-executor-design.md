# Planner/Executor Architecture Design

**Date:** 2026-02-26
**Status:** Draft

## Problem

ZeroClaw routes each conversation to a single model. The same model handles reasoning (what to do), execution (tool calls), and content generation (writing replies). This forces a cost/quality tradeoff: strong models waste money on mechanical work, fast models make bad judgment calls.

Concrete failure modes with a single fast model:
- Closes an issue that should stay open (wrong judgment)
- Fabricates issue URLs instead of reading tool output (hallucination)
- Misinterprets ambiguous requests (weak reasoning)

The current classifier (6 dimensions, linear thresholds, no confidence calibration) also underperforms on routing accuracy. It cannot distinguish agentic multi-step tasks from simple complex questions, and has no fallback when a selected model can't handle the request's context size.

## Solution

Two complementary changes:

1. **Classifier upgrade** — Expand from 6 to 14 weighted dimensions (inspired by ClawRouter's scoring engine), add sigmoid confidence calibration, agentic score detection, fallback chains, context window filtering, and overrides for large context and structured output. This improves routing quality for all conversations and provides the signal the planner needs.

2. **Planner/executor split** — For messages the upgraded classifier flags as agentic/multi-step, route through a two-phase flow: a reasoning model plans, a fast model executes action-by-action.

## Part 1: Classifier Upgrade

### Current State (6 dimensions)

| Dimension | Weight |
|-----------|--------|
| length | 0.20 |
| code_density | 0.25 |
| question_complexity | 0.20 |
| conversation_depth | 0.10 |
| tool_hint | 0.10 |
| domain_specificity | 0.15 |

Linear tier boundaries, no confidence calibration, no agentic detection.

### Target State (14 dimensions + agentic score)

| Dimension | Weight | Description |
|-----------|--------|-------------|
| token_count | 0.08 | Short → simple, long → complex |
| code_presence | 0.15 | Code keywords and fenced blocks |
| reasoning_markers | 0.18 | "prove", "derive", "step by step", "chain of thought" |
| technical_terms | 0.10 | "algorithm", "architecture", "distributed", "kubernetes" |
| creative_markers | 0.05 | "story", "poem", "brainstorm", "compose" |
| simple_indicators | 0.02 | "what is", "define", "translate", "hello" |
| multi_step_patterns | 0.12 | Regexes: `first.*then`, `step \d`, `\d\.\s` |
| question_complexity | 0.05 | Count of `?` in message (>3 = complex) |
| imperative_verbs | 0.03 | "build", "create", "implement", "deploy", "configure" |
| constraint_count | 0.04 | "at most", "within", "no more than", "maximum", "budget" |
| output_format | 0.03 | "json", "yaml", "table", "csv", "markdown", "schema" |
| reference_complexity | 0.02 | "above", "previous", "the docs", "the api", "the code" |
| negation_complexity | 0.01 | "don't", "avoid", "never", "without", "except" |
| domain_specificity | 0.02 | "quantum", "fpga", "genomics", "zero-knowledge" |
| agentic_task | 0.10 | "edit", "modify", "deploy", "fix", "debug", "step 1", "after that" |

Weights sum to 1.0. The `conversation_depth` dimension from the current scorer is replaced by `multi_step_patterns` and `question_complexity`, which better capture the underlying signal.

### Agentic Score

Scored separately from the main weighted average (like ClawRouter). Returns a float in [0, 1]:

| Agentic keyword matches | Score | Interpretation |
|--------------------------|-------|----------------|
| 0 | 0.0 | Not agentic |
| 1-2 | 0.2 | Mildly agentic |
| 3 | 0.6 | Likely agentic |
| 4+ | 1.0 | Strongly agentic |

The agentic score feeds into both tier selection (via its dimension weight) and the planner activation decision (see Part 2).

Agentic keywords target explicit multi-step and tool-using intent: "read file", "edit", "modify", "update the", "execute", "deploy", "install", "after that", "once done", "step 1", "fix", "debug", "until it works", "iterate", "verify".

### Sigmoid Confidence Calibration

Replace linear tier boundaries with sigmoid-calibrated confidence:

```rust
fn calibrate_confidence(distance_from_boundary: f64, steepness: f64) -> f64 {
    1.0 / (1.0 + (-steepness * distance_from_boundary).exp())
}
```

- `steepness` = 12.0 (configurable)
- `confidence_threshold` = 0.7

When the weighted score lands near a tier boundary, confidence drops below threshold → tier is marked **ambiguous**. Ambiguous classifications fall back to a configurable default tier (MEDIUM).

This produces three zones per boundary instead of a hard cutoff:
- **Confident high** (confidence >= 0.7) → use the classified tier
- **Ambiguous** (confidence < 0.7) → use default tier
- **Confident low** (confidence >= 0.7) → use the classified tier

### Tier Boundaries

```rust
const SIMPLE_MEDIUM: f64 = 0.0;
const MEDIUM_COMPLEX: f64 = 0.3;
const COMPLEX_REASONING: f64 = 0.5;
```

Raised from the current values to prevent over-promotion. Simple tasks should stay simple; REASONING is reserved for genuine reasoning needs.

### Overrides

Two hard overrides applied before tier mapping:

1. **Large context** — Estimated input tokens > 100,000 → force COMPLEX tier. Small models can't handle it.
2. **Structured output** — Request contains JSON/YAML/schema indicators → minimum tier MEDIUM. Structured output requires model capability.
3. **Reasoning keyword override** — 2+ reasoning keywords in user message → force REASONING tier with confidence >= 0.85 (matches ClawRouter behavior).

### Fallback Chains

Change `ModelRouteConfig` from a single model to an ordered list:

```yaml
model_routes:
  - hint: fast
    provider: groq
    model: llama-3.3-70b-versatile
    fallbacks:
      - provider: openrouter
        model: deepseek/deepseek-chat
      - provider: openrouter
        model: google/gemini-2.5-flash-lite
```

When the primary model fails (API error, rate limit), the router tries each fallback in order. The existing `RouterProvider.resolve()` returns the primary; a new `resolve_with_fallbacks()` returns the ordered chain.

### Context Window Filtering

Each model in the route table gets an optional `context_window` field (token count). Before routing, estimate input tokens (~4 chars per token). Filter the fallback chain to models whose context window can handle the request (with 10% buffer). If all models filtered out, use the full chain and let the API reject if needed.

```yaml
model_routes:
  - hint: fast
    provider: groq
    model: llama-3.3-70b-versatile
    context_window: 131072
    fallbacks:
      - provider: openrouter
        model: google/gemini-2.5-flash-lite
        context_window: 1048576
```

### Classification Output

The `ClassificationDecision` struct gains new fields:

```rust
pub struct ClassificationDecision {
    pub hint: String,
    pub tier: Tier,           // NEW: SIMPLE, MEDIUM, COMPLEX, REASONING
    pub confidence: f64,      // NEW: sigmoid-calibrated [0, 1]
    pub agentic_score: f64,   // NEW: [0, 1]
    pub priority: i32,
    pub signals: Vec<String>, // NEW: human-readable scoring signals
}
```

---

## Part 2: Planner/Executor Split

### Design Decisions

| # | Decision | Choice |
|---|----------|--------|
| 1 | Where does the planner live? | Above the loop — new `plan_then_execute()` function, existing `run_tool_call_loop()` untouched |
| 2 | Plan format | Free-form JSON interpreted by executor LLM, no Rust action enum |
| 3 | Who decides if planning is needed? | Three-zone system: confident non-agentic skips planner, confident agentic goes to planner, ambiguous zone lets planner decide |
| 4 | How does the executor consume the plan? | Action-by-action with result accumulation, not single-pass |
| 5 | Parallel actions? | Planner marks groups; same-group actions run in parallel, groups run sequentially |
| 6 | Planner model configuration | Reserved `hint:planner` route in existing `model_routes`; no route = no planning |
| 7 | What context does the planner receive? | System prompt + memory context + thread history + user message (no tool specs) |
| 8 | Passthrough behavior | Discard planner response, run executor from scratch via `agent_turn()` |

### Flow

```
Inbound message
    │
    ▼
hint:planner route configured?
    ├─ No  → agent_turn() [current behavior, unchanged]
    │
    └─ Yes → classify message (14 dimensions + agentic score)
                │
                ├─ agentic_score < 0.3 AND confident → agent_turn() [skip planner]
                │
                ├─ agentic_score >= 0.5 AND confident → call planner model
                │
                └─ ambiguous (0.3-0.5 or low confidence) → call planner model
                                                             (let planner decide passthrough)
                    │
                    ├─ passthrough: true → agent_turn()
                    │
                    └─ actions: [...] → action-by-action executor
                                          │
                                          ▼
                                    For each group (sequential):
                                      For each action in group (parallel):
                                        → run_tool_call_loop()
                                        → collect results
                                          │
                                          ▼
                                    Return final response
```

The three-zone approach uses the agentic score from the classifier:
- **Score < 0.3 with high confidence** — Simple/conversational. Skip planner entirely.
- **Score >= 0.5 with high confidence** — Multi-step/agentic. Go straight to planner.
- **Score 0.3-0.5 or low confidence** — Ambiguous. Call planner, let it decide passthrough.

### Plan Format

The planner outputs JSON. The schema lives in the planner system prompt, not in Rust types.

```json
{
  "passthrough": false,
  "actions": [
    {
      "group": 1,
      "type": "create_issue",
      "description": "Create sub-issue for companies page under SPO-29",
      "tools": ["linear_create_issue"],
      "params": {
        "team": "spore",
        "title": "Companies page",
        "parent": "SPO-29",
        "assignee": "ra"
      }
    },
    {
      "group": 1,
      "type": "create_issue",
      "description": "Create sub-issue for usage dashboard under SPO-29",
      "tools": ["linear_create_issue"],
      "params": { "...": "..." }
    },
    {
      "group": 2,
      "type": "reply",
      "description": "Reply in thread with links to all created issues",
      "tools": ["slack_reply"],
      "params": {
        "content_hint": "Created 4 sub-issues from SPO-29, parent remains open"
      }
    }
  ]
}
```

Field semantics:
- `passthrough` — `true` means skip planning, hand off to normal executor flow
- `group` — integer; groups execute sequentially, actions within a group execute in parallel
- `type` — free-form string; context for the executor, not matched against an enum
- `description` — primary instruction to the executor; states what to accomplish
- `tools` — list of tool names this action needs; orchestrator filters tool specs to only these
- `params` — structured hints the executor uses when calling tools
- The planner must never fabricate data (URLs, IDs); if data is unknown, it adds a prior lookup action

### Executor Orchestration

Rust code iterates over plan groups. For each action, it constructs a focused prompt and runs `run_tool_call_loop()` with a low iteration cap (3-5).

**Per-action executor input:**
- Slim system prompt (~200 tokens): execution instructions only, no persona or context
- Filtered tool specs: only the tools listed in the action's `tools` field
- The action's `description` and `params`
- Accumulated results from prior groups

**Accumulated results format:**

```
Action "create_issue" (group 1): Created issue SPO-31 "Companies page" — URL: https://linear.app/spore/issue/SPO-31
Action "create_issue" (group 1): Created issue SPO-32 "Usage dashboard" — URL: https://linear.app/spore/issue/SPO-32
```

Summaries are extracted from actual tool output, not from executor LLM prose. This prevents URL hallucination.

**Group execution:**

```
groups = plan.actions.group_by(|a| a.group).sort_by_key()
accumulated_results = []

for group in groups:
    results = parallel_execute(group, accumulated_results)
    accumulated_results.extend(results)

return last_action_output
```

### Planner System Prompt

Appended to Rain's base system prompt (the planner keeps Rain's identity and context):

- You are in planning mode. Output a JSON action plan.
- Do not call tools or write final content. Only output the plan.
- If the request is simple (direct question, single lookup, casual conversation), return `{"passthrough": true}`.
- Break multi-step tasks into discrete actions with `type`, `description`, `params`, and `tools`.
- Assign `group` numbers: independent actions share a group, dependent actions get higher numbers.
- Never fabricate data. If you need a value, add a lookup action before the action that needs it.
- Include all judgment calls in the plan. The executor follows instructions; it does not make decisions.

The planner does **not** receive tool specs. It knows tool names from the system prompt's tool list description, but doesn't need parameter schemas.

### Token Efficiency

Three mitigations prevent wasteful token usage:

**1. Slim executor prompt.** The executor receives ~200 tokens of execution instructions, not Rain's full 2k persona prompt. The planner already consumed the context; the executor does mechanical work.

**2. Filtered tool specs.** The planner's `tools` field per action lets the orchestrator send only relevant tool specs (e.g., only `linear_create_issue` for an issue creation action). Saves 3-5k input tokens per executor call.

**3. Three-zone classifier pre-filter.** The upgraded classifier's agentic score and sigmoid confidence skip planning for ~60-70% of messages (confident non-agentic). The planner handles only confident-agentic and ambiguous cases. This avoids ~4k wasted input tokens per simple message.

**Token budget comparison (4-issue example):**

Without mitigations: 5 actions × 2 iterations × 7k overhead = ~70k input tokens
With mitigations: 5 actions × 2 iterations × 2k overhead = ~20k input tokens

### Configuration

The planner uses the existing `model_routes` system. Fallback chains and context windows are new fields on all routes:

```yaml
model_routes:
  - hint: planner
    provider: openrouter
    model: anthropic/claude-sonnet-4-20250514
    context_window: 200000
  - hint: reasoning
    provider: openrouter
    model: anthropic/claude-sonnet-4-20250514
    context_window: 200000
    fallbacks:
      - provider: openrouter
        model: google/gemini-2.5-pro
        context_window: 1048576
  - hint: fast
    provider: groq
    model: llama-3.3-70b-versatile
    context_window: 131072
    fallbacks:
      - provider: openrouter
        model: deepseek/deepseek-chat
        context_window: 131072

query_classification:
  enabled: true
  mode: weighted
  scoring:
    confidence_steepness: 12.0
    confidence_threshold: 0.7
    tier_boundaries:
      simple_medium: 0.0
      medium_complex: 0.3
      complex_reasoning: 0.5
    overrides:
      max_tokens_force_complex: 100000
      structured_output_min_tier: medium
      ambiguous_default_tier: medium
    planning:
      skip_threshold: 0.3      # agentic_score below this skips planner
      activate_threshold: 0.5  # agentic_score above this activates planner
```

- `hint:planner` route present → `plan_then_execute()` is used
- `hint:planner` route absent → `agent_turn()` (current behavior, zero change)

Heartbeat and triage remain single-pass. They have their own flows and don't enter `plan_then_execute()`.

### Observability

New observer events:
- `ClassificationResult` — logs tier, confidence, agentic score, and signals
- `PlannerRequest` / `PlannerResponse` — logs the full plan for tracing
- `FallbackTriggered` — logs when a fallback model is used after primary failure
- Per-action executor calls are visible through existing `LlmRequest` / `LlmResponse` events

### Error Handling

| Scenario | Behavior |
|----------|----------|
| Planner returns invalid JSON | Fall back to `agent_turn()` (passthrough). Log parse failure. |
| Planner returns empty actions array | Treat as passthrough. |
| Action execution fails (tool error) | Log failure, add to accumulated results as `FAILED — [reason]`, continue to next action. |
| Reply action fails | Return last successful output or generic error message. |
| Accumulated results exceed token budget | Truncate older group results, keep most recent group in full. |
| Executor drifts from plan | Low `max_tool_iterations` cap (3-5) limits blast radius. Tighten per-action prompt if drift recurs. |
| Primary model fails | Router tries fallback chain in order. If all fail, return error. |
| Model can't handle context | Context window filter removes it from fallback chain before attempting. |

## Scope

**In scope:**

Part 1 — Classifier upgrade:
- 14 weighted dimensions + agentic score in `WeightedScorer`
- Sigmoid confidence calibration
- Tier overrides (large context, structured output, reasoning keyword)
- Fallback chains in `ModelRouteConfig` and `RouterProvider`
- Context window field and filtering
- Extended `ClassificationDecision` struct
- New config fields for scoring parameters, tier boundaries, and overrides

Part 2 — Planner/executor:
- `plan_then_execute()` function above the existing agent loop
- Planner system prompt construction
- Action-by-action executor orchestration with group parallelism
- Three-zone planning activation (skip / activate / ambiguous)
- `hint:planner` route convention
- Observer events for planner phase

**Out of scope:**
- Changes to `run_tool_call_loop()` internals
- Changes to tool interface or skill system
- Multi-turn planning (one plan per inbound message)
- User approval of plans before execution
- Heartbeat or triage changes
- Routing profiles (eco/premium) — existing hint system covers this
- Multilingual keywords — add when user base requires it
- LLM fallback classifier — planner serves as the smart fallback

## Files Affected

| File | Change |
|------|--------|
| `src/agent/classifier.rs` | Replace 6-dimension scorer with 14-dimension + agentic score; add sigmoid calibration, overrides |
| `src/config/schema.rs` | New fields: `ScoringConfig`, `TierBoundaries`, `OverridesConfig`, `PlanningConfig`; extend `ModelRouteConfig` with `fallbacks` and `context_window` |
| `src/providers/router.rs` | Add `resolve_with_fallbacks()`, context window filtering |
| `src/agent/planner.rs` | New module: `plan_then_execute()`, plan parsing, action orchestration |
| `src/agent/agent.rs` | Route to `plan_then_execute()` when `hint:planner` configured; use agentic score for activation |
| `src/agent/mod.rs` | Export new planner module |
| `src/observability/traits.rs` | Add `ClassificationResult`, `PlannerRequest`, `PlannerResponse`, `FallbackTriggered` events |

## Implementation Order

1. **Classifier upgrade** — Can land independently, improves routing for all users
2. **Fallback chains + context filtering** — Can land independently, improves resilience
3. **Planner/executor** — Depends on #1 for agentic score signal
