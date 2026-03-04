# Planner Module Design

**Date:** 2026-03-04
**Slug:** `planner-module`
**Status:** Draft

## Problem

Cron-scheduled rituals (standup, sweep) execute as flat tool-call loops: the agent
gets a big prompt, calls tools until it decides to stop, and exits. The sweep
completed in 10 tool calls over 2 minutes — creating 2 issues and posting a summary,
but skipping enrichment, duplicate scanning, dedup guards, and likely only reading 1
Slack channel.

The root cause: `run_agent_job()` calls `crate::agent::run()`, which runs a flat
loop. A `plan_then_execute()` system exists in `src/agent/planner.rs` but it is
buried inside the agent module, conditionally gated behind agentic-score thresholds,
and structurally incomplete (no synthesis step, no per-action model routing).

The gating mechanism itself is broken: a keyword-counting classifier
(`score_agentic_task`) produces an `agentic_score` checked against a three-zone
threshold system where two of the three zones execute identical code. Ritual prompts
like "Run the daily sweep" score 0.0–0.2 and skip the planner entirely.

## Decision

Implement Anthropic's recommended orchestrator-worker pattern as a first-class
`src/planner/` module. The planner becomes THE execution engine for all paths —
cron, channel orchestrator, and CLI agent.

Delete the keyword-based classifier's planner-gating role, the three-zone
agentic-score activation logic, and `PlanningConfig` thresholds. The planner itself
is the gate: it receives every message and returns `passthrough` for simple queries
or a structured plan for complex ones. One LLM call (fast model) replaces the
classifier + conditional planner — same cost, simpler code, no misclassification.

### Design Principles (Anthropic-aligned)

