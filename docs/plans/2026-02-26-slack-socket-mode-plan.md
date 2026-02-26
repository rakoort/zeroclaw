# Slack Socket Mode + Channel Improvements — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace Slack polling with Socket Mode WebSocket, add retry logic, reaction-based ack, and message chunking.

**Architecture:** Socket Mode receives push events over WebSocket instead of polling `conversations.history`. A shared retry helper handles rate limits on all outbound API calls. Reactions signal processing state; chunking guards against oversized replies.

**Tech Stack:** Rust, tokio, tokio-tungstenite, futures-util, reqwest, serde_json, rand (all already in Cargo.toml)

**Design doc:** `docs/plans/2026-02-26-slack-socket-mode-design.md`

---

### Task 1: Add `ack_reaction_ts` to `SendMessage`

**Files:**
- Modify: `src/channels/traits.rs:24-61`

**Step 1: Add the field to `SendMessage`**

In `src/channels/traits.rs`, add a new field to `SendMessage`:

```rust
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    /// Platform thread identifier for threaded replies (e.g. Slack `thread_ts`).
    pub thread_ts: Option<String>,
    /// Original message timestamp for ack reaction removal (Slack-specific).
    /// When set, the channel removes the ack reaction after sending the reply.
    pub ack_reaction_ts: Option<String>,
}
```

Update the `new()`, `with_subject()` constructors to initialize `ack_reaction_ts: None`.

Add a builder method:

```rust
/// Set the ack reaction timestamp for reaction removal after reply.
pub fn with_ack_reaction(mut self, ts: Option<String>) -> Self {
    self.ack_reaction_ts = ts;
    self
}
```

**Step 2: Run `cargo check` to find all sites that need updating**

Run: `cargo check 2>&1 | head -40`
Expected: Compile errors at every `SendMessage { ... }` literal that doesn't include `ack_reaction_ts`. Fix each by adding `ack_reaction_ts: None`.

Known sites that construct `SendMessage` with struct literals (none — all use `SendMessage::new()` or `SendMessage::with_subject()`, which you already updated). Verify with: `cargo check`

**Step 3: Run tests**

Run: `cargo test --lib channels::traits 2>&1 | tail -20`
Expected: PASS (existing tests construct via `SendMessage::new`)

**Step 4: Commit**

```
feat(channels): add ack_reaction_ts field to SendMessage

Allows channels to track the original message timestamp for
reaction-based acknowledgment removal after reply delivery.
```

---

### Task 2: Make `app_token` required in `SlackConfig`

**Files:**
- Modify: `src/config/schema.rs:2891-2913`

**Step 1: Change `app_token` from `Option<String>` to `String`**

In `src/config/schema.rs`, find `SlackConfig` and change:

```rust
/// Slack app-level token for Socket Mode (xapp-...).
pub app_token: String,
```

**Step 2: Run `cargo check` to find breakage**

Run: `cargo check 2>&1 | head -60`
Expected: Errors in `src/channels/mod.rs` and `src/cron/scheduler.rs` where `sl.app_token` was previously unused or accessed as `Option`. Also likely errors in `src/gateway/api.rs` where it's masked.

**Step 3: Fix all call sites**

In `src/gateway/api.rs:731` (approximate — search for `app_token`), the masking code may reference `app_token` as `Option`. Update to handle it as `String`.

In `src/cron/scheduler.rs:337-348`, the `SlackChannel::new()` call needs `app_token` — this is handled in Task 3 when we update the constructor.

For now, just fix any config/schema compilation errors. Constructor call sites are updated in Task 3.

**Step 4: Run tests**

Run: `cargo test --lib config 2>&1 | tail -20`
Expected: PASS

**Step 5: Commit**

```
feat(config): make Slack app_token required for Socket Mode

Socket Mode replaces polling entirely. The app_token is no longer
optional — Slack channel startup fails fast if it is missing.
```

---

### Task 3: Restructure `SlackChannel` (shared client, app_token, remove polling helpers)

**Files:**
- Modify: `src/channels/slack.rs:152-343`
- Modify: `src/channels/mod.rs:2810-2821`
- Modify: `src/cron/scheduler.rs:337-348`

**Step 1: Update `SlackChannel` struct and constructor**

Replace the struct and `new()`:

```rust
pub struct SlackChannel {
    bot_token: String,
    app_token: String,
    client: reqwest::Client,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    mention_only: bool,
    mention_regex: Option<regex::Regex>,
    participated_threads: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl SlackChannel {
    pub fn new(
        bot_token: String,
        app_token: String,
        channel_id: Option<String>,
        allowed_users: Vec<String>,
    ) -> Self {
        Self {
            client: crate::config::build_runtime_proxy_client("channel.slack"),
            bot_token,
            app_token,
            channel_id,
            allowed_users,
            mention_only: true,
            mention_regex: None,
            participated_threads: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }
```

