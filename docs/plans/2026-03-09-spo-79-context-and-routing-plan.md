# Context Engineering & Routing — Design & Implementation Plan

**Date:** 2026-03-09
**Issue:** [SPO-79](https://linear.app/netspore/issue/SPO-79)
**Problem statement:** `docs/plans/2026-03-09-spo-79-context-and-routing-design.md`
**Follow-up:** [SPO-81](https://linear.app/netspore/issue/SPO-81) — tool result formatting audit

---

## Approved Design Decisions

### Ordering

1. P5 — Idempotent cron add (tiny, isolated)
2. P3 — Inline cron context + clean fallback (small, isolated)
3. P2 — Tool result clearing (medium, foundational)
4. P1+P4 — Planner routing + sub-agent delegation (main event)

P3 and P5 are prerequisites: they remove noise that would mask sub-agent improvements. P2 is foundational: it benefits both the current flat loop and the future sub-agent architecture.

### P5 — Idempotent Cron Add

- Job `name` is the unique key.
- `cron add` with an existing name upserts (updates schedule, prompt, model, context_files).
- `name` is required for agent jobs.
- Return value indicates created vs. updated.
- **Affected code:** `src/cron/store.rs` — add `find_by_name()`, modify `add_agent_job()` / `add_shell_job()` to check-then-upsert.

### P3 — Inline Cron Context + Clean Fallback

**3a — Context files:**
- New `context_files: Vec<String>` field on cron jobs (stored as JSON array in SQLite).
- `cron add` accepts `--context-file <path>` (repeatable).
- At execution time, `run_agent_job()` reads each file and prepends to the prompt as `## Context: <filename>\n<content>\n`.

**3b — Clean fallback:**
- When `plan_then_execute()` fails, the flat run fallback starts with a completely fresh context: original prompt + inlined context files only.
- No history carried from the failed planner attempt.
- A failed fallback counts as the job's final failure — no re-entry into the planner.

**Affected code:** `src/cron/store.rs` (schema + migration), `src/cron/scheduler.rs` (`run_agent_job`).

### P2 — Tool Result Clearing

- Turn-based clearing in the tool call loop.
- After each assistant response, scan history for tool results older than N assistant turns.
- Replace stale results with: `[Cleared: {tool_name} returned {byte_count} bytes]`
- Default TTL: 3 assistant turns. Configurable via `agent.tool_result_ttl`.
- The most recent tool result is never cleared regardless of TTL.
- Only applies within `run_tool_call_loop()`. Complements (does not replace) history compaction at 50 messages.
- **Affected code:** `src/agent/loop_.rs` — new `clear_stale_tool_results()` function called at top of each loop iteration.

### P1+P4 — Routing + Sub-Agent Delegation

**Routing architecture:**

```
Message arrives
    |
    v
Classifier (existing 14-dim + optional LLM refinement)
    |
    +-- Tier::Simple, confidence >= 0.8
    |       -> Fast path: flat loop, max 3 iterations
    |       -> If budget exhausted: escalate to planner
    |
    +-- Cron ritual (detected by scheduler)
    |       -> Pre-structured plan from .plan.toml
    |       -> Orchestrator executes directly, no planner LLM
    |
    +-- Medium / Complex / Reasoning (or Simple < 0.8)
            -> Planner receives classifier signals
            -> Planner produces action groups with delegation
            -> Orchestrator executes plan
```

**Separation of concerns:**
- Classifier owns "what kind of message is this?"
- Planner owns "how should we handle this complex message?" — it never re-classifies.
- Scheduler owns "this is a known ritual, here's the pre-built plan."

**Classifier-to-planner handoff:**
- Planner prompt receives: `tier`, `agentic_score`, `integrations`, `signals` from classifier.
- New config: `agent.simple_routing_confidence = 0.8`, `agent.simple_max_iterations = 3`.

**Pre-structured ritual plans:**
- New file convention: `<skill>.plan.toml` alongside `<skill>.md`.
- Scheduler reads `.plan.toml` -> deserializes into existing `Plan` struct.
- Scheduler reads `.md` -> inlines as context for synthesis actions.
- Hands `Plan` to orchestrator executor, skipping planner phase.

**Unified plan interface:**
- The existing `Plan` struct is the interface. Two producers:
  - Planner LLM (for complex interactive messages)
  - Scheduler (for cron rituals, from `.plan.toml` files)
- Orchestrator is the single consumer. No new types needed.

**Plan file format (`<skill>.plan.toml`):**

```toml
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
```

**Sub-agent execution:**
- Group 1 actions with `action_type = "delegate"` run as parallel sub-agents via existing delegate tool infrastructure.
- Each sub-agent gets its own context window (isolated from main loop).
- Inter-group compression (existing 3000 char limit) passes summaries to group 2.
- Group 2 synthesis action receives compressed gather results + skill file content.

**Template variables:**
- Scheduler resolves `{{date}}`, `{{job_name}}` in action prompts at execution time.
- Minimal set — no complex templating engine.

**Escalation from fast path:**
- If the simple fast path (3 iterations) exhausts its budget without completing, escalate to the planner with accumulated context.

**Affected code:**
- `src/agent/loop_.rs` — routing logic before planner invocation
- `src/planner/orchestrator.rs` — accept classifier signals in planner prompt
- `src/planner/types.rs` — TOML deserialization for `Plan`
- `src/cron/scheduler.rs` — plan file loading, template resolution, direct orchestrator invocation

---

## Validated Against

- **Anthropic "Building Effective Agents":** Routing as prerequisite pattern, orchestrator-worker for parallel subtasks, "add sub-agents only when it demonstrably improves outcomes."
- **Anthropic "Effective Context Engineering":** Tool result clearing as "safest, lightest-touch compaction."
- **LangGraph orchestrator-worker pattern:** Workers with own state, parallel execution, orchestrator synthesizes. "Each worker operates with its own distinct state."

Our architecture aligns on all three: classifier-based routing before delegation, parallel isolated sub-agents for gather, orchestrator synthesis of compressed results.

---

## Implementation Plan

### Step 1: Idempotent Cron Add

**Files:** `src/cron/store.rs`, relevant CLI handler for `cron add`

1. Add `find_by_name(name: &str) -> Option<CronJob>` to cron store.
2. Modify `add_agent_job()`: if a job with the same name exists, update its fields (schedule, prompt, model, etc.) instead of inserting. Return an enum `Created | Updated`.
3. Same for `add_shell_job()`.
4. Make `name` required for agent jobs (validate at CLI level).
5. Tests: add job, re-add with same name (verify upsert), add with different name (verify new row).

### Step 2: Context Files Field on Cron Jobs

**Files:** `src/cron/store.rs`, `src/cron/scheduler.rs`, CLI handler

1. Add `context_files: Vec<String>` column to `cron_jobs` table (JSON text, default `"[]"`). Write migration.
2. Add `--context-file <path>` flag to `cron add` CLI (repeatable).
3. Store paths in the new column on insert/upsert.
4. In `run_agent_job()`, before invoking planner: read each context file, prepend as `## Context: <filename>\n<content>\n` to the prompt.
5. Tests: create job with context files, verify files are read and prepended at execution time. Verify missing file produces a clear error.

### Step 3: Clean Fallback Context

**Files:** `src/cron/scheduler.rs`

1. In `run_agent_job()`, when `plan_then_execute()` returns error or malformed result: build a fresh prompt (original prompt + inlined context files).
2. Call flat agent loop with the fresh prompt only — do not pass any history from the failed planner attempt.
3. If the flat fallback also fails, record as job failure. Do not retry the planner.
4. Tests: simulate planner failure, verify fallback uses fresh context. Simulate both planner and fallback failure, verify no retry loop.

### Step 4: Tool Result Clearing

**Files:** `src/agent/loop_.rs`, `src/config/schema.rs`

1. Add `tool_result_ttl: Option<u32>` to agent config (default `3`).
2. Implement `clear_stale_tool_results(history: &mut Vec<ChatMessage>, ttl: u32)`:
   - Walk history backwards, count assistant messages.
   - For each tool result message older than `ttl` assistant turns: replace content with `[Cleared: {tool_name} returned {byte_count} bytes]`.
   - Never clear the most recent tool result.
3. Call `clear_stale_tool_results()` at the top of each iteration in `run_tool_call_loop()`.
4. Tests: build a history with N tool results and M assistant messages, verify correct results are cleared at various TTL values. Verify most-recent is preserved.

### Step 5: Classifier-Based Fast Path Routing

**Files:** `src/agent/loop_.rs`, `src/config/schema.rs`

1. Add config: `agent.simple_routing_confidence = 0.8`, `agent.simple_max_iterations = 3`.
2. In the main agent entry point (before planner invocation): check classifier output.
3. If `tier == Simple && confidence >= threshold`: run flat loop with `max_iterations = simple_max_iterations`. Skip planner.
4. If fast path exhausts budget: escalate to planner with accumulated context.
5. Tests: mock classifier returning Simple/high-confidence, verify planner is skipped and iteration cap is enforced. Mock budget exhaustion, verify escalation to planner.

### Step 6: Classifier Signals to Planner

**Files:** `src/planner/orchestrator.rs`, `src/agent/loop_.rs`

1. Pass `ClassificationDecision` (tier, agentic_score, integrations, signals) to the planner prompt as structured context.
2. Update planner prompt template to include: "The classifier assessed this message as {tier} with agentic_score {score}. Relevant integrations: {integrations}. Use this to inform your plan structure."
3. Tests: verify planner prompt includes classifier signals. Integration test: complex message with high agentic_score produces a multi-group plan (not passthrough).

### Step 7: Plan File Loading for Cron Rituals

**Files:** `src/cron/scheduler.rs`, `src/planner/types.rs`

1. Add TOML deserialization support to `Plan` / `PlanAction` (derive or manual `serde` impl).
2. In scheduler: when executing an agent job, check for `<skill>.plan.toml` alongside the skill `.md` file.
3. If plan file exists: deserialize into `Plan`, read `.md` as synthesis context, hand directly to orchestrator executor (skip planner phase).
4. If no plan file: use current flow (planner LLM).
5. Implement minimal template variable resolution: `{{date}}` -> current date, `{{job_name}}` -> job name.
6. Tests: create a `.plan.toml` fixture, verify it deserializes into a valid `Plan`. Verify template variables are resolved. Verify missing plan file falls through to planner.

### Step 8: Sub-Agent Execution in Action Groups

**Files:** `src/planner/orchestrator.rs`

1. For actions with `action_type = "delegate"`: execute via the delegate tool infrastructure instead of direct tool calls.
2. Each delegate action gets: its own context window (prompt from plan action), filtered tool set (from plan action `tools`), model hint, iteration budget.
3. Group 1 delegate actions run in parallel (existing `join_all` machinery).
4. Inter-group compression passes summaries to group 2 (existing `compress_accumulated_lines`).
5. Group 2 synthesis action receives compressed results + skill file content as context.
6. Tests: create a 2-group plan with delegate actions, verify parallel execution in group 1, verify compressed handoff to group 2, verify synthesis receives skill content.

### Step 9: End-to-End Validation

1. Create a test `.plan.toml` for a standup ritual with gather + synthesize groups.
2. Run the full path: scheduler -> plan file load -> orchestrator -> parallel sub-agents -> compression -> synthesis.
3. Verify latency improvement vs. current flat loop (target: < 30s for standup, down from 60-100+s).
4. Verify simple interactive messages ("mark SPO-74 done") complete in < 10s via fast path.
5. Verify complex interactive messages route through planner with classifier signals.

---

## Risk and Rollback

| Step | Risk | Rollback |
|------|------|----------|
| 1 (idempotent cron) | Low — additive DB change | Revert migration, old behavior resumes |
| 2 (context files) | Low — new column with default | Drop column, cron works without inlining |
| 3 (clean fallback) | Low — changes error path only | Revert to passing full history on fallback |
| 4 (tool result clearing) | Medium — could clear results still needed | Set `tool_result_ttl` to a high value or disable (None) |
| 5-6 (routing + signals) | Medium — classifier confidence thresholds may need tuning | Set `simple_routing_confidence` to 1.0 to disable fast path |
| 7-8 (plan files + sub-agents) | Higher — new execution path for rituals | Remove `.plan.toml` files to fall back to planner LLM |

Each step is independently deployable and reversible. No step depends on a later step being present.
