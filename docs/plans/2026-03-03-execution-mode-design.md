# Execution Mode: Classifier-Driven Integration Filtering

**Date:** 2026-03-03
**Status:** Design
**Slug:** `execution-mode`

## Problem

Query classification controls model selection but not execution strategy. Every message enters the same tool-calling loop regardless of complexity.

**Observed behavior:**

- "hey" in Slack triggers 8 tool-call iterations (24s, 9 API calls) to produce "Listening."
- "are you listening" triggers 3 planner action groups that all exhaust iteration caps (42s, 15+ API calls, empty reply).

**Root cause:** The classifier assigns a tier (simple/medium/complex/reasoning) that selects a model via `model_routes`. But the executor receives the same tool surface and iteration budget regardless of tier. The model follows system prompt instructions (e.g., "query Linear before every response") because tools are always available and it has no signal to skip them.

This problem worsens as integrations grow. Adding web access, inbox management, and other tools expands the surface the model explores on every message.

## Solution

Add integration filtering to the classifier. Every message passes through a two-stage classification:

1. **Weighted scorer** (free, instant) extracts structural signals from the message.
2. **LLM classifier** (fast model, one call) receives the message, weighted signals, and the integration catalog. It outputs the tier, agentic score, and which integrations the message needs.

The executor then loads tools only from selected integrations. Internal capabilities (memory, respond) remain always available. The planner decides how to sequence work; it never decides which integrations to include.

Separately, both iteration caps become configurable by consumer repos so operators can raise them to match their workload.

## Pipeline

```
message
  -> weighted scorer (14 dimensions, keyword/pattern, free)
  -> LLM classifier call
       inputs:  message + weighted score + signals + integration catalog
       outputs: tier, agentic_score, integrations[], reasoning
  -> ClassificationDecision {
       tier,           // Simple | Medium | Complex | Reasoning
       hint,           // model route key
       confidence,     // LLM-informed, uses weighted score as prior
       agentic_score,  // 0.0-1.0, drives planner gate
       integrations,   // Vec<String> -- selected external systems
       signals,        // merged weighted signals + LLM reasoning
     }
  -> tool registry filters to: internal tools + selected integration tools
  -> planner gate (uses agentic_score thresholds, unchanged)
       planner sees only filtered tools
  -> execute with filtered tools
```

## Key Design Decisions

### Weighted scorer feeds the LLM, not the other way around

The weighted scorer (14 dimensions: token count, code presence, reasoning markers, etc.) runs first and produces a score plus signal list. The LLM receives these as structured context. This lets the LLM skip mechanical analysis (counting tokens, detecting keywords) and focus on intent and integration relevance.

The weighted scorer does not make the final tier decision. The LLM does.

### Integration selection uses an LLM, not keywords

Keywords are brittle. "Can you check if anyone assigned me anything today" clearly needs Linear, but no keyword list catches every way someone phrases that. The LLM understands intent and selects integrations from the catalog based on what the message actually needs.

### The classifier prompt receives the integration catalog

The existing `IntegrationEntry` catalog (`src/integrations/catalog.rs`) provides names, descriptions, and categories for all available integrations. The classifier prompt includes this catalog so the LLM knows what it can select from. No new registry is needed.

### Internal vs. external tool boundary

Internal capabilities (memory, respond) are always available. They are not gated because nearly every response benefits from context. Only external integrations (Linear, Slack, web, email) are gated by classifier selection.

### Integration filtering scopes what the planner sees

The planner still outputs per-action `tools` lists that name individual tools -- this precision is how it sequences work. The change is upstream: the classifier decides which integrations are visible, and the planner only sees tools from those integrations. It cannot pick tools from integrations the classifier excluded.

The filter happens before the planner, not instead of it.

### Both iteration caps become consumer-configurable

The normal tool loop budget (`max_tool_iterations`, default 10) and the executor action budget (`MAX_EXECUTOR_ACTION_ITERATIONS`, hardcoded 15) both become config values. Consumer repos set what fits their workload:

```toml
[agent]
max_tool_iterations = 30
max_executor_action_iterations = 25
```

Zeroclaw provides sensible defaults. Consumers override as needed. No hardcoded caps remain.

### Classifier model is configurable

A new `classifier` model route lets operators choose the model for the classification call:

```toml
[[model_routes]]
hint = "classifier"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

Operators trade classification accuracy for speed, or vice versa.

## Classifier Prompt Shape

```
You are a message classifier. Given a message, a preliminary analysis,
and a list of available integrations, classify the message.

Preliminary analysis:
- Score: {weighted_score} (leans {tier_suggestion})
- Signals: {signal_list}
- Token estimate: {token_count}

Available integrations:
- linear: Project management -- tasks, sprints, issues, assignments
- web: Web search and URL fetching
- email: Inbox reading and sending
{...dynamically built from catalog}