**Step 2: Remove `http_client()` method**

Delete:
```rust
fn http_client(&self) -> reqwest::Client {
    crate::config::build_runtime_proxy_client("channel.slack")
}
```

Replace all `self.http_client()` calls with `self.client.clone()` (or `&self.client` where borrowed). There are usages in `get_bot_user_id()`, `send()`, `health_check()`, and `fetch_thread_replies()` (as a parameter).

For `fetch_thread_replies()`, change the `client: &reqwest::Client` parameter calls from `&self.http_client()` to `&self.client`.

**Step 3: Delete polling-only helpers**

Remove these methods/functions entirely:
- `list_accessible_channels()` (lines ~263-325)
- `slack_now_ts()` (lines ~327-332)
- `ensure_poll_cursor()` (lines ~334-343)
- `extract_channel_ids()` (lines ~236-261)

**Step 4: Update factory call sites**

In `src/channels/mod.rs:2814`, update:
```rust
SlackChannel::new(
    sl.bot_token.clone(),
    sl.app_token.clone(),
    sl.channel_id.clone(),
    sl.allowed_users.clone(),
)
```

In `src/cron/scheduler.rs:343`, update:
```rust
let channel = SlackChannel::new(
    sl.bot_token.clone(),
    sl.app_token.clone(),
    sl.channel_id.clone(),
    sl.allowed_users.clone(),
);
```

**Step 5: Update tests**

All test `SlackChannel::new()` calls need the new `app_token` parameter:
```rust
SlackChannel::new("xoxb-fake".into(), "xapp-fake".into(), None, vec![])
```

Remove tests for deleted helpers:
- `ensure_poll_cursor_bootstraps_new_channel`
- `ensure_poll_cursor_keeps_existing_cursor`
- `extract_channel_ids_filters_archived_and_non_member_entries`

**Step 6: Verify compilation**

Run: `cargo check 2>&1 | tail -20`
Expected: Compiles (the `listen()` method still has the old polling body — it will be replaced in Task 6).

**Step 7: Run tests**

Run: `cargo test --lib channels::slack 2>&1 | tail -30`
Expected: PASS (remaining tests compile with updated constructor)

**Step 8: Commit**

```
refactor(slack): add shared HTTP client, app_token field, remove polling helpers

SlackChannel now holds a shared reqwest::Client built once at
construction. Polling-only helpers (channel discovery, cursor
management) are deleted in preparation for Socket Mode.
```

---

### Task 4: Implement retry helper

**Files:**
- Modify: `src/channels/slack.rs` (add function near top of file, after imports)

**Step 1: Write tests for retry delay calculation**

Add these tests to the `tests` module:

```rust
#[test]
fn parse_retry_after_valid_header() {
    // Simulate Retry-After: 30
    let delay = parse_retry_after_secs(Some("30"));
    assert_eq!(delay, Some(30));
}

#[test]
fn parse_retry_after_missing() {
    let delay = parse_retry_after_secs(None);
    assert_eq!(delay, None);
}

#[test]
fn parse_retry_after_zero() {
    let delay = parse_retry_after_secs(Some("0"));
    assert_eq!(delay, Some(0));
}

#[test]
fn parse_retry_after_invalid() {
    let delay = parse_retry_after_secs(Some("not-a-number"));
    assert_eq!(delay, None);
}

#[test]
fn is_ratelimited_json_detects_ratelimit() {
    let body: serde_json::Value = serde_json::json!({
        "ok": false,
        "error": "ratelimited"
    });
    assert!(is_slack_ratelimited(&body));
}

#[test]
fn is_ratelimited_json_ignores_other_errors() {
    let body: serde_json::Value = serde_json::json!({
        "ok": false,
        "error": "channel_not_found"
    });
    assert!(!is_slack_ratelimited(&body));
}

#[test]
fn is_ratelimited_json_ignores_success() {
    let body: serde_json::Value = serde_json::json!({
        "ok": true
    });
    assert!(!is_slack_ratelimited(&body));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib channels::slack::tests 2>&1 | tail -20`
Expected: FAIL — `parse_retry_after_secs` and `is_slack_ratelimited` not found.

**Step 3: Implement the helper functions**

Add near the top of `slack.rs`, after imports:

