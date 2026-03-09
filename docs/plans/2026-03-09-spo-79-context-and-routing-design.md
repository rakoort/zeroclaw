# Context Engineering & Routing — Design

**Date:** 2026-03-09
**Issue:** [SPO-79](https://linear.app/netspore/issue/SPO-79)
**Scope:** zeroclaw runtime changes
**Companion:** spore-pm [SPO-80](https://linear.app/netspore/issue/SPO-80) covers config/prompt-level changes
**Follow-up:** [SPO-81](https://linear.app/netspore/issue/SPO-81) — tool result formatting audit
**Plan:** `2026-03-09-spo-79-context-and-routing-plan.md`

## Background

Rain (our PM agent on zeroclaw) processes messages via Slack and runs 4 daily cron rituals. Analysis of production logs (2026-03-09) reveals that the runtime architecture has clear bottlenecks that degrade response latency and reliability. Interactive replies take 19–101 seconds. Cron rituals take 60–100+ seconds. The root causes are in the zeroclaw runtime, not in the prompts.

This document identifies the specific runtime problems and the architectural decisions to fix them, informed by Anthropic's published guidance on building effective agents ("Building Effective Agents", "Effective Context Engineering for AI Agents") and LangGraph's orchestrator-worker patterns.

## Guiding Principle

From Anthropic: "Success in the LLM space isn't about building the most sophisticated system. It's about building the right system for your needs." Every change here reduces complexity or holds it constant while improving outcomes. No new abstractions unless they replace existing ones.

---

## Problem 1: The Planner Always Returns Passthrough

### What happens now

The 3-phase planner (plan → execute → synthesize) exists in `src/planner/orchestrator.rs`, but in practice the planner LLM returns `passthrough: true` on nearly every message. This means every message — whether "mark SPO-74 done" or "plan next sprint" — enters the same flat tool call loop with all tools available.

### Why this matters

- A simple status update ("SPO-74 is done") triggers 15+ sequential LLM roundtrips because the flat loop has no concept of message complexity
- Every roundtrip sends the full growing conversation context
- The 14-dimension classifier in `src/agent/classifier.rs` scores messages but the planner ignores the classification

### Decision: Classifier-based routing with planner bypass

The classifier already runs before the planner. Instead of making the planner smarter, we route deterministically based on classifier output:

- **Simple messages** (`Tier::Simple`, confidence >= 0.8): skip the planner entirely, run a tight flat loop (max 3 iterations). If budget exhausts, escalate to planner with accumulated context.
- **Complex messages** (`Medium`/`Complex`/`Reasoning`, or `Simple` < 0.8): planner receives classifier signals (`tier`, `agentic_score`, `integrations`, `signals`) and produces action groups. The planner never re-classifies.

Separation of concerns:
- **Classifier** owns "what kind of message is this?"
- **Planner** owns "how should we handle this complex message?"
- **Scheduler** owns "this is a known ritual, here's the pre-built plan"

Config: `agent.simple_routing_confidence = 0.8`, `agent.simple_max_iterations = 3`.

---

## Problem 2: No Tool Result Clearing / Mid-Loop Compaction

### What happens now

History compaction triggers at 50 non-system messages (`src/agent/loop_.rs`). Within a single interaction, the tool call loop accumulates raw tool results in the conversation context. By the time the loop reaches iteration 15–20, the model is reasoning over a massive context full of stale Linear API responses, Slack message dumps, and file contents that were only needed for earlier steps.

### Why this matters

Anthropic identifies "context rot" — model recall degrades as token count increases. Logs show `contents_count` reaching 55 in a single interaction.

### Decision: Turn-based clearing with one-line summaries

After each assistant response, scan history for tool results older than N assistant turns. Replace with a one-line summary.

- **Trigger:** turn-based (not token-budget-based). Simpler, predictable.
- **TTL:** 3 assistant turns (configurable via `agent.tool_result_ttl`).
- **Summary format:** `[Cleared: {tool_name} returned {byte_count} bytes]` — preserves tool name and scale without token cost.
- **Guard:** the most recent tool result is never cleared regardless of TTL.
- **Scope:** only within `run_tool_call_loop()`. Complements (does not replace) history compaction at 50 messages.
- **No LLM-generated summaries** — avoids added latency and failure points.

---

## Problem 3: Cron Job Planner Failures from Malformed First Tool Call

### What happens now

Cron jobs use `plan_then_execute()`. The prompt is minimal: "Run the morning standup. Read skills/pm-rituals/standup.md for instructions." This forces the model's very first action to be a `read_file` tool call. Gemini frequently returns malformed function calls on the first turn, causing cascading retries and fallback failures.

The recovery path also creates conversation history with malformed content that the fallback model (flash) rejects due to missing `thought_signature`.

### Decision: Explicit context files field + completely fresh fallback

**3a — Context files:**
- New `context_files: Vec<String>` field on cron jobs (stored as JSON array in SQLite).
- `cron add` accepts `--context-file <path>` (repeatable).
- At execution time, `run_agent_job()` reads each file and prepends to the prompt as `## Context: <filename>\n<content>\n`.
- This is explicit (no fragile regex to detect file references), reads fresh content at execution time, and keeps the prompt clean.

**3b — Clean fallback:**
- When `plan_then_execute()` fails, the flat run fallback starts with a completely fresh context: original prompt + inlined context files only.
- No history carried from the failed planner attempt.
- A failed fallback counts as the job's final failure — no re-entry into the planner.
- No loop risk: fallback uses the flat agent loop (different code path from planner), and a failed fallback terminates the job.

---

## Problem 4: No Sub-Agent Delegation for Heavy Context Gathering

### What happens now

When the standup or planning ritual runs, the main agent loop makes 15–25 sequential tool calls to gather state. Each call adds tokens to the growing context. By the time the agent is ready to think about what to write, it's reasoning through a massive context.

### Decision: Pre-structured plans with parallel delegate sub-agents

**Unified plan interface:** The existing `Plan` struct is the interface. Two producers feed it:
- **Planner LLM** — for complex interactive messages
- **Scheduler** — for cron rituals, from `.plan.toml` files

The orchestrator is the single consumer. No new types needed.

**Pre-structured ritual plans:**
- New file convention: `<skill>.plan.toml` alongside `<skill>.md`.
- Scheduler reads `.plan.toml` → deserializes into `Plan` struct.
- Scheduler reads `.md` → inlines as context for synthesis actions.
- Hands `Plan` to orchestrator executor, skipping planner phase entirely.

**Plan file format:**

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
- Group 1 actions with `action_type = "delegate"` run as parallel sub-agents via the existing delegate tool infrastructure.
- Each sub-agent gets its own context window (isolated from main loop). This is the main win — per Anthropic: "Clear separation of concerns — the detailed search context remains isolated within sub-agents."
- Inter-group compression (existing 3000 char limit) passes summaries to group 2.
- Group 2 synthesis action receives compressed gather results + skill file content.

**Template variables:** Scheduler resolves `{{date}}`, `{{job_name}}` in action prompts at execution time. Minimal set, no complex templating engine.

### Validated against external guidance

- **Anthropic "Building Effective Agents":** Orchestrator-worker pattern with parallel workers. "Add sub-agents only when it demonstrably improves outcomes" — we only use sub-agents for known ritual patterns, not speculatively.
- **LangGraph orchestrator-worker:** "Each worker operates with its own distinct state" — matches our isolated delegate sub-agents. Orchestrator synthesizes worker outputs — matches our group 2 synthesis.

---

## Problem 5: Cron Job Duplicate Registration

### What happens now

`entrypoint.sh` runs `zeroclaw cron add` on every container start. If the container restarts, duplicate jobs are created. This produces "scheduled more frequently than every 5 minutes" warnings and potentially double-executes rituals.

### Decision: Name-based upsert

- Job `name` is the unique key.
- `cron add` with an existing name upserts (updates schedule, prompt, model, context_files).
- `name` is required for agent jobs.
- Return value indicates created vs. updated.
- Upsert means you can update a ritual's schedule or prompt without manually deleting the old one first.

---

## Non-Goals

- **Event-driven world model**: Too complex. Structured note-taking (files the agent reads/writes) achieves 80% of the benefit at 10% of the cost.
- **New memory subsystem**: SQLite + file-based state is sufficient. No need for vector search or embeddings for this use case.
- **Multi-model orchestration within a single interaction**: The current single-model-per-action approach is fine. Adding model switching mid-loop adds complexity without clear benefit.
- **LLM-generated tool result summaries**: Adds latency and failure points. One-line mechanical summaries are sufficient.
- **Complex template engine for plan files**: `{{date}}` and `{{job_name}}` cover the need. No Tera/Handlebars.

## Summary

| # | Problem | Decision | Affected Code |
|---|---------|----------|---------------|
| 5 | Duplicate cron registration | Name-based upsert | cron/store.rs |
| 3 | Cron first-tool-call failures | context_files field + fresh fallback | scheduler.rs, store.rs |
| 2 | No mid-loop compaction | Turn-based clearing, 3-turn TTL, one-line summaries | loop_.rs |
| 1 | Planner always passthroughs | Classifier fast path (skip planner for simple); classifier signals to planner for complex | loop_.rs, orchestrator.rs |
| 4 | No sub-agent delegation | Pre-structured .plan.toml; parallel delegate sub-agents; unified Plan interface | scheduler.rs, orchestrator.rs, types.rs |

Implementation order follows the table: P5 → P3 → P2 → P1+P4. Each step is independently deployable and reversible.