Output JSON:
{
  "tier": "simple|medium|complex|reasoning",
  "agentic_score": 0.0-1.0,
  "integrations": ["linear"],
  "reasoning": "one sentence"
}
```

## What Changes

| Component | Change |
|-----------|--------|
| `ClassificationDecision` | Add `integrations: Vec<String>` field |
| `classify_with_context()` | After weighted scoring, make LLM call with weighted signals + catalog |
| Tool registry (`src/tools/mod.rs`) | Filter integration tools by classifier output before passing to executor |
| `MAX_EXECUTOR_ACTION_ITERATIONS` | Move from constant to config field |
| Config schema | Add `max_executor_action_iterations`, `classifier` model route |
| Planner tool visibility | Planner receives only tools from classifier-selected integrations |

## What Does Not Change

| Component | Reason |
|-----------|--------|
| Weighted scorer dimensions | Still useful as free feature extraction for the LLM |
| Planner gate logic | Still uses `agentic_score` with `skip_threshold`/`activate_threshold` |
| Executor structure | Action groups, parallel execution, context passing all unchanged |
| Model routing | `hint` -> `model_routes` mapping unchanged |
| Integration trait | `name()`, `tools()`, `health_check()` unchanged |
| `collect_integrations()` | Still collects all configured integrations; filtering happens downstream |
| Integration catalog | Already has names and descriptions; classifier prompt reads from it |

## Example Flows

**"hey" in Slack:**
- Weighted scorer: score -0.02 (simple_indicators), agentic signals low
- LLM classifier: tier=simple, agentic_score=0.05, integrations=[]
- Planner gate: score below skip_threshold, skip planner
- Tool loop: internal tools only (memory, respond), no integration tools loaded
- Result: model reads memory for context, responds in 1-2 iterations

**"what's on the sprint this week":**
- Weighted scorer: score 0.08 (imperative verb, medium complexity)
- LLM classifier: tier=medium, agentic_score=0.35, integrations=["linear"]
- Planner gate: depends on threshold config (Rain: skip at 0.6, so skipped)
- Tool loop: internal tools + Linear tools available
- Result: model queries Linear issues, responds in 3-5 iterations

**"research competitor pricing and draft a summary with action items":**
- Weighted scorer: score 0.45 (multi-step, imperative, high complexity)
- LLM classifier: tier=complex, agentic_score=0.75, integrations=["web", "linear"]
- Planner gate: score above activate_threshold, planner runs
- Planner outputs action groups (research, synthesize, create tasks)
- Executor: each action gets internal tools + web + Linear tools
- Result: full pipeline, generous iteration budget, converges without cap issues

## Full Decision Tree

Every inbound message follows this path. Existing steps are marked; new steps introduced by this design are marked with **(new)**.

```
1. MESSAGE ARRIVES (from channel)
   │
2. WEIGHTED SCORER [existing]
   │  14-dimension keyword/pattern analysis (free, no API call)
   │  Outputs: score (float), signals (list), token_estimate
   │
3. LLM CLASSIFIER CALL [new]
   │  Model: classifier_model (configurable route)
   │  Inputs: message + weighted score + signals + integration catalog
   │  Outputs: tier, agentic_score, integrations[], reasoning
   │
4. MODEL SELECTION [existing]
   │  tier -> hint -> model_routes -> resolved model (lookup, no API call)
   │
5. TOOL FILTERING [new]
   │  Builds tool set: internal tools (always) + tools from selected integrations
   │  This filtered set is locked for the rest of the message lifecycle.
   │
6. PLANNER GATE [existing]
   │  Compares agentic_score against skip_threshold / activate_threshold
   │
   ├── agentic_score < skip_threshold
   │   │
   │   └── 7a. NORMAL TOOL LOOP [existing]
   │           Model: resolved model from step 4
   │           Tools: filtered set from step 5
   │           Budget: max_tool_iterations (configurable)
   │           Iterates until model responds with no tool calls
   │
   └── agentic_score >= activate_threshold
       │
       7b. PLANNER CALL [existing]
       │   Model: planner_model (separate route, no tools)
       │   Sees: message + context + filtered tool names from step 5
       │   Outputs: passthrough (bool) + action groups with per-action tool lists
       │
       ├── passthrough: true
       │   └── Falls through to 7a (normal tool loop)
       │
       └── passthrough: false
           │
           8. EXECUTOR [existing]
              For each action group (sequential):
                For each action in group (parallel):
                  Model: resolved model from step 4
                  Tools: subset of filtered set, scoped by planner's per-action tool list
                  Budget: max_executor_action_iterations (configurable)
              Collects results, passes to next group as context
   │
9. RESPONSE returned to channel
```

### Decision summary

| Step | Decider | Decision | Input |
|------|---------|----------|-------|
| 2 | Weighted scorer | Preliminary score + signals | Message text (keywords, patterns) |
| 3 | LLM classifier | Tier, agentic_score, integrations | Message + weighted output + catalog |
| 4 | Config lookup | Which model runs the work | Tier -> hint -> model_routes |
| 5 | Filter logic | Which tools exist for this message | Classifier's integration list |
| 6 | Threshold check | Plan or go direct | agentic_score vs config thresholds |
| 7b | Planner LLM | How to sequence work, which tools per action | Message + filtered tool list |

## Risk

**LLM classifier adds latency.** Every message pays ~200-500ms for the classification call. This cost is offset by reduced downstream iterations -- a message that previously burned 8 tool-call iterations now converges in 1-2 because irrelevant tools are absent.

**LLM classifier can misclassify integrations.** If the classifier omits a needed integration, the agent cannot complete the task. Mitigation: the `reasoning` field in classifier output makes misclassifications debuggable. Operators can also tune via the classifier model choice.

**Weighted scorer keywords can mislead the LLM.** If the weighted scorer flags "sprint" as a simple_indicator (it doesn't, but hypothetically), the LLM might underweight it. Mitigation: the LLM receives signals as context, not as binding constraints. It can override.
