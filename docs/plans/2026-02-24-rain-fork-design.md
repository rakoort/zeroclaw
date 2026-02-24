# Rain Fork Design: Slack Context, Gemini Preprocessing, Linear-as-Brain

**Date:** 2026-02-24
**Status:** Draft
**Repo:** rakoort/zeroclaw (fork of zeroclaw-labs/zeroclaw)

## Problem

ZeroClaw has a solid Rust foundation — fast, lean, secure — but three gaps make it
unusable as a PM agent:

1. **Slack is blind.** It polls `conversations.history` only. Thread replies are invisible.
   Every message from an allowed user triggers a response. No mention gating, no
   silent observation, no conversation awareness.

2. **Gemini 3 Flash returns 500s.** ZeroClaw sends raw tool schemas and transcripts to
   Gemini. Unsupported JSON Schema keywords (`$ref`, `const`, `anyOf`, `additionalProperties`,
   `format`, etc.) and non-alphanumeric tool call IDs cause server errors. OpenClaw avoids
   this with a 420-line schema sanitizer and transcript repair pipeline.

3. **No continuous Linear awareness.** The agent treats Linear as an on-demand tool
   rather than its primary brain. A PM must consult the project board before every
   action, not just during rituals.

## Design Principles

- **Stay close to upstream.** Surgical changes in isolated files. Easy to rebase.
- **No bloat.** Port only the patterns that matter, not OpenClaw's full complexity.
- **Lean token usage.** Compress context rather than dumping raw history.

---

## Change 1: Slack Context Awareness

**Files:** `src/channels/slack.rs`, `src/channels/mod.rs`, `src/channels/traits.rs`

### 1.1 Thread Hydration

When an incoming message has `thread_ts`, fetch the full thread before processing:

- Call `conversations.replies?channel={ch}&ts={thread_ts}&limit=20`
- Format each reply as: `[Slack #channel Sender (role) timestamp] Sender: text`
- Label messages from the bot as `(assistant)`, all others as `(user)`
- Prepend thread history to the message content before it enters `process_channel_message`
- Cache thread replies per `(channel, thread_ts)` with 60-second TTL to avoid
  redundant API calls

Add `thread_starter_body: Option<String>` and `thread_history: Option<String>` fields
to `ChannelMessage` in `traits.rs`. The processing pipeline in `mod.rs` can use these
to build richer context without the Slack channel leaking into core logic.

### 1.2 Mention Gating

Add a `mention_only` config field to `SlackConfig` (default: `true`). When enabled:

**Respond when:**
- Message contains `<@{bot_user_id}>`
- Message is a reply in a thread where `parent_user_id == bot_user_id` (implicit mention)
- Message matches a configurable mention regex

**Observe silently when:**
- None of the above. Buffer the message as pending history.

### 1.3 Pending History Buffer

Maintain a per-channel ring buffer of non-mention messages (max 50). When Rain IS
mentioned, prepend the buffer to the message:

```
[Chat messages since your last reply - for context]
[Slack #channel Alice Wed 2026-02-24 14:25] Alice: hey did anyone see the deploy?
[Slack #channel Bob Wed 2026-02-24 14:27] Bob: yeah looks good

[Current message - respond to this]
[Slack #channel Charlie Wed 2026-02-24 14:30] Charlie: @Rain what do you think?
```

Clear the buffer after responding. This gives Rain full conversational awareness
without responding to every message.

### 1.4 Conversation Role Detection

Strip `<@bot_user_id>` from message text before sending to the LLM (the mention
served its purpose as a routing signal). Pass `was_mentioned: bool` as metadata
so the system prompt can instruct the model accordingly.

### 1.5 Session Scoping Fix

In `mod.rs`, change `conversation_history_key` from:
```
{channel}_{thread_ts}_{sender}
```
to:
```
{channel}_{thread_ts}
```

Drop per-sender isolation so the bot sees the full thread conversation, not
siloed per-person views.

### 1.6 Envelope Format

Wrap all messages (current, history, thread) in a structured envelope:

```
[Slack #channel-name SenderName Wed 2026-02-24 14:30:05] SenderName: message text
```

Include elapsed time markers (`+5m`) between consecutive messages to give the model
a sense of conversation pacing.

---

## Change 2: Gemini Schema and Transcript Preprocessing

**Files:** new `src/providers/gemini_sanitize.rs`, modifications to `src/providers/gemini.rs`

### 2.1 Tool Schema Sanitization

Before sending tool definitions to Gemini, run every parameter schema through a
recursive sanitizer that:

**Strips unsupported keywords:**
```
patternProperties, additionalProperties, $schema, $id, $ref, $defs, definitions,
examples, minLength, maxLength, minimum, maximum, multipleOf, pattern, format,
minItems, maxItems, uniqueItems, minProperties, maxProperties
```

**Resolves `$ref` references:**
- Inline referenced definitions from `$defs`/`definitions`
- Detect circular references and replace with `{}` (empty object)

**Converts `const` to `enum`:**
- `{ "const": "value" }` becomes `{ "enum": ["value"] }`

**Flattens unions:**
- If all `anyOf`/`oneOf` variants are literal values, collapse to `{ "type": "string", "enum": [...] }`
- Strip null variants from unions; unwrap single-variant unions
- Drop `type` field when it coexists with `anyOf`/`oneOf`
- Last resort: pick representative type from first variant

**Normalizes type arrays:**
- `"type": ["string", "null"]` becomes `"type": "string"`

### 2.2 Transcript Sanitization

Before each LLM call, sanitize the message history:

**Tool call IDs:**
- Rewrite all IDs to alphanumeric-only `[a-zA-Z0-9]`
- Maintain a mapping to prevent collisions

**Turn ordering:**
- Merge consecutive same-role messages
- Prepend synthetic user message if history starts with assistant
- Ensure every tool_call has a matching tool_result immediately after
- Drop orphaned tool_results; inject synthetic error results for missing ones

**Thought signatures:**
- Strip invalid (non-base64) thought signatures
- Handle both `thought_signature` and `thoughtSignature` variants

### 2.3 Integration Point

Apply sanitization in `gemini.rs` before the HTTP request in `chat_with_tools`
and `chat_with_history`. The sanitizer is a pure function with no side effects —
`fn sanitize_tools_for_gemini(tools: &[Tool]) -> Vec<Tool>` and
`fn sanitize_transcript_for_gemini(messages: &[ChatMessage]) -> Vec<ChatMessage>`.

---

## Change 3: Linear as the Brain (No Rust Changes)

**Files:** ZeroClaw workspace config (AGENTS.md, skills, SOUL.md)

### 3.1 Linear Context Skill

Create a skill at `workspace/skills/linear-context/SKILL.md`:

- Tool: `check_linear` — shell command that queries Linear GraphQL API for:
  - Active cycle issues (status, assignee, priority)
  - Recently updated issues (last 24h)
  - Issues matching keywords from the current message
- Prompt instruction: "Before every response that touches work state, use
  `check_linear` to query current issues. Never create, update, or close an issue
  without first checking Linear state."

### 3.2 AGENTS.md Rule

Add to Rain's operating rules:

> **Linear is your brain.** Before creating, updating, or commenting on any issue,
> query Linear for current state. Before responding to any message about work,
> check if relevant issues exist. A real PM always has their project board open.
> You must do the same — every time, not just during rituals.

### 3.3 Confidence-Based Autonomy

Define in AGENTS.md:

- **High confidence** (exact issue ID mentioned, status update with clear mapping):
  Update Linear silently, log to #rain-log.
- **Low confidence** (vague reference, ambiguous commitment, could map to multiple issues):
  Confirm with the person in-thread before acting.

### 3.4 Escalation Path

If prompt-level instructions prove unreliable (model skips Linear checks), wire
ZeroClaw's existing `before_llm_call` hook — the trait method and runner dispatch
exist but are never called. Requires ~10 lines in `src/agent/loop_.rs` to activate,
plus a builtin `LinearContextHook` implementation.

---

## What We Keep From Upstream

Everything not listed above stays untouched:

- Provider abstraction and `ReliableProvider` retry/fallback logic
- Gemini auth (5 methods, OAuth refresh, credential rotation)
- Tool-call loop and agent architecture
- Memory system (SQLite, semantic recall)
- Cron scheduler
- Config system (TOML)
- All other channel implementations
- Security sandbox, hooks framework, SOP engine

## Fork Maintenance

- Track upstream `zeroclaw-labs/zeroclaw` via `upstream` remote
- Rebase periodically: our changes touch 3-4 files, conflicts should be rare
- If upstream adds thread awareness or schema sanitization, drop our patches

## Implementation Order

1. **Gemini preprocessing** (Change 2) — unblocks Gemini 3 Flash, smallest blast radius
2. **Slack context** (Change 1) — largest change, most impact on Rain's behavior
3. **Linear brain** (Change 3) — config-only, no Rust, can iterate on wording

## Open Questions

- Should pending history use token-aware compression (summarize older messages)?
- Should thread hydration depth be configurable per channel?
- Should the Gemini sanitizer live in `reliable.rs` (applied to all providers)
  or only in `gemini.rs`?