Source: [Building Effective Agents](https://www.anthropic.com/research/building-effective-agents),
[Multi-Agent Research System](https://www.anthropic.com/engineering/multi-agent-research-system),
[Orchestrator Workers Cookbook](https://github.com/anthropics/anthropic-cookbook/blob/main/patterns/agents/orchestrator_workers.ipynb)

1. **Orchestrator decomposes, workers execute.** The planner LLM (no tools) produces
   structured decomposition. Workers execute scoped actions with scoped tool sets.
2. **Workers operate independently.** Actions within a group share no state and run
   in parallel. Results flow back to the orchestrator, not between workers.
3. **Effort scaling.** The planner prompt embeds complexity heuristics: simple queries
   get passthrough, moderate tasks get 1–3 actions, complex rituals get 5+ grouped
   actions. Prevents overinvestment in simple queries.
4. **Anti-duplication.** The planner assigns non-overlapping responsibilities per
   action. No two actions in the same group touch the same tool or data source.
5. **Start wide, then narrow.** Early groups do broad information gathering. Later
   groups narrow focus based on gathered results.
6. **Synthesize results.** A final LLM pass combines all action results into a
   coherent summary — not just the last action's raw output.
7. **Per-action model routing.** The planner specifies `model_hint` per action so
   complex reasoning actions get a powerful executor while simple tool calls use a
   fast model.

## Architecture

### Module Structure

```
src/planner/
├── mod.rs              — public API, re-exports
├── types.rs            — Plan, PlanAction, ActionResult, PlanExecutionResult
├── parser.rs           — JSON/fenced extraction from LLM responses
├── orchestrator.rs     — plan_then_execute(): plan → execute → synthesize
├── prompts.rs          — planner system prompt, executor prompt, synthesis prompt
└── runtime.rs          — PlannerRuntime: provider, tools, observer construction
```

### Execution Flow

```
┌──────────────┐   ┌──────────────────┐   ┌──────────────────┐
│ Cron          │   │ Channel          │   │ CLI Agent        │
│ scheduler     │   │ orchestrator     │   │                  │
└──────┬───────┘   └────────┬─────────┘   └────────┬─────────┘
       │                    │                       │
       └────────────────────┼───────────────────────┘
                            │
                    ┌───────▼────────┐
                    │ src/planner/   │
                    │ PlannerRuntime │
                    │                │
                    │ plan()         │   ← Phase 1: Decompose (or passthrough)
                    │ execute()      │   ← Phase 2: Workers (parallel within group)
                    │ synthesize()   │   ← Phase 3: Combine results
                    └───────┬────────┘
                            │
                    ┌───────▼────────┐
                    │ run_tool_call  │   ← Implementation detail
                    │ _loop()        │      of worker execution
                    └────────────────┘
```

All callers go through `PlannerRuntime`. The planner IS the gate:
- Simple query → planner returns `{"passthrough": true}` → single-action execution
  via `run_tool_call_loop()` with full tool set
- Complex task → planner returns structured plan → grouped parallel execution

No separate classifier gate. No separate flat-loop codepath. One engine.

### Three-Phase Execution

**Phase 1 — Plan (Orchestrator LLM, no tools):**

The planner model (always fast/cheap, from `model_routes` hint `"planner"`) receives
the task and decides: passthrough or plan.

Planner prompt includes effort scaling heuristics:

```
Assess the complexity of the request:
- Simple (greeting, single question, casual conversation): return {"passthrough": true}
- Moderate (1-3 tool calls needed, single concern): 1-3 actions, single group
- Complex (multi-step, multiple data sources, dependencies): 3-10 actions, multiple groups
- Ritual/sweep (structured multi-phase workflow): 5+ actions, ordered groups

Scale effort to match complexity. Do not over-plan simple tasks.
```

Planner output format:

```json
{
  "analysis": "The sweep requires reading all Slack channels, enriching with thread
    context, deduplicating against existing Linear issues, creating new issues, and
    posting a summary. Phases 1-2 are independent reads. Phase 3 depends on both.
    Phases 4-5 depend on phase 3.",
  "passthrough": false,
  "actions": [
    {
      "group": 1,
      "type": "read_slack_channels",
      "description": "Read messages from all configured Slack channels for the last 24h",
      "tools": ["slack_read_messages"],
      "params": {"timeframe": "24h"},
      "model_hint": "fast"
    },
    {
      "group": 1,
      "type": "read_existing_issues",
      "description": "Fetch all open Linear issues to check for duplicates",
      "tools": ["linear_issues"],
      "params": {},
      "model_hint": "fast"
    },
    {
      "group": 2,
      "type": "enrich_and_dedup",
      "description": "Enrich Slack messages with thread context, identify duplicates
        against existing issues from group 1. Each message gets a dedup verdict.",
      "tools": ["slack_read_thread"],
      "params": {},
      "model_hint": "reasoning"
    },
    {
      "group": 3,
      "type": "create_issues",
      "description": "Create Linear issues for non-duplicate items identified in group 2",
      "tools": ["linear_create_issue"],
      "params": {},
      "model_hint": "fast"
    },
    {
      "group": 4,
      "type": "post_summary",
      "description": "Post sweep summary to Slack with links to all created issues",
      "tools": ["slack_post_message"],
      "params": {},
      "model_hint": "fast"
    }
  ]
}
```

**Phase 2 — Execute (Workers):**

Actions grouped by `group` field. Groups execute sequentially. Actions within a group
execute in parallel via `join_all`.

Each worker gets:
- A scoped system prompt (action type, description, params)
- The planner's `analysis` as context (awareness of broader plan)
- Accumulated results from prior groups (URLs, IDs, counts)
- A scoped tool set (only tools listed in the action's `tools` field)
- A model resolved from `model_hint` against `model_routes` config

Anti-duplication: the planner assigns non-overlapping tool/data-source
responsibilities. The executor enforces this via tool filtering — each action only
sees the tools it was assigned.

Workers use `run_tool_call_loop()` internally — this remains as an implementation
detail of the planner, not an alternative codepath.

**Phase 3 — Synthesize (LLM, no tools):**

After all action groups complete, a final LLM call produces a coherent summary.

The synthesizer receives:
- The original user prompt
- The planner's analysis
- All accumulated action results (with success/failure status)

It produces a clear, factual summary with concrete outputs (URLs, counts, failures).

Skip synthesis when:
- Only one action succeeded — raw output is the summary
- Passthrough — no plan was executed

### Data Types

```rust
// types.rs

#[derive(Debug, Clone, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub analysis: Option<String>,
    #[serde(default)]
    pub passthrough: bool,
    #[serde(default)]
    pub actions: Vec<PlanAction>,
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

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub action_type: String,
    pub group: u32,
    pub success: bool,
    pub summary: String,
    pub raw_output: String,
}

pub enum PlanExecutionResult {
    /// Planner deemed the task simple — caller runs it through the flat tool loop.
    Passthrough,
    /// Plan was executed action-by-action with synthesized output.
    Executed {
        output: String,
        action_results: Vec<String>,
        analysis: Option<String>,
    },
}
```

### PlannerRuntime

```rust
// runtime.rs

pub struct PlannerRuntime {
    pub provider: Box<dyn Provider>,
    pub tools: Vec<Box<dyn Tool>>,
    pub tool_specs: Vec<ToolSpec>,
    pub observer: Arc<dyn Observer>,
    pub planner_model: String,
    pub executor_model: String,
    pub model_routes: HashMap<String, String>,
    pub temperature: f64,
    pub max_tool_iterations: usize,
    pub max_executor_iterations: usize,
}

impl PlannerRuntime {
    pub fn from_config(config: &Config) -> Result<Self>;
    pub async fn plan_then_execute(&self, ...) -> Result<PlanExecutionResult>;
}
```

`PlannerRuntime::from_config()` extracts the provider, tools, observer, and model
construction that currently lives in `Agent::from_config()`. The `Agent` struct wraps
`PlannerRuntime` and adds conversation history, REPL, and prompt building.

### Model Resolution

- **Planner model:** Always fast. Resolved from `model_routes` hint `"planner"`.
  The planner does structural decomposition, not deep reasoning.
- **Executor model per action:** Resolved from the action's `model_hint` against
  `model_routes`. If no hint or hint not found, falls back to the default executor
  model (from job config or `default_model`).
- **Synthesis model:** Uses the planner model (fast, no tools needed).

### Cron Wiring

`run_agent_job()` calls `PlannerRuntime::from_config()` and
`runtime.plan_then_execute()` directly. No more `agent::run()` intermediary.

```rust
async fn run_agent_job(config: &Config, security: &SecurityPolicy, job: &CronJob) -> (bool, String) {
    // ... security checks (unchanged) ...

    let runtime = PlannerRuntime::from_config(config)?;
    let executor_model = job.model.clone()
        .or_else(|| config.cron.model.clone())
        .unwrap_or(runtime.executor_model.clone());

    let result = runtime.plan_then_execute(
        &executor_model,
        &prefixed_prompt,
        "",   // system prompt
        "",   // memory context
        "cron",
        None, // cancellation token
        None, // hooks
        &[],  // excluded tools
    ).await;

    match result {
        Ok(PlanExecutionResult::Executed { output, .. }) => (true, output),
        Ok(PlanExecutionResult::Passthrough) => {
            // Passthrough: run through flat tool loop (simple task)
            fallback_flat_run(config, &prefixed_prompt, &executor_model).await
        }
        Err(e) => (false, format!("agent job failed: {e}")),
    }
}
```

### Passthrough Handling

When the planner returns `Passthrough`, the caller runs the message through the flat
`run_tool_call_loop()` with the full tool set — same as the current behavior for
simple messages. This is not a "fallback" in the error sense; it is the intended path
for simple queries. The planner decided no planning was needed.

For cron jobs, passthrough should be rare — ritual prompts are inherently multi-step.
If a cron job hits passthrough, it means the planner model judged it simple enough
for a single-pass execution, which is a valid outcome.

## What Gets Deleted

1. **`src/agent/planner.rs`** — fully superseded by `src/planner/`
2. **Three-zone agentic-score gate** in `agent.rs:593-666` — the planner IS the gate
3. **`PlanningConfig`** (`skip_threshold`, `activate_threshold`) — no longer needed
4. **`score_agentic_task()`** keyword classifier — replaced by planner's own
   passthrough decision
5. **`resolve_planner_model()`** in orchestrator — moved to `PlannerRuntime`
6. **Conditional planner invocation** in `orchestrator.rs:1454-1514` — replaced by
   direct `PlannerRuntime` call
7. **`last_agentic_score` field** on `Agent` — no longer used for planner gating

Note: the classifier's **model routing** (hint selection) and **integration
filtering** roles are separate from planner gating. Model routing is absorbed by the
planner's per-action `model_hint`. Integration filtering can move into the planner's
analysis or remain as a lightweight pre-step.

## What Stays

- `run_tool_call_loop()` in `src/agent/loop_.rs` — used by workers and passthrough
- `QueryClassificationConfig` — retains model routing and integration filtering roles
  (but loses planner gating role)
- Security checks in `run_agent_job()` — unchanged
- Delivery mechanism in scheduler — unchanged
- CronJob/CronConfig schema — unchanged
- `model_routes` config — unchanged, now also consumed for per-action routing
- Observability events — preserved and extended with `analysis` field

## Anthropic Alignment Verification

| Anthropic Guideline | Our Design | Status |
|---------------------|-----------|--------|
| Orchestrator decomposes, workers execute | Plan phase (no tools) → Execute phase (scoped workers) | Aligned |
| Workers operate independently | Parallel within group, no shared state | Aligned |
| Workers return to lead, not each other | Results accumulated by orchestrator | Aligned |
| Synthesis of results | Phase 3 synthesizes all results | Aligned |
| Effort scaling in prompts | Planner prompt embeds complexity heuristics | Aligned |
| Anti-duplication guidance | Planner assigns non-overlapping responsibilities | Aligned |
| Start wide, then narrow | Early groups gather broadly, later groups narrow | Aligned |
| Scoped tool sets per worker | Action `tools` field restricts available tools | Aligned |
| Handle failures gracefully | Action failures logged, execution continues | Aligned |
| Per-action model routing | `model_hint` resolves against `model_routes` | Aligned |
| Transparency | `analysis` field logged to observability | Aligned |
| Start simple first | Planner returns passthrough for simple queries | Aligned |

## Migration Path

1. Create `src/planner/` module with types, parser, prompts from `src/agent/planner.rs`
2. Add `PlannerRuntime` struct with `from_config()` extracted from `Agent::from_config()`
3. Add synthesis phase to orchestrator
4. Add `model_hint` to `PlanAction` with route resolution
5. Add effort scaling and anti-duplication to planner prompt
6. Rewire `run_agent_job()` to use `PlannerRuntime` directly
7. Rewire channel orchestrator to use `PlannerRuntime` directly
8. Refactor `Agent` to wrap `PlannerRuntime` (delegates execution)
9. Delete `src/agent/planner.rs`, three-zone gate, `PlanningConfig`, `score_agentic_task`
10. Update imports across codebase

## Testing Strategy

- Move existing planner unit tests to `src/planner/`
- New: synthesis prompt produces coherent output from action results
- New: `PlannerRuntime::from_config()` constructs valid runtime
- New: per-action `model_hint` resolves against `model_routes`
- New: passthrough for simple queries ("hi", "what time is it")
- New: multi-group plan for complex prompts (ritual-style)
- New: effort scaling — simple prompt gets 1 action, complex gets 5+
- New: cron job goes through plan → execute → synthesize
- Existing scheduler tests unchanged (shell jobs, security, delivery)

## Risks

- **Medium:** Extracting runtime construction from `Agent::from_config()` touches a
  critical path. Mitigation: Agent wraps PlannerRuntime, single source of truth.
- **Low:** Every message pays the planner call cost (~1 fast LLM call). This replaces
  the classifier LLM call — same cost, not additional.
- **Low:** Planner model might return poor plans for unusual prompts. Mitigation:
  passthrough is the safe default; bad plans are worse than no plan.

## Rollback

Revert to the commit before the planner module extraction. The old
`src/agent/planner.rs` and three-zone gate are restored via git history.