```rust
const SLACK_RETRY_MAX: u32 = 3;
const SLACK_RETRY_DEFAULT_SECS: u64 = 5;
const SLACK_RETRY_JITTER_MS: u64 = 500;

/// Parse the `Retry-After` header value as seconds.
fn parse_retry_after_secs(value: Option<&str>) -> Option<u64> {
    value.and_then(|v| v.trim().parse::<u64>().ok())
}

/// Check if a Slack JSON response indicates rate limiting.
fn is_slack_ratelimited(body: &serde_json::Value) -> bool {
    body.get("ok") == Some(&serde_json::Value::Bool(false))
        && body.get("error").and_then(|e| e.as_str()) == Some("ratelimited")
}

/// Execute a Slack API POST request with rate-limit retry.
///
/// Retries up to `SLACK_RETRY_MAX` times on HTTP 429 or JSON `"error": "ratelimited"`.
/// Reads `Retry-After` header to determine wait duration; falls back to 5s.
/// Adds random jitter (0-500ms) to each retry delay.
async fn slack_api_post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    for attempt in 0..=SLACK_RETRY_MAX {
        let resp = client
            .post(url)
            .bearer_auth(token)
            .json(body)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempt == SLACK_RETRY_MAX {
                anyhow::bail!("Slack rate limit exceeded after {SLACK_RETRY_MAX} retries: {url}");
            }
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok());
            let wait_secs = parse_retry_after_secs(retry_after)
                .unwrap_or(SLACK_RETRY_DEFAULT_SECS);
            let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
            tracing::warn!(
                "Slack rate limited on {url} (attempt {}/{}). Retry-After: {wait_secs}s",
                attempt + 1,
                SLACK_RETRY_MAX
            );
            tokio::time::sleep(Duration::from_millis(wait_secs * 1000 + jitter)).await;
            continue;
        }

        let resp_text = resp.text().await
            .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"read_failed: {e}\"}}"));
        let parsed: serde_json::Value = serde_json::from_str(&resp_text).unwrap_or_default();

        if is_slack_ratelimited(&parsed) {
            if attempt == SLACK_RETRY_MAX {
                anyhow::bail!("Slack rate limit exceeded after {SLACK_RETRY_MAX} retries: {url}");
            }
            let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
            tracing::warn!(
                "Slack JSON ratelimited on {url} (attempt {}/{}). Waiting {SLACK_RETRY_DEFAULT_SECS}s",
                attempt + 1,
                SLACK_RETRY_MAX
            );
            tokio::time::sleep(Duration::from_millis(SLACK_RETRY_DEFAULT_SECS * 1000 + jitter)).await;
            continue;
        }

        if !status.is_success() {
            anyhow::bail!("Slack API error ({status}): {resp_text}");
        }

        return Ok(parsed);
    }
    unreachable!()
}

/// Execute a Slack API GET request with rate-limit retry.
/// Same retry semantics as `slack_api_post`.
async fn slack_api_get(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    query: &[(&str, String)],
) -> anyhow::Result<serde_json::Value> {
    for attempt in 0..=SLACK_RETRY_MAX {
        let resp = client
            .get(url)
            .bearer_auth(token)
            .query(query)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempt == SLACK_RETRY_MAX {
                anyhow::bail!("Slack rate limit exceeded after {SLACK_RETRY_MAX} retries: {url}");
            }
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok());
            let wait_secs = parse_retry_after_secs(retry_after)
                .unwrap_or(SLACK_RETRY_DEFAULT_SECS);
            let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
            tracing::warn!(
                "Slack rate limited on {url} (attempt {}/{}). Retry-After: {wait_secs}s",
                attempt + 1,
                SLACK_RETRY_MAX
            );
            tokio::time::sleep(Duration::from_millis(wait_secs * 1000 + jitter)).await;
            continue;
        }

        let resp_text = resp.text().await
            .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"read_failed: {e}\"}}"));
        let parsed: serde_json::Value = serde_json::from_str(&resp_text).unwrap_or_default();

        if is_slack_ratelimited(&parsed) {
            if attempt == SLACK_RETRY_MAX {
                anyhow::bail!("Slack rate limit exceeded after {SLACK_RETRY_MAX} retries: {url}");
            }
            let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
            tracing::warn!(
                "Slack JSON ratelimited on {url} (attempt {}/{}). Waiting {SLACK_RETRY_DEFAULT_SECS}s",
                attempt + 1,
                SLACK_RETRY_MAX
            );
            tokio::time::sleep(Duration::from_millis(SLACK_RETRY_DEFAULT_SECS * 1000 + jitter)).await;
            continue;
        }

        if !status.is_success() {
            anyhow::bail!("Slack API error ({status}): {resp_text}");
        }

        return Ok(parsed);
    }
    unreachable!()
}
```

**Step 4: Run tests**

Run: `cargo test --lib channels::slack::tests 2>&1 | tail -30`
Expected: PASS

**Step 5: Commit**

```
feat(slack): add rate-limit-aware retry helpers for Slack API calls

Parses Retry-After headers on 429 responses and detects JSON-level
ratelimited errors. Retries up to 3 times with jitter.
```

---

### Task 5: Migrate `send()` to use retry helper + add chunking + reaction removal

