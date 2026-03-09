# Context Engineering & Routing — Problem Statement

**Date:** 2026-03-09
**Issue:** [SPO-79](https://linear.app/netspore/issue/SPO-79)
**Scope:** zeroclaw runtime changes
**Companion:** spore-pm [SPO-80](https://linear.app/netspore/issue/SPO-80) covers config/prompt-level changes

## Background

Rain (our PM agent on zeroclaw) processes messages via Slack and runs 4 daily cron rituals. Analysis of production logs (2026-03-09) reveals that the runtime architecture has clear bottlenecks that degrade response latency and reliability. Interactive replies take 19–101 seconds. Cron rituals take 60–100+ seconds. The root causes are in the zeroclaw runtime, not in the prompts.

This document identifies the specific runtime problems and the simplest changes that would fix them, informed by Anthropic's published guidance on building effective agents ("Building Effective Agents", "Effective Context Engineering for AI Agents", "Framework for Safe and Trustworthy Agents").

## Guiding Principle

From Anthropic: "Success in the LLM space isn't about building the most sophisticated system. It's about building the right system for your needs." Every change proposed here should reduce complexity or hold it constant while improving outcomes. No new abstractions unless they replace existing ones.

---

## Problem 1: The Planner Always Returns Passthrough

### What happens now

The 3-phase planner (plan → execute → synthesize) exists in `src/planner/orchestrator.rs`, but in practice the planner LLM returns `passthrough: true` on nearly every message. This means every message — whether "mark SPO-74 done" or "plan next sprint" — enters the same flat tool call loop with all tools available.

### Why this matters

- A simple status update ("SPO-74 is done") triggers 15+ sequential LLM roundtrips because the flat loop has no concept of message complexity
- Every roundtrip sends the full growing conversation context
- The 14-dimension classifier in `src/agent/classifier.rs` scores messages but the planner ignores the classification

### What Anthropic says

Routing is a core workflow pattern: "Classifies inputs and directs them to specialized downstream tasks." The planner should be the smartest routing decision in the system, not a rubber stamp.

### Desired outcome

The planner should distinguish at least two tiers:
1. **Acknowledge** — simple status updates, confirmations, single-tool-call tasks. The planner returns a 1-action plan with a tight tool budget (1–3 iterations).
2. **Full loop** — complex requests requiring multi-step reasoning and many tool calls. The planner returns a multi-action plan or passthrough with full budget.

The classifier output should inform the planner's decision. This is not a new subsystem — it's making the existing planner and classifier actually work together.

---

## Problem 2: No Tool Result Clearing / Mid-Loop Compaction

### What happens now

History compaction triggers at 50 non-system messages (`src/agent/loop_.rs`). Within a single interaction, the tool call loop accumulates raw tool results in the conversation context. By the time the loop reaches iteration 15–20, the model is reasoning over a massive context full of stale Linear API responses, Slack message dumps, and file contents that were only needed for earlier steps.

### Why this matters

Anthropic identifies "context rot" — model recall degrades as token count increases. Every token depletes the "attention budget." Logs show `contents_count` reaching 55 in a single interaction, meaning the model is processing 55 conversation turns of accumulated tool output.

### What Anthropic says

"One of the safest, lightest-touch forms of compaction is tool result clearing — removing raw tool outputs once their purpose is served."

### Desired outcome

After the model has processed a tool result and moved on (i.e., the next assistant message doesn't reference it), the raw tool result should be replaced with a short summary or cleared entirely. This is a change to the tool call loop in `src/agent/loop_.rs`, not a new subsystem.

Two levels of aggressiveness:
1. **Conservative:** Replace tool results older than N turns with a one-line summary ("Linear query returned 8 issues for cycle W11")
2. **Aggressive:** Clear all tool results except the most recent 2–3

The threshold (N turns, or token budget) should be configurable in `[agent]` config.

---

## Problem 3: Cron Job Planner Failures from Malformed First Tool Call

### What happens now

Cron jobs use `plan_then_execute()`. The prompt is minimal: "Run the morning standup. Read skills/pm-rituals/standup.md for instructions." This forces the model's very first action to be a `read_file` tool call. Gemini frequently returns malformed function calls on the first turn (e.g., `call{"name":"read_file",...}` instead of proper format), causing cascading retries and fallback failures.

The recovery path also creates conversation history with malformed content that the fallback model (flash) rejects due to missing `thought_signature`.

### Why this matters

The standup cron failed its first planner attempt on 2026-03-09. It recovered via flat run fallback, but this added ~30 seconds of wasted retries and produced error noise. If the fallback had also failed, the standup would have been skipped entirely.

### Desired outcome

Two changes, both simple:

1. **Cron job prompts should inline essential context** rather than requiring a tool call as the first action. The cron scheduler already has access to the workspace — it could read the skill file and include its content in the prompt. This eliminates the "first tool call" failure mode entirely.

2. **The fallback path should not replay malformed conversation history.** When the planner fails and falls back to a flat run, it should start with a clean context, not inherit the broken conversation from the failed attempt.

---

## Problem 4: No Sub-Agent Delegation for Heavy Context Gathering

### What happens now

When the standup or planning ritual runs, the main agent loop makes 15–25 sequential tool calls to gather state: query Linear for cycle issues, read Slack channels, check GitHub PRs, read memory, read state files. Each call adds tokens to the growing context. By the time the agent is ready to *think* about what to write, it's reasoning through a massive context.

### Why this matters

Anthropic explicitly recommends sub-agent architectures for this: "Specialized sub-agents handle focused tasks with clean context windows, returning condensed, distilled summary of its work (often 1,000-2,000 tokens) despite potentially using tens of thousands internally."

### What Anthropic says

"Clear separation of concerns — the detailed search context remains isolated within sub-agents, while the lead agent focuses on synthesizing and analyzing the results."

### Current state in zeroclaw

The `delegate` tool exists. The planner can specify actions with different tool sets and model hints per group. The infrastructure for sub-agent-like behavior is partially there.

### Desired outcome

The planner's action groups already support this pattern. The change is to make the planner actually use it:
- Group 1 actions: data gathering (Linear queries, Slack reads, file reads) with a compressed summary returned to the next group
- Group 2 action: synthesis and response composition using the compressed summaries

The inter-group context compression (`compress_context_between_groups` in orchestrator.rs, currently 3000 char limit) is the mechanism. The question is whether the planner LLM can be prompted to produce plans that leverage this, or whether it needs to be hardcoded for cron rituals.

The simplest version: the cron scheduler pre-structures the plan for ritual jobs instead of asking the LLM to plan. The ritual skill file defines the action groups, and the scheduler feeds them directly to the executor, skipping the plan phase entirely.

---

## Problem 5: Cron Job Duplicate Registration

### What happens now

`entrypoint.sh` runs `zeroclaw cron add` on every container start. If the container restarts, duplicate jobs are created. This produces "scheduled more frequently than every 5 minutes" warnings and potentially double-executes rituals.

### Desired outcome

Either:
- `zeroclaw cron add` should be idempotent (skip if a job with the same schedule+prompt already exists)
- Or provide a `zeroclaw cron sync` command that reconciles registered jobs with a desired state

The idempotent approach is simpler.

---

## Non-Goals

- **Event-driven world model**: Too complex. Structured note-taking (files the agent reads/writes) achieves 80% of the benefit at 10% of the cost.
- **New memory subsystem**: SQLite + file-based state is sufficient. No need for vector search or embeddings for this use case.
- **Multi-model orchestration within a single interaction**: The current single-model-per-action approach is fine. Adding model switching mid-loop adds complexity without clear benefit.

## Summary

| # | Problem | Simplest Fix | Affected Code |
|---|---------|-------------|---------------|
| 1 | Planner always passthroughs | Make classifier inform planner; 2-tier routing | orchestrator.rs, classifier.rs |
| 2 | No mid-loop compaction | Clear stale tool results after N turns | loop_.rs |
| 3 | Cron first-tool-call failures | Inline skill content in cron prompt; clean fallback context | scheduler.rs, orchestrator.rs |
| 4 | No sub-agent delegation | Pre-structured action groups for rituals; use existing inter-group compression | scheduler.rs, orchestrator.rs |
| 5 | Duplicate cron registration | Idempotent `cron add` | cron/store.rs |
