# Slack Socket Mode + Channel Improvements — Design

**Date:** 2026-02-26
**Scope:** `src/channels/slack.rs`, `src/config/schema.rs`, `SendMessage` struct

## Problem

SlackChannel's `listen()` polls `conversations.history` every 3 seconds for every accessible channel. This burns ~200 API calls/minute, triggers rate limit spirals after any usage spike, and imposes a 3-second latency floor on message detection.

## Solution

Replace polling with Slack Socket Mode. Slack pushes message events over a WebSocket. Five supporting improvements ship alongside.

## Architecture

### Socket Mode Connection Lifecycle

`listen()` POSTs to `apps.connections.open` with the `app_token` to obtain a single-use WebSocket URL. It connects via `tokio-tungstenite`. Slack sends a `hello` frame to confirm the connection.

In steady state, Slack pushes JSON envelopes with `"type": "events_api"`. Each envelope contains a `payload.event` with message data. The handler acknowledges every envelope within 3 seconds by sending `{"envelope_id": "..."}` back over the WebSocket.

After acknowledgment, the event passes through the existing filter pipeline: skip bot messages (`bot_id` field), skip own messages (`user == bot_user_id`), allowlist check, mention gating, thread hydration. The only change from today is the message source.

On WebSocket close or error, reconnect with exponential backoff: 1s base, double each failure, cap at 60s, reset on successful `hello`. Add random jitter (0-500ms) to each delay.

On channel shutdown (mpsc sender dropped), close the WebSocket and exit.

### Struct Changes

**`SlackConfig`:**
- `app_token` changes from `Option<String>` to `String` (required)

**`SlackChannel`:**
```
  bot_token: String
+ app_token: String
+ client: reqwest::Client          // shared, replaces per-call http_client()
  channel_id: Option<String>
  allowed_users: Vec<String>
  mention_only: bool
  mention_regex: Option<regex::Regex>
  participated_threads: std::sync::Mutex<HashSet<String>>
```

**`SendMessage`:**
```
+ ack_reaction_ts: Option<String>  // original message ts for reaction removal
```

The `http_client()` method is removed. All methods use `self.client`. The shared client is built once in the constructor via `build_runtime_proxy_client("channel.slack")`.

### Deleted Code

- `list_accessible_channels()` — Socket Mode receives events from all channels the app belongs to
- `slack_now_ts()`, `ensure_poll_cursor()` — polling cursor machinery
- `extract_channel_ids()` — channel discovery helper
- The entire polling loop in `listen()`

### Retry Logic

A `slack_api_request()` helper function handles retries for all outbound Slack API calls: `send()`, `fetch_thread_replies()`, `reactions.add`, `reactions.remove`.

Behavior:
- HTTP 429: read `Retry-After` header, wait that duration plus jitter (0-500ms), retry
- 429 without `Retry-After`: wait 5s, retry
- Slack `"ok": false` with `"error": "ratelimited"`: treat as 429
- Max 3 retries, then propagate error
- Non-rate-limit errors are not retried

### Reaction Lifecycle

Two states: `eyes` on receive, remove after reply.

1. `listen()` receives a message from the WebSocket
2. After filters pass, calls `reactions.add` with `eyes` on the original message
3. Stores the message `ts` in `ChannelMessage`, propagated to `SendMessage.ack_reaction_ts`
4. `send()` posts the reply, then calls `reactions.remove` for `eyes` if `ack_reaction_ts` is present
5. Reaction failures log at `warn` but never fail the message pipeline

### Message Chunking

A safety net in `send()` for oversized responses. Threshold: 4,000 characters.

1. If content length is under 4,000, post normally
2. If over, split at the last `\n\n` before the limit
3. If no paragraph break, fall back to last `\n`
4. If no newline, hard split at 4,000
5. Post each chunk sequentially to the same channel and thread
6. Remove the ack reaction after the final chunk

### Error Handling and Edge Cases

**Envelope deduplication:** Track the last ~100 `envelope_id` values. Skip duplicates after reconnect. Slack redelivers unacknowledged envelopes.

**Startup failure:** If `apps.connections.open` fails on first attempt, `listen()` returns an error. The agent runtime sees a channel startup failure. Mid-operation failures use the backoff loop.

**Channel scoping:** Socket Mode delivers events for all channels the app belongs to. If `channel_id` is configured, filter by comparing `event.channel`. If `None`/`"*"`, accept all.

**Thread hydration:** Still calls `conversations.replies` per inbound threaded message. Uses the retry helper.

**Participated threads cap:** Cap the `HashSet` at 1,000 entries. Evict oldest when full to prevent unbounded memory growth.

## Dependencies

**New:**
- `tokio-tungstenite` — WebSocket client, async/tokio-native
- `futures-util` — `StreamExt`/`SinkExt` for WebSocket stream (likely already transitive)

**No other new dependencies.**

## Testing

Unit tests (no network):
- Envelope parsing: valid `events_api` → extracted message fields
- Envelope parsing: unknown type → skipped
- Envelope deduplication: same ID twice → second skipped
- Channel scoping: event from wrong channel → skipped
- Message chunking: under 4,000 → single chunk
- Message chunking: over 4,000 with `\n\n` → splits at paragraph
- Message chunking: over 4,000 with only `\n` → splits at newline
- Message chunking: over 4,000 with no newlines → hard split
- Retry: simulated 429 with `Retry-After` → correct delay
- Retry: simulated ratelimited JSON → same as 429
- Participated threads cap: 1,001 inserts → size stays at 1,000

Existing tests to update:
- `SlackChannel::new()` signature (takes `app_token`)
- Remove tests for deleted helpers (`ensure_poll_cursor`, `extract_channel_ids`)

Integration tests are out of scope. Socket Mode requires a live Slack connection.

## Not in Scope

- Changes to how messages are sent (still `chat.postMessage` via bot token)
- Changes to `conversations.history` in external CLI tools
- Backward compatibility with polling (no `app_token` = startup error)
- OpenClaw-style multi-state reaction machine (can iterate later)
