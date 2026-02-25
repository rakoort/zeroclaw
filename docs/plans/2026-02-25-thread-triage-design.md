# Thread Participation Triage Design

Date: 2026-02-25

## Problem

ZeroClaw's Slack channel only auto-follows threads the bot started (parent_user_id == bot_user_id). When a user @mentions the bot in a channel message, the bot replies in-thread, but subsequent thread replies without @mention are ignored because the thread parent is the user, not the bot.

Users must @mention the bot in every thread message, which is unnatural and breaks conversational flow.

## Solution

Two changes: detect thread participation via an in-memory set, and add LLM-based triage so the bot monitors threads without replying to every message.

### Thread Participation Tracking

Replace `is_implicit_mention(bot_user_id, parent_user_id)` with an in-memory `HashSet<String>` of `thread_ts` values where the bot has sent a reply.

- Populate the set when the bot posts a reply (already tracked in the listen loop's send path)
- Check membership when a threaded message arrives: if `thread_ts` is in the set, the bot is a participant
- Resets on restart — correct behavior, since the bot hasn't participated in any threads in the current session

### Triage Routing

Thread-participant messages without explicit @mention get a `triage_required: true` flag on `ChannelMessage`. The orchestration layer makes a cheap LLM call to decide whether to respond.

**Flow:**

```
Message arrives in thread
  ├── Explicit @mention? → respond (triage_required = false)
  ├── Bot participated in thread? → triage (triage_required = true)
  └── Neither? → buffer silently (existing behavior)

Triage (orchestration layer):
  ├── triage_model configured? → call flash-lite with triage prompt
  │     ├── YES → full agent turn
  │     └── NO / error / timeout → buffer as context, skip
  └── triage_model not configured? → buffer silently (backward-compatible)
```

### Triage Prompt

```
You are monitoring a Slack thread you previously participated in.
A new message arrived. Decide whether you should respond.

Respond YES if:
- You are directly addressed by name or role
- Someone asks a question you can answer
- The conversation needs your input to move forward
- You're being asked to take an action

Respond NO if:
- People are talking to each other
- The message is an acknowledgment (ok, thanks, got it)
- Your input would not add value
- The conversation is proceeding fine without you

Thread context:
{thread_history_summary}

New message:
{message_text}

Respond with exactly YES or NO.
```

**Response parsing:** Starts with "YES" (case-insensitive) = respond. Anything else = skip. Fail silent — users can always @mention to force a response.

### Config

```toml
[channels_config.slack]
triage_model = "gemini-2.5-flash-lite"   # Optional. Disables triage when absent.
```

When `triage_model` is `None`, thread-participant messages without @mention are buffered silently (same as today). Feature is opt-in.

## Files

1. `src/config/schema.rs` — Add `triage_model: Option<String>` to `SlackConfig`
2. `src/channels/traits.rs` — Add `triage_required: bool` to `ChannelMessage` (default false)
3. `src/channels/slack.rs` — Replace `is_implicit_mention` with participation set. Set `triage_required = true` on participant messages. Track `participated_threads: HashSet<String>`.
4. `src/channels/mod.rs` — Triage check in orchestration loop. Cheap LLM call via provider. Parse YES/NO. Skip or proceed.

## What Doesn't Change

- Explicit @mentions always respond (no triage)
- `mention_only` config still gates mention checking
- Pending history buffer still works — triage-skipped messages get buffered
- Other channels (Telegram, Discord) unaffected — `triage_required` defaults to false
- No new dependencies

## Rollback

Remove `triage_model` from config. Thread-participant messages revert to silent buffering. The participation set tracking is harmless without triage.

## Testing Strategy

- Unit tests for participation set (insert on send, lookup on receive, empty after init)
- Unit tests for triage response parsing (YES/yes/Yes → true, NO/no/empty/error → false)
- Integration test: thread-participant message with triage_required flag
- Existing mention tests must pass unchanged
