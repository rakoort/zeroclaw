# Thread Gate Design

**Linear:** [SPO-92](https://linear.app/netspore/issue/SPO-92)
**Prior art:** `docs/plans/2026-02-25-thread-triage-design.md` (thread participation tracking + YES/NO triage — already implemented). This design extends it with a thread lock and richer triage actions.

## Problem

When Rain watches a Slack thread and two users send messages before Rain finishes responding, each message triggers a separate agent loop. This doubles API cost, risks contradictory responses, and creates race conditions on shared state.

More broadly, Rain treats every @mention as an independent task. It lacks thread awareness — it cannot distinguish "additional context for the current task" from "new unrelated request" from "humans talking among themselves."

## Goal

Rain should behave like a competent human PM in a Slack thread:

1. Read the whole thread before responding, not just the triggering message.
2. Stay silent when humans are talking among themselves.
3. Absorb new context while working, not spawn parallel tasks.
4. Speak only when it adds value — answering questions, correcting misinformation, confirming decisions, or capturing commitments.

## Design

### Two mechanisms

**Thread lock** — At most one agent loop per thread at a time. Messages arriving during execution are not dispatched. When the agent finishes, it re-reads the thread via `slack_threads` and addresses everything in one response.

**Thread triage** — When Rain is idle and a message arrives in a participated thread without an @mention, a fast classifier decides whether Rain should respond, act silently, or ignore it.

### Thread lock

The Slack channel adapter (`src/channels/slack.rs`) maintains a per-thread state map:

```
HashMap<ThreadId, ThreadState>

ThreadState:
  status: Idle | InFlight(task_id)
  pending_count: u32
```

Flow:

```
Message arrives for thread T
        |
   ThreadState[T].status?
   |                    |
  Idle               InFlight
   |                    |
  eyes react         eyes react
  dispatch agent     increment pending_count
  status -> InFlight   (no dispatch)
   |                    |
  (agent completes)   (agent completes)
   |                    |
  re-read thread     pending_count > 0?
  respond            yes: re-read thread, dispatch follow-up, reset count
  remove eyes        no: status -> Idle
  status -> Idle
```

The agent always calls `slack_threads` before composing its response. This ensures it addresses the full conversation state, not just the message that triggered dispatch.

#### Eyes emoji behavior

| Situation | Behavior |
|-----------|----------|
| First message, thread idle | Eyes immediately (responsive acknowledgment) |
| Message while agent in-flight | Eyes immediately (you're seen, in queue) |
| Agent responds | Remove eyes from the triggering message |
| Follow-up dispatches | Eyes already present on queued messages |

Every message gets eyes. This tells the user "Rain sees you." The difference is that only one agent loop runs at a time.

#### Rust implementation scope

In `src/channels/slack.rs`:

1. Add `thread_states: Mutex<HashMap<String, ThreadState>>` to `SlackChannel`.
2. Before dispatching (around line 804), check thread state. If `InFlight`, increment `pending_count` and skip dispatch.
3. After agent reply (around line 562, where eyes are removed), check `pending_count`. If > 0, re-read thread and dispatch a follow-up agent with the full thread context.
4. Bound the map to prevent unbounded growth. Evict entries with `Idle` status that haven't been touched in 30 minutes. Cap at 500 threads.

#### Config surface

```toml
[channels_config.slack.thread_gate]
enabled = true
max_tracked_threads = 500
idle_eviction_minutes = 30
```

### Thread triage

When Rain is idle in a participated thread and a message arrives without an @mention, a fast classifier decides the action.

#### Trigger conditions

All three must hold:
- Thread is in `participated_threads` (Rain sent a message in this thread before).
- Message does not @mention Rain.
- Thread state is `Idle` (no in-flight agent).

If the message @mentions Rain, bypass triage and dispatch directly — @mention is the guaranteed way to get Rain's attention.

#### Classifier

**Model:** gemini-3-flash (same as current triage model).

**Input:** Thread context (last 20 messages), Rain's role summary (one paragraph), the triggering message.

**Output:**

```json
{
  "action": "respond" | "silent_act" | "ignore",
  "confidence": 0.0-1.0,
  "reason": "one sentence"
}
```

**Confidence gate:** If confidence < 0.8, treat as `ignore`. This biases Rain toward silence.

#### Action definitions

**respond** — Rain posts a visible message. Triggers: direct questions about project state, requests for data Rain has (Linear, GitHub), incorrect claims about project status, decisions that need confirmation, unresolved blockers Rain can route.

**silent_act** — Rain updates Linear or state files but posts nothing. Triggers: commitments made ("I'll finish NET-45 by Thursday"), status updates ("NET-45 is done"), scope changes mentioned in passing.

**ignore** — Rain does nothing. Triggers: humans debating, social/emotional messages, someone already answered the question, technical implementation details, acknowledgments ("ok", "thanks", "sounds good").

#### Classifier prompt

```
You are a triage classifier for Rain, an autonomous PM agent.
Rain participated in this thread. Decide if the latest message requires Rain to act.

Rain's role: track project state in Linear, surface blockers, capture decisions
and commitments, correct incorrect claims about project status.

Rain does NOT weigh in on technical decisions, social conversations, or debates.

RULES:
- Bias toward "ignore". When in doubt, stay silent.
- "respond" = Rain must post a visible reply
- "silent_act" = Rain should update Linear or state, no visible reply
- "ignore" = Rain does nothing
- If a human already addressed the need, output "ignore"
- If the message is social, emotional, or off-topic, output "ignore"
- If the message is about technical implementation, output "ignore"

Thread context:
{thread_messages}

Latest message:
{latest_message}

Output JSON only:
{"action": "respond"|"silent_act"|"ignore", "confidence": 0.0-1.0, "reason": "..."}
```

#### Performance

- ~200-500ms latency per triage call.
- Volume: 10-30 messages/day in participated threads (current Slack activity).
- Cost: negligible — short context, cheapest model.

### Agent prompt changes

AGENTS.md needs new instructions:

1. **Always re-read the thread before responding.** Call `slack_threads` to get the current state. Address the full conversation, not just the triggering message.
2. **One response per thread turn.** If multiple questions were asked, answer them all in one message. Do not send separate messages for each.
3. **Match your response to the need.** A decision confirmation is one sentence. A status query gets a brief answer. Don't over-explain.
4. **Silent actions need no announcement.** When capturing a commitment or updating Linear status based on thread context, do it without posting.

## Scenarios

**Rapid-fire from one person:**
> Alice: "@rain I think we should reprioritize the sprint"
> Alice (2s later): "NET-45 is blocked and NET-52 is more urgent"

Thread lock holds. Agent dispatches on first message. When it finishes, it re-reads the thread, sees both messages, and responds to the full context in one reply.

**Two people, quick succession:**
> Alice: "@rain what's the status of NET-45?"
> Bob (3s later): "@rain also check NET-52?"

First message dispatches agent. Bob's message gets eyes but no new agent. When the agent finishes, it re-reads the thread, sees Bob's question, and either addresses both in one response or dispatches a follow-up for Bob's question.

**Message while Rain is working:**
> Alice: "@rain run the sweep"
> (Rain is gathering data, 20s in...)
> Bob: "@rain what's blocking NET-45?"

Bob gets eyes immediately. Rain finishes the sweep, posts it, then dispatches a follow-up for Bob's question with full thread context.

**Humans discussing, no @mention:**
> Alice: "I think we should use Redis for the cache"
> Bob: "Postgres might be simpler"
> Alice: "good point, let's go with Postgres"

Triage classifies each message as `ignore`. Rain stays silent. This is a technical decision — not Rain's domain.

**Commitment made, no @mention:**
> Alice: "I'll finish NET-45 by end of day Thursday"

Triage classifies as `silent_act` (confidence 0.9). Rain updates the Linear issue due date. Posts nothing.

**Incorrect claim, no @mention:**
> Bob: "I think NET-45 is done, we can move on"
> (Linear shows NET-45 is in-progress)

Triage classifies as `respond` (confidence 0.85). Rain replies: "NET-45 is still in-progress in Linear. Want me to check with the assignee?"

## Implementation scope

| Component | Change | Effort |
|-----------|--------|--------|
| `src/channels/slack.rs` | Thread state map, dispatch gating, follow-up logic | Medium |
| `src/channels/slack.rs` | Move eyes react inside thread gate (already fires, just gate dispatch) | Small |
| `zeroclaw.toml` schema | `[channels_config.slack.thread_gate]` section | Small |
| Triage classifier | New classifier mode for participated-thread messages | Medium |
| `AGENTS.md` | Thread-aware response instructions | Small |
| `SOUL.md` | Response mode guidance (respond vs silent_act) | Small |

## What does not change

- @mention always triggers a response. Thread triage only applies to non-mentioned messages in participated threads.
- Cron jobs and heartbeat bypass the thread gate entirely.
- The scheduler (`max_concurrent = 4`) still enforces global concurrency.
- `mention_only = true` still applies to non-participated threads.
- The `slack_react` tool remains available for explicit use by the agent.
