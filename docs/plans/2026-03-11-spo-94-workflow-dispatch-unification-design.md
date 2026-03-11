# Workflow and Dispatch Unification Design

**Date:** 2026-03-11
**Issue:** [SPO-94](https://linear.app/netspore/issue/SPO-94)
**Status:** Draft
**Scope:** Execution model, dispatch, intake, state, waiting

## Problem

ZeroClaw's execution model cannot express async interactive workflows. A standup ritual that sends DMs, waits for replies, and posts a summary requires three capabilities the system lacks today:

1. **Pause and resume.** Plans run synchronously in a single pass. A plan cannot yield mid-execution and resume when an external event arrives.
2. **Event correlation.** Every incoming message creates new agent work. No mechanism routes a Slack reply back to the plan that initiated the DM.
3. **Cross-integration triggers.** The system ignores events from GitHub and Linear. A workflow cannot wait for a PR merge or an issue close.

These gaps block all interactive PM rituals and every future workflow requiring human-in-the-loop steps.

## Root Cause

The system lacks a long-lived unit of work spanning multiple triggers. Six concerns — intake, execution, state, triggers, waiting, and routing — each have multiple implementations that cannot see each other's decisions or share state across execution boundaries.

## Current Architecture: Overlap Analysis

### Intake: Four Filters, Two LLM Calls

Four mechanisms decide whether the agent engages:

- **Mention gate**: regex check for @bot_id. Cheap, instant.
- **Triage gate**: LLM call deciding respond/silent_act/ignore. Expensive, brittle.
- **Classifier**: 14-dimension heuristic scoring query complexity.
- **Router**: maps classifier output to execution path.

Triage and classifier both evaluate the same message in the same context yet run as separate steps. The triage gate exists to answer "should I respond to a message I wasn't asked about?" — a question that should never arise.

### Execution: Four Wrappers Around One Loop

Four callers wrap `run_tool_call_loop`:

- **Agent loop**: the real executor.
- **Planner execute phase**: wraps it with group sequencing, context accumulation, tool filtering.
- **Cron agent job**: wraps it with prompt building and delivery.
- **Delegate tool**: wraps it with context isolation.

The planner orchestrator adds ~300 lines of group-by-group sequencing. The delegate tool duplicates planner delegate-mode. All four wrappers exist because the loop lacks a concept of steps.

### State: Five Stores, Zero Workflow Awareness

- **Conversation history**: in-memory, per-thread/sender, trimmed at 50 messages.
- **Memory**: persistent KV with semantic search, designed for recall.
- **Plan accumulated results**: in-memory, inter-group context, discarded after execution.
- **Cron `last_output`**: denormalized string on the job row.
- **Cron `cron_runs`**: audit log table.

Plan accumulation is conversation history by another name. `last_output` duplicates `cron_runs`. No store represents "what is pending and what should happen next."

### Triggers: Three Entry Points, Three Dispatch Paths

- **Channel.listen()**: WebSocket → dispatch loop → triage → classify → route → execute.
- **Cron tick**: timer → load job → build prompt → execute.
- **Delegate**: tool call → isolated agent loop.

Three separate dispatch paths feed identical execution. The differences are pre-processing steps, not structural.

### Waiting: Five Mechanisms, None for External Events

- **Loop iteration**: synchronous await within a turn.
- **Cron schedule**: time-based (`next_run <= now`), fires fresh execution only.
- **Thread gate**: concurrency lock, drops messages if thread is busy.
- **Plan group sequence**: synchronous await within a single execution.
- **Polling workaround**: agent burns LLM calls checking for state changes.

The cron scheduler is the only true async wait mechanism. It checks time conditions but not event conditions, and it starts work but never resumes it.

### Routing: Already Clean

Model routes, per-action hints, and tool lists layer cleanly. No consolidation needed.

## Design: Consolidated Architecture

### Intent Model

Rain acts when intent is unambiguous. Rain listens when it is not.

**Unambiguous intent (produce WorkItem, dispatch):**
- Explicit @mention in a group channel
- Any message in a DM
- Cron/scheduled trigger
- Workflow event condition met (PR merged, reaction received, deadline expired)
- Webhook from an integration

**Ambiguous intent (accumulate context silently):**
- Group thread message without @mention

Rain listens in every thread it participates in. Every message adds to thread context. When someone @mentions Rain, the full conversation is available — including messages Rain did not respond to.

This eliminates the triage gate entirely. No LLM call decides engagement. The mention gate becomes the sole intake decision for channel messages.

### Consolidated Intake

```
Input arrives
  → Does this produce a WorkItem?
    → @mention in group context: yes
    → DM message: yes
    → Cron trigger: yes
    → Webhook event: yes
    → Workflow condition match: yes
    → Group thread, no @mention: no → accumulate context, done
  → If yes: needs_evaluation?
    → Unsolicited input (channel message): evaluate (classify complexity)
    → Pre-authorized (cron, webhook, workflow): skip evaluation
  → Dispatch WorkItem
```

Mention gate stays as a cheap pre-filter. Triage gate disappears. Classifier runs only when needed. One evaluation step replaces two.

### Unified WorkItem

Every trigger produces the same structure:

```rust
struct WorkItem {
    prompt: String,
    context: Vec<ContextBlock>,       // thread history, context files, prior step results
    reply_to: ReplyTarget,            // channel+thread, delivery config, parent workflow
    constraints: ExecutionConstraints, // tools, model, iteration budget
    needs_evaluation: bool,           // true only for unsolicited channel input
}
```

Channel messages, cron jobs, delegates, and workflow continuations all produce WorkItems. One dispatch path processes them all, skipping pre-resolved steps.

### Unified Execution

The agent loop (`run_tool_call_loop`) remains the sole executor but gains step-awareness:

- The planner LLM produces steps but no longer orchestrates execution.
- Steps are WorkItems. Group sequencing reduces to: dispatch step N; when it completes, dispatch N+1 with accumulated context.
- Delegate mode becomes a WorkItem with isolated context — no separate mechanism.
- Cron jobs produce WorkItems through the same dispatch path.

Plan accumulation merges into conversation history. Each step's result becomes a history entry scoped to its execution.

### Consolidated State

Three stores replace five:

| Store | Purpose | Persistence |
|-------|---------|-------------|
| **Conversation history** | Execution context (turns, step results) | In-memory, per-scope |
| **Memory** | Long-term knowledge, semantic recall | Persistent (SQLite/vector) |
| **Runs** | Audit log (cron runs, workflow runs) | Persistent (SQLite) |

`last_output` drops from the cron job row; query `cron_runs` instead. Plan accumulated results become conversation history entries.

One new store: **workflow state**. When a workflow yields, it persists:
- Completed steps and their results
- Remaining steps
- Wait condition (what to resume on)

This is a new table, not a modification to existing stores. It captures pending work — the one thing no current store represents.

### Event System

Every connected integration can produce events. The system matches each event against pending workflow conditions.

```rust
struct Event {
    source: IntegrationSource,        // Slack, GitHub, Linear, Timer
    event_type: String,               // "message", "pr_merged", "issue_updated"
    fields: HashMap<String, Value>,   // source-specific payload
    timestamp: Instant,
}
```

**Slack (partially ready):**
- Socket Mode WebSocket already delivers events in real time.
- Currently filters to `message` events only. Widen to accept `reaction_added` and `app_mention`.
- Small change to existing code.

**GitHub (new):**
- GitHub posts signed JSON payloads to a webhook URL on repository events (push, PR merge, issue close, etc.).
- Add a gateway endpoint (`POST /github`) with HMAC-SHA256 signature verification.
- Follows the pattern the WhatsApp/Nextcloud webhook handlers establish.

**Linear (new):**
- Linear posts signed JSON payloads on data changes (issue created/updated, comment added, etc.).
- Add a gateway endpoint (`POST /linear`) with signature verification.
- Same pattern as GitHub.

All three emit unified `Event` structs into the same stream. The dispatch layer matches events against pending conditions regardless of which integration produced them.

### Waiting: Two Paths

**Event path (push, instant):**
When an event arrives — a Slack message, a GitHub webhook, a Linear notification — the dispatch layer checks it against pending workflow conditions before treating it as new work. A match filters on event fields: source, type, sender, thread, branch name, issue ID. If matched, the event delivers to the waiting workflow; otherwise, normal dispatch proceeds.

**Timeout path (poll, periodic):**
The cron scheduler already ticks on an interval and checks time conditions. Extend it to check workflow deadlines as well. When a deadline expires, the scheduler loads the persisted workflow state and dispatches a continuation WorkItem with a timeout result.

```rust
struct WaitCondition {
    workflow_id: String,
    event_matcher: EventMatcher,          // source + type + field filters
    completion: CompletionTrigger,        // when is the wait satisfied?
    deadline: Option<Instant>,            // when to give up
    timeout_behavior: TimeoutBehavior,    // what to do on timeout
}
```

**Completion triggers:**
- `EventMatch` — the event itself completes the wait (PR merged, issue closed). Unambiguous.
- `Mention` — an @mention in the thread completes the wait. For message-based waits (standup DMs), the cofounder @mentions Rain when done. The normal agent loop handles messages during the wait as live conversation.

**Timeout behaviors:**
- `CollectAndContinue` — gather thread/event history; resume the workflow with whatever exists.
- `SkipWithDefault` — resume with a default "no response" value.
- `Retry { max }` — send a reminder and reset the timer.

### Workflow Lifecycle

A workflow is a sequence of steps; some steps wait for external events.

```
1. Trigger fires (cron tick, @mention, webhook)
2. Produce WorkItem(s) for initial steps
3. Dispatch through unified path
4. Executor runs steps sequentially
5. Step hits a wait condition:
   a. Persist workflow state (completed steps, remaining steps, accumulated context)
   b. Register WaitCondition (event matcher + deadline)
   c. Execution ends cleanly
6. Time passes. Thread conversations happen normally.
7. Wake-up:
   a. Event arrives → dispatch layer matches → load workflow state → build continuation WorkItem
   b. Deadline expires → scheduler matches → load workflow state → build timeout WorkItem
8. Dispatch continuation through unified path
9. Executor resumes from persisted state with event/timeout payload as new context
10. Repeat from step 4 until no steps remain
```

Steps that do not wait run synchronously, as today. The yield/resume mechanism activates only when a step explicitly declares a wait condition.

### Thread Context Accumulation

Rain listens in all threads it participates in, even without an @mention. Every message adds to thread context. This requires:

- `Channel.listen()` continues to receive all messages in participated threads (already works).
- Messages that produce no WorkItem still accumulate as thread context.
- When Rain receives an @mention or a workflow resumes, the full thread history is available.

Thread history hydration (fetching `conversations.replies` from Slack API) already provides this. The change: non-@mention messages bypass triage and accumulate silently.

## Integration Event Sources: Current State

| Integration | Push events today? | Work needed |
|---|---|---|
| Slack | Messages only (Socket Mode) | Widen event filter to reactions + app_mention |
| GitHub | No | New gateway webhook endpoint |
| Linear | No | New gateway webhook endpoint |

The gateway already handles webhooks for WhatsApp, Linq, WATI, and Nextcloud with rate limiting, idempotency, and signature verification. GitHub and Linear endpoints follow this established pattern.

Both GitHub and Linear support webhook configuration natively. GitHub POSTs signed JSON payloads for 40+ event types; Linear POSTs signed JSON payloads for issue, comment, project, and cycle changes.

Slack event widening and GitHub/Linear webhooks ship incrementally, after core workflow/dispatch unification.

## What Gets Removed

| Component | Reason |
|-----------|--------|
| Triage gate (`triage_required`, `THREAD_TRIAGE_PROMPT`, triage LLM call) | Replaced by intent model: @mention = act, otherwise = listen |
| `silent_act` triage outcome | If Rain should act, someone @mentions it |
| Planner orchestrator's group-by-group execution logic | Steps become WorkItems; dispatch handles sequencing |
| Delegate tool as separate mechanism | WorkItem with isolated context, same dispatch path |
| Plan accumulated results (separate from history) | Merge into conversation history |
| `last_output` on cron job row | Query `cron_runs` table instead |
| Polling workaround for async waits | Event system + timeout path replace it |

## What Gets Added

| Component | Purpose |
|-----------|---------|
| `WorkItem` struct | Unified dispatch unit for all triggers |
| Unified dispatch path | One function processes all WorkItems |
| `Event` struct + event stream | Unified event representation from all integrations |
| `WaitCondition` + pending conditions store | Yielding workflows register; dispatch matches |
| Workflow state persistence (SQLite table) | Persists yielded workflow state for resumption |
| Event matching in dispatch loop | Checks incoming events against pending conditions before normal processing |
| Deadline checking in scheduler | Checks workflow timeouts on each tick |
| Gateway endpoints for GitHub and Linear | Receive webhook POSTs; emit Events |
| Wider Slack event filter | Accepts reactions and app_mention events |

## What Stays Unchanged

| Component | Reason |
|-----------|--------|
| `run_tool_call_loop` | Remains the sole executor. Gains step-awareness, not replacement. |
| Memory subsystem | Long-term knowledge store, orthogonal to workflow state. |
| Provider/model routing | Composes well; no consolidation needed. |
| Security policy | Tool-level enforcement unchanged. |
| Observability | Event recording pattern unchanged; new event types added. |
| Mention gate | Stays as cheap pre-filter; now the sole intake gate for channel messages. |
| Conversation history | Gains plan step results. Storage model unchanged. |

## Risks

1. **Workflow state schema.** Serializing mid-execution state (completed steps, accumulated context, remaining steps) requires a stable schema. Plan structure changes break persisted workflows. Mitigation: version the schema; expire stale workflows.

2. **Event matcher expressiveness.** Simple field filters may not cover all matching needs. A workflow might need "PR merged where branch name contains the Linear issue ID extracted from step 2." Mitigation: start with exact and contains filters; extend as real use cases emerge.

3. **Concurrent workflow contention.** Two workflows may wait on events in the same thread or from the same user. Mitigation: match returns all matching workflows, not just the first; each evaluates independently.

4. **Gateway security surface.** New webhook endpoints for GitHub and Linear expand the attack surface. Mitigation: signature verification (both services support HMAC-SHA256), rate limiting, and body size limits — all patterns the gateway already implements.

## Non-Goals

- Full workflow DSL or visual builder. Workflows are plans with wait steps, not a new language.
- Real-time streaming of workflow state to a UI. Observability events suffice.
- Replacing cron for time-based scheduling. Cron stays for simple recurring jobs; workflows handle multi-step interactive flows.
- Backward compatibility with `triage_required` / triage gate. This is a clean removal.