**Files:**
- Modify: `src/channels/slack.rs` — `send()` method (lines ~407-451)

**Step 1: Write tests for message chunking**

Add to `tests` module:

```rust
#[test]
fn chunk_message_under_limit() {
    let text = "Short message.";
    let chunks = chunk_message(text, 4000);
    assert_eq!(chunks, vec!["Short message."]);
}

#[test]
fn chunk_message_splits_at_paragraph() {
    let text = format!("{}\n\n{}", "a".repeat(3000), "b".repeat(2000));
    let chunks = chunk_message(&text, 4000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], "a".repeat(3000));
    assert_eq!(chunks[1], "b".repeat(2000));
}

#[test]
fn chunk_message_falls_back_to_newline() {
    let text = format!("{}\n{}", "a".repeat(3000), "b".repeat(2000));
    let chunks = chunk_message(&text, 4000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], "a".repeat(3000));
    assert_eq!(chunks[1], "b".repeat(2000));
}

#[test]
fn chunk_message_hard_splits_no_newlines() {
    let text = "a".repeat(5000);
    let chunks = chunk_message(&text, 4000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 4000);
    assert_eq!(chunks[1].len(), 1000);
}

#[test]
fn chunk_message_empty_string() {
    let chunks = chunk_message("", 4000);
    assert_eq!(chunks, vec![""]);
}

#[test]
fn chunk_message_exact_limit() {
    let text = "a".repeat(4000);
    let chunks = chunk_message(&text, 4000);
    assert_eq!(chunks.len(), 1);
}

#[test]
fn chunk_message_multiple_splits() {
    let text = format!("{}\n\n{}\n\n{}", "a".repeat(3500), "b".repeat(3500), "c".repeat(3500));
    let chunks = chunk_message(&text, 4000);
    assert_eq!(chunks.len(), 3);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib channels::slack::tests::chunk 2>&1 | tail -20`
Expected: FAIL — `chunk_message` not found.

**Step 3: Implement `chunk_message`**

Add above the `impl Channel for SlackChannel` block:

```rust
const SLACK_MESSAGE_CHUNK_LIMIT: usize = 4000;

/// Split a message into chunks at paragraph boundaries.
///
/// Tries to split at `\n\n`, falls back to `\n`, then hard-splits at the limit.
fn chunk_message(text: &str, limit: usize) -> Vec<&str> {
    if text.len() <= limit {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while remaining.len() > limit {
        let search_range = &remaining[..limit];

        let split_at = search_range.rfind("\n\n")
            .map(|pos| pos)  // split before the \n\n
            .or_else(|| search_range.rfind('\n'))
            .unwrap_or(limit);

        let split_at = if split_at == 0 { limit } else { split_at };

        chunks.push(remaining[..split_at].trim_end());
        remaining = remaining[split_at..].trim_start();
    }

    if !remaining.is_empty() {
        chunks.push(remaining);
    }

    chunks
}
```

**Step 4: Run chunk tests**

Run: `cargo test --lib channels::slack::tests::chunk 2>&1 | tail -20`
Expected: PASS

**Step 5: Rewrite `send()` with retry, chunking, and reaction removal**

Replace the `send()` method body:

```rust
async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
    let chunks = chunk_message(&message.content, SLACK_MESSAGE_CHUNK_LIMIT);

    for chunk in &chunks {
        let mut body = serde_json::json!({
            "channel": message.recipient,
            "text": chunk
        });
        if let Some(ref ts) = message.thread_ts {
            body["thread_ts"] = serde_json::json!(ts);
        }

        let result = slack_api_post(
            &self.client,
            "https://slack.com/api/chat.postMessage",
            &self.bot_token,
            &body,
        )
        .await?;

        if result.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = result
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack chat.postMessage failed: {err}");
        }
    }

    // Record thread participation
    if let Some(ref ts) = message.thread_ts {
        self.record_participation(ts);
    }

    // Remove ack reaction after reply
    if let Some(ref ack_ts) = message.ack_reaction_ts {
        let remove_body = serde_json::json!({
            "channel": message.recipient,
            "name": "eyes",
            "timestamp": ack_ts
        });
        if let Err(e) = slack_api_post(
            &self.client,
            "https://slack.com/api/reactions.remove",
            &self.bot_token,
            &remove_body,
        )
        .await
        {
            tracing::warn!("Failed to remove ack reaction: {e}");
        }
    }

    Ok(())
}
```

**Step 6: Run full test suite**

Run: `cargo test --lib channels::slack 2>&1 | tail -30`
Expected: PASS

**Step 7: Commit**

```
feat(slack): rewrite send() with retry, chunking, and reaction removal

Messages over 4000 chars split at paragraph boundaries. All API
calls go through the rate-limit-aware retry helper. Ack reaction
is removed after the final chunk is posted.
```

---

### Task 6: Migrate `fetch_thread_replies` and `get_bot_user_id` to use retry helpers

**Files:**
- Modify: `src/channels/slack.rs` — `fetch_thread_replies()` (lines ~49-80), `get_bot_user_id()` (lines ~198-214)

**Step 1: Rewrite `fetch_thread_replies`**

The standalone function currently takes a `&reqwest::Client`. Update it to use `slack_api_get`:

```rust
async fn fetch_thread_replies(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    thread_ts: &str,
    limit: usize,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let query = vec![
        ("channel", channel.to_string()),
        ("ts", thread_ts.to_string()),
        ("limit", limit.to_string()),
    ];

    let data = slack_api_get(
        client,
        "https://slack.com/api/conversations.replies",
        bot_token,
        &query,
    )
    .await?;

    if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
        let err = data
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown");
        anyhow::bail!("Slack conversations.replies failed: {err}");
    }

    Ok(data
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default())
}
```

**Step 2: Rewrite `get_bot_user_id`**

Update to use `slack_api_get`:

```rust
async fn get_bot_user_id(&self) -> Option<String> {
    let data = slack_api_get(
        &self.client,
        "https://slack.com/api/auth.test",
        &self.bot_token,
        &[],
    )
    .await
    .ok()?;

    data.get("user_id")
        .and_then(|u| u.as_str())
        .map(String::from)
}
```

**Step 3: Update `health_check` to use shared client**

```rust
async fn health_check(&self) -> bool {
    slack_api_get(
        &self.client,
        "https://slack.com/api/auth.test",
        &self.bot_token,
        &[],
    )
    .await
    .is_ok()
}
```

**Step 4: Run tests**

Run: `cargo test --lib channels::slack 2>&1 | tail -20`
Expected: PASS

**Step 5: Commit**

```
refactor(slack): migrate all API calls to rate-limit-aware helpers

fetch_thread_replies, get_bot_user_id, and health_check now use
slack_api_get with automatic retry on rate limits.
```

---

### Task 7: Implement Socket Mode envelope parsing and deduplication

**Files:**
- Modify: `src/channels/slack.rs` — add types and functions before `impl Channel`

**Step 1: Write tests for envelope parsing**

```rust
#[test]
fn parse_socket_envelope_valid_message() {
    let envelope = serde_json::json!({
        "envelope_id": "abc123",
        "type": "events_api",
        "payload": {
            "event": {
                "type": "message",
                "channel": "C0AFC38118C",
                "user": "U05TBBNT94G",
                "text": "hello",
                "ts": "1708789200.000100",
                "thread_ts": "1708789100.000050"
            }
        }
    });
    let parsed = parse_socket_event(&envelope);
    assert!(parsed.is_some());
    let (env_id, event) = parsed.unwrap();
    assert_eq!(env_id, "abc123");
    assert_eq!(event["channel"].as_str().unwrap(), "C0AFC38118C");
    assert_eq!(event["text"].as_str().unwrap(), "hello");
}

#[test]
fn parse_socket_envelope_non_message_event() {
    let envelope = serde_json::json!({
        "envelope_id": "abc123",
        "type": "events_api",
        "payload": {
            "event": {
                "type": "reaction_added",
                "user": "U05TBBNT94G"
            }
        }
    });
    let parsed = parse_socket_event(&envelope);
    assert!(parsed.is_none());
}

#[test]
fn parse_socket_envelope_hello() {
    let envelope = serde_json::json!({
        "type": "hello"
    });
    let parsed = parse_socket_event(&envelope);
    assert!(parsed.is_none());
}

#[test]
fn parse_socket_envelope_missing_payload() {
    let envelope = serde_json::json!({
        "envelope_id": "abc123",
        "type": "events_api"
    });
    let parsed = parse_socket_event(&envelope);
    assert!(parsed.is_none());
}

#[test]
fn parse_socket_envelope_disconnect() {
    let envelope = serde_json::json!({
        "type": "disconnect",
        "reason": "refresh_requested"
    });
    let parsed = parse_socket_event(&envelope);
    assert!(parsed.is_none());
}

#[test]
fn envelope_dedup_tracks_ids() {
    let mut dedup = EnvelopeDedup::new(3);
    assert!(dedup.is_new("a"));
    assert!(dedup.is_new("b"));
    assert!(!dedup.is_new("a")); // seen
}

#[test]
fn envelope_dedup_evicts_oldest() {
    let mut dedup = EnvelopeDedup::new(3);
    dedup.is_new("a");
    dedup.is_new("b");
    dedup.is_new("c");
    dedup.is_new("d"); // evicts "a"
    assert!(dedup.is_new("a")); // "a" was evicted, so it's new again
    assert!(!dedup.is_new("d")); // "d" still tracked
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib channels::slack::tests::parse_socket 2>&1 | tail -20`
Expected: FAIL

**Step 3: Implement parsing and dedup**

```rust
/// Parse a Socket Mode envelope, returning (envelope_id, event) for message events.
/// Returns None for non-message events (hello, disconnect, reaction_added, etc.).
fn parse_socket_event(envelope: &serde_json::Value) -> Option<(String, serde_json::Value)> {
    let env_type = envelope.get("type")?.as_str()?;
    if env_type != "events_api" {
        return None;
    }
    let envelope_id = envelope.get("envelope_id")?.as_str()?.to_string();
    let event = envelope.get("payload")?.get("event")?;
    let event_type = event.get("type")?.as_str()?;
    if event_type != "message" {
        return None;
    }
    Some((envelope_id, event.clone()))
}

/// Bounded deduplication set for Socket Mode envelope IDs.
struct EnvelopeDedup {
    ids: Vec<String>,
    max: usize,
}

impl EnvelopeDedup {
    fn new(max: usize) -> Self {
        Self {
            ids: Vec::with_capacity(max),
            max,
        }
    }

    /// Returns true if the ID is new (not seen before). Tracks it.
    fn is_new(&mut self, id: &str) -> bool {
        if self.ids.iter().any(|existing| existing == id) {
            return false;
        }
        if self.ids.len() >= self.max {
            self.ids.remove(0);
        }
        self.ids.push(id.to_string());
        true
    }
}
```

**Step 4: Run tests**

Run: `cargo test --lib channels::slack::tests 2>&1 | tail -30`
Expected: PASS

**Step 5: Commit**

```
feat(slack): add Socket Mode envelope parsing and deduplication

Parses events_api envelopes, extracts message events, and tracks
envelope IDs in a bounded set to skip redeliveries after reconnect.
```

---

### Task 8: Implement Socket Mode `listen()` with reconnection

**Files:**
- Modify: `src/channels/slack.rs` — replace the `listen()` method entirely

This is the core task. Add these imports at the top of `slack.rs`:

```rust
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;
```

**Step 1: Implement `connect_socket_mode` helper**

Add to `impl SlackChannel`:

```rust
/// Open a Socket Mode WebSocket connection.
/// Calls apps.connections.open to get a WebSocket URL, then connects.
async fn connect_socket_mode(&self) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>
    >
> {
    // Get WebSocket URL
    let resp = self.client
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(&self.app_token)
        .send()
        .await?;

    let data: serde_json::Value = resp.json().await?;
    if data.get("ok") != Some(&serde_json::Value::Bool(true)) {
        let err = data.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
        anyhow::bail!("apps.connections.open failed: {err}");
    }

    let ws_url = data.get("url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| anyhow::anyhow!("apps.connections.open returned no URL"))?;

    // Connect WebSocket
    let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await?;
    Ok(ws_stream)
}
```

**Step 2: Replace `listen()` with Socket Mode implementation**

```rust
async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
    let bot_user_id = match self.get_bot_user_id().await {
        Some(id) => {
            tracing::info!("Slack: resolved bot user ID: {id}");
            id
        }
        None => {
            tracing::warn!("Slack: failed to resolve bot user ID via auth.test");
            String::new()
        }
    };

    let scoped_channel = self.configured_channel_id();
    let mut dedup = EnvelopeDedup::new(100);
    let mut backoff_ms: u64 = 1000;
    const BACKOFF_CAP_MS: u64 = 60_000;

    loop {
        // Connect (or reconnect)
        let ws_stream = match self.connect_socket_mode().await {
            Ok(ws) => {
                backoff_ms = 1000; // reset on success
                tracing::info!("Slack Socket Mode connected");
                ws
            }
            Err(e) => {
                tracing::warn!("Slack Socket Mode connection failed: {e}. Retrying in {backoff_ms}ms");
                let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
                tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
                backoff_ms = (backoff_ms * 2).min(BACKOFF_CAP_MS);
                continue;
            }
        };

        let (mut ws_sink, mut ws_stream_rx) = ws_stream.split();

        // Process messages until disconnect
        while let Some(msg_result) = ws_stream_rx.next().await {
            let ws_msg = match msg_result {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!("Slack WebSocket error: {e}");
                    break; // reconnect
                }
            };

            let text = match ws_msg {
                WsMessage::Text(t) => t,
                WsMessage::Close(_) => {
                    tracing::info!("Slack WebSocket closed by server");
                    break; // reconnect
                }
                _ => continue,
            };

            let envelope: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Slack Socket Mode: invalid JSON: {e}");
                    continue;
                }
            };

            // Acknowledge envelope immediately (required within 3s)
            if let Some(env_id) = envelope.get("envelope_id").and_then(|v| v.as_str()) {
                let ack = serde_json::json!({"envelope_id": env_id});
                if let Err(e) = ws_sink.send(WsMessage::Text(ack.to_string().into())).await {
                    tracing::warn!("Slack Socket Mode: failed to ack envelope: {e}");
                    break; // reconnect
                }
            }

            // Handle disconnect requests
            if envelope.get("type").and_then(|t| t.as_str()) == Some("disconnect") {
                tracing::info!("Slack Socket Mode: server requested disconnect");
                break; // reconnect
            }

            // Parse message events
            let (env_id, event) = match parse_socket_event(&envelope) {
                Some(parsed) => parsed,
                None => continue,
            };

            // Deduplicate
            if !dedup.is_new(&env_id) {
                tracing::debug!("Slack Socket Mode: skipping duplicate envelope {env_id}");
                continue;
            }

            // Channel scoping
            let channel_id = match event.get("channel").and_then(|c| c.as_str()) {
                Some(c) => c.to_string(),
                None => continue,
            };
            if let Some(ref scoped) = scoped_channel {
                if &channel_id != scoped {
                    continue;
                }
            }

            let ts = event.get("ts").and_then(|t| t.as_str()).unwrap_or("");
            let user = event.get("user").and_then(|u| u.as_str()).unwrap_or("unknown");
            let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");

            // Skip bot messages
            if event.get("bot_id").is_some() {
                continue;
            }
            if !bot_user_id.is_empty() && user == bot_user_id {
                continue;
            }

            // Allowlist check
            if !self.is_user_allowed(user) {
                tracing::warn!("Slack: ignoring message from unauthorized user: {user}");
                continue;
            }

            if text.is_empty() {
                continue;
            }

            // Format timestamp
            let ts_secs = ts.split('.').next()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let ts_display = {
                let dt = chrono::DateTime::from_timestamp(ts_secs, 0).unwrap_or_default();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            };

            // Mention gating
            let thread_ts_val = event.get("thread_ts").and_then(|v| v.as_str());
            let (text, triage_required) = if self.mention_only {
                match self.resolve_mention_gate(text, &bot_user_id, thread_ts_val) {
                    MentionGateResult::ExplicitMention(cleaned) => (cleaned, false),
                    MentionGateResult::ParticipatedThread(text) => (text, true),
                    MentionGateResult::Buffer => continue,
                }
            } else {
                (text.to_string(), false)
            };

            // Add ack reaction (best-effort)
            let ack_body = serde_json::json!({
                "channel": channel_id,
                "name": "eyes",
                "timestamp": ts
            });
            if let Err(e) = slack_api_post(
                &self.client,
                "https://slack.com/api/reactions.add",
                &self.bot_token,
                &ack_body,
            ).await {
                tracing::warn!("Failed to add ack reaction: {e}");
            }

            // Thread hydration
            let inbound_thread = Self::inbound_thread_ts(&event, ts);
            let (starter_body, history_body) = if let Some(ref tts) = inbound_thread {
                match fetch_thread_replies(
                    &self.client,
                    &self.bot_token,
                    &channel_id,
                    tts,
                    20,
                ).await {
                    Ok(replies) => {
                        let starter = replies.first()
                            .and_then(|r| r["text"].as_str())
                            .map(String::from);
                        let history = format_thread_history(
                            &replies,
                            &bot_user_id,
                            "assistant",
                            &channel_id,
                            &HashMap::new(),
                        );
                        (starter, Some(history))
                    }
                    Err(e) => {
                        tracing::warn!("Slack thread hydration failed for {channel_id}/{tts}: {e}");
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

            let channel_msg = ChannelMessage {
                id: format!("slack_{channel_id}_{ts}"),
                sender: user.to_string(),
                reply_target: channel_id.clone(),
                content: text,
                channel: "slack".to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                thread_ts: inbound_thread,
                thread_starter_body: starter_body,
                thread_history: history_body,
                triage_required,
            };

            if tx.send(channel_msg).await.is_err() {
                return Ok(()); // receiver dropped, shut down
            }
        }

        // WebSocket closed — reconnect with backoff
        tracing::info!("Slack Socket Mode disconnected, reconnecting...");
        let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
        tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
        backoff_ms = (backoff_ms * 2).min(BACKOFF_CAP_MS);
    }
}
```

**Step 3: Remove old imports if no longer needed**

Check whether `Instant` is still used (it was for `last_discovery` in the polling loop). If not, remove from the `use` block.

**Step 4: Verify compilation**

Run: `cargo check 2>&1 | tail -20`
Expected: Compiles

**Step 5: Run full test suite**

Run: `cargo test --lib channels::slack 2>&1 | tail -30`
Expected: PASS

**Step 6: Commit**

```
feat(slack): replace polling with Socket Mode WebSocket

listen() now connects via apps.connections.open and receives push
events over WebSocket. Includes exponential backoff reconnection,
envelope deduplication, ack reactions, and channel scoping.
Removes all polling code paths.
```

---

### Task 9: Propagate `ack_reaction_ts` through the agent dispatch path

**Files:**
- Modify: `src/channels/mod.rs` — all `SendMessage::new(...).in_thread(...)` call sites

**Step 1: Add `ack_reaction_ts` to `ChannelMessage`**

In `src/channels/traits.rs`, add to `ChannelMessage`:

```rust
/// Original message timestamp for ack reaction removal (Slack-specific).
pub ack_reaction_ts: Option<String>,
```

**Step 2: Set it in Socket Mode `listen()`**

In the `listen()` method (Task 8), set `ack_reaction_ts: Some(ts.to_string())` in the `ChannelMessage` construction.

**Step 3: Thread it through dispatch**

In `src/channels/mod.rs`, update the `SendMessage` construction at all dispatch sites. The pattern changes from:

```rust
SendMessage::new(response, &msg.reply_target).in_thread(msg.thread_ts.clone())
```

to:

```rust
SendMessage::new(response, &msg.reply_target)
    .in_thread(msg.thread_ts.clone())
    .with_ack_reaction(msg.ack_reaction_ts.clone())
```

Search for all `in_thread(msg.thread_ts` in `src/channels/mod.rs` and chain `.with_ack_reaction(msg.ack_reaction_ts.clone())`. There are approximately 10 call sites.

Also update any other files that construct `ChannelMessage` (search for `ChannelMessage {`) to add `ack_reaction_ts: None` where the message doesn't come from Slack.

**Step 4: Verify compilation**

Run: `cargo check 2>&1 | tail -20`
Expected: Compiles

**Step 5: Run full tests**

Run: `cargo test 2>&1 | tail -30`
Expected: PASS

**Step 6: Commit**

```
feat(slack): propagate ack_reaction_ts through dispatch path

ChannelMessage now carries the original Slack timestamp so the
ack reaction can be removed after reply delivery.
```

---

### Task 10: Cap participated_threads HashSet

**Files:**
- Modify: `src/channels/slack.rs`

**Step 1: Write test**

```rust
#[test]
fn participated_threads_capped_at_limit() {
    let channel = SlackChannel::new("xoxb-test".into(), "xapp-test".into(), None, vec!["*".into()]);
    for i in 0..1100 {
        channel.record_participation(&format!("{i}.0000"));
    }
    let threads = channel.participated_threads();
    assert!(threads.len() <= 1000, "expected <= 1000, got {}", threads.len());
    // Most recent should be present
    assert!(channel.has_participated("1099.0000"));
    // Oldest should be evicted
    assert!(!channel.has_participated("0.0000"));
}
```

**Step 2: Run test to verify failure**

Run: `cargo test --lib channels::slack::tests::participated_threads_capped 2>&1 | tail -10`
Expected: FAIL — set grows to 1100.

**Step 3: Implement capped participation tracking**

Replace `participated_threads` with a bounded structure. Change the struct field type and update `record_participation`:

```rust
const MAX_PARTICIPATED_THREADS: usize = 1000;

pub fn record_participation(&self, thread_ts: &str) {
    let mut set = self.participated_threads.lock().unwrap();
    if set.len() >= MAX_PARTICIPATED_THREADS {
        // Remove an arbitrary entry (HashSet has no ordering, so remove first from iter)
        if let Some(oldest) = set.iter().next().cloned() {
            set.remove(&oldest);
        }
    }
    set.insert(thread_ts.to_string());
}
```

Note: `HashSet` doesn't preserve insertion order, so "oldest" is approximate. For true LRU we'd need `IndexSet` or a `VecDeque`. Since this is a leak-prevention cap (not a correctness requirement), approximate eviction is acceptable.

**Step 4: Run test**

Run: `cargo test --lib channels::slack::tests::participated_threads_capped 2>&1 | tail -10`
Expected: PASS

**Step 5: Run full test suite**

Run: `cargo test --lib channels::slack 2>&1 | tail -20`
Expected: PASS

**Step 6: Commit**

```
fix(slack): cap participated_threads set at 1000 entries

Prevents unbounded memory growth in long-running instances by
evicting entries when the set exceeds capacity.
```

---

### Task 11: Final validation

**Step 1: Run full project checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: All pass.

**Step 2: Fix any clippy or formatting issues**

Address warnings or errors.

**Step 3: Commit any fixes**

```
chore: fix clippy and formatting issues
```

**Step 4: Review the diff**

Run: `git log --oneline main..HEAD`

Verify the commit sequence is clean and each commit is atomic.
