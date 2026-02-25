# Thread Participation Triage Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the broken implicit-mention detection with thread participation tracking and LLM-based triage so the bot monitors threads it participates in without replying to every message.

**Architecture:** Add `triage_required` flag to `ChannelMessage`. Track thread participation in `SlackChannel` via `HashSet<String>` populated on send. In the orchestration layer, check the flag and make a cheap LLM call (gemini-2.5-flash-lite) to decide whether to respond. Fail-silent default: if triage errors or says NO, skip the message.

**Tech Stack:** Rust, std::sync::Mutex, HashSet, existing Provider trait for triage LLM call.

---

### Task 1: Add `triage_required` field to `ChannelMessage`

**Files:**
- Modify: `src/channels/traits.rs:4-17` (ChannelMessage struct)
- Modify: `src/channels/traits.rs:149-265` (tests that construct ChannelMessage)

**Step 1: Write the failing test**

Add a test in `src/channels/traits.rs` under the existing `mod tests`:

```rust
#[test]
fn channel_message_triage_required_defaults_to_false() {
    let message = ChannelMessage {
        id: "1".into(),
        sender: "tester".into(),
        reply_target: "tester".into(),
        content: "hello".into(),
        channel: "test".into(),
        timestamp: 0,
        thread_ts: None,
        thread_starter_body: None,
        thread_history: None,
        triage_required: false,
    };
    assert!(!message.triage_required);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib channels::traits::tests::channel_message_triage_required_defaults_to_false`
Expected: FAIL — `triage_required` field doesn't exist yet.

**Step 3: Add the field to `ChannelMessage`**

In `src/channels/traits.rs`, add to the `ChannelMessage` struct after `thread_history`:

```rust
/// Whether this message requires LLM triage before responding.
/// Set by channels that detect thread participation without explicit @mention.
#[allow(dead_code)]
pub triage_required: bool,
```

**Step 4: Fix all existing `ChannelMessage` construction sites**

Every place that constructs a `ChannelMessage` needs `triage_required: false`. These are:

- `src/channels/traits.rs` tests (lines 169-178, 187-198) — add `triage_required: false`
- `src/channels/slack.rs` line 621 — add `triage_required: false`
- `src/channels/mod.rs` tests — search for `ChannelMessage {` and add `triage_required: false`
- Any other channel implementations (telegram, discord, etc.) — search for `ChannelMessage {` across the codebase

Use `cargo build` to find all sites — the compiler will report every missing field.

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib channels::traits`
Expected: PASS — all existing tests plus new test pass.

**Step 6: Commit**

```bash
git add src/channels/traits.rs src/channels/slack.rs src/channels/mod.rs
# Plus any other files that construct ChannelMessage
git commit -m "feat(channels): add triage_required field to ChannelMessage"
```

---

### Task 2: Add `triage_model` to `SlackConfig`

**Files:**
- Modify: `src/config/schema.rs:2909-2926` (SlackConfig struct)

**Step 1: Write the failing test**

Add a test in `src/config/schema.rs` (or in a nearby test module) that deserializes a SlackConfig with `triage_model`:

```rust
#[test]
fn slack_config_triage_model_parses() {
    let toml_str = r#"
bot_token = "xoxb-test"
triage_model = "gemini-2.5-flash-lite"
"#;
    let config: SlackConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.triage_model.as_deref(), Some("gemini-2.5-flash-lite"));
}

#[test]
fn slack_config_triage_model_defaults_to_none() {
    let toml_str = r#"
bot_token = "xoxb-test"
"#;
    let config: SlackConfig = toml::from_str(toml_str).unwrap();
    assert!(config.triage_model.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib config::schema -- slack_config_triage`
Expected: FAIL — `triage_model` field doesn't exist.

**Step 3: Add the field**

In `src/config/schema.rs`, add to `SlackConfig` after `mention_regex`:

```rust
/// Optional model for triage decisions on thread-participant messages.
/// When set, the bot makes a cheap LLM call to decide whether to respond
/// to messages in threads it participates in (without explicit @mention).
/// When absent, thread-participant messages are buffered silently.
pub triage_model: Option<String>,
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib config::schema -- slack_config_triage`
Expected: PASS

**Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add triage_model to SlackConfig"
```

---

### Task 3: Add thread participation tracking to `SlackChannel`

**Files:**
- Modify: `src/channels/slack.rs:149-155` (SlackChannel struct)
- Modify: `src/channels/slack.rs:157-180` (SlackChannel::new, with_mention_config)
- Modify: `src/channels/slack.rs:347-386` (send method)
- Modify: `src/channels/slack.rs:750-950` (tests)

**Step 1: Write failing tests for participation tracking**

Add tests in `src/channels/slack.rs` `mod tests`:

```rust
#[test]
fn participated_threads_empty_on_init() {
    let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
    assert!(channel.participated_threads().is_empty());
}

#[test]
fn record_participation_tracks_thread() {
    let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
    channel.record_participation("1234.5678");
    assert!(channel.has_participated("1234.5678"));
}

#[test]
fn has_participated_returns_false_for_unknown_thread() {
    let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
    assert!(!channel.has_participated("unknown.thread"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib channels::slack::tests::participated_threads`
Expected: FAIL — methods don't exist.

**Step 3: Add participation tracking to `SlackChannel`**

Add field to struct:

```rust
pub struct SlackChannel {
    bot_token: String,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    mention_only: bool,
    mention_regex: Option<regex::Regex>,
    participated_threads: std::sync::Mutex<std::collections::HashSet<String>>,
}
```

Initialize in `new()`:

```rust
participated_threads: std::sync::Mutex::new(std::collections::HashSet::new()),
```

Add public methods:

```rust
/// Record that the bot has participated in a thread.
pub fn record_participation(&self, thread_ts: &str) {
    self.participated_threads
        .lock()
        .unwrap()
        .insert(thread_ts.to_string());
}

/// Check if the bot has participated in a thread.
pub fn has_participated(&self, thread_ts: &str) -> bool {
    self.participated_threads
        .lock()
        .unwrap()
        .contains(thread_ts)
}

/// Get the current set of participated threads (for testing).
fn participated_threads(&self) -> std::collections::HashSet<String> {
    self.participated_threads.lock().unwrap().clone()
}
```

**Step 4: Track participation in `send()`**

In the `send()` method (line 347), after the successful send, record participation if the message has a `thread_ts`:

```rust
// Record thread participation for triage tracking
if let Some(ref ts) = message.thread_ts {
    self.record_participation(ts);
}
```

Add this right before the final `Ok(())` at line 385.

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib channels::slack`
Expected: PASS — all existing tests plus new participation tests.

**Step 6: Commit**

```bash
git add src/channels/slack.rs
git commit -m "feat(slack): add thread participation tracking via HashSet"
```

---

### Task 4: Replace `is_implicit_mention` with participation check

**Files:**
- Modify: `src/channels/slack.rs:143-146` (is_implicit_mention function)
- Modify: `src/channels/slack.rs:534-553` (mention gating in listen loop)
- Modify: `src/channels/slack.rs:621-634` (ChannelMessage construction)

This is the core behavior change. The listen loop currently uses `is_implicit_mention(bot_user_id, parent_uid)` which only matches threads the bot started. We replace it with a participation set check.

**Step 1: Write failing tests for new mention gating behavior**

Add tests in `src/channels/slack.rs`:

```rust
#[test]
fn participated_thread_message_sets_triage_required() {
    // When bot has participated in a thread, messages in that thread
    // without explicit @mention should set triage_required = true
    let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
    channel.record_participation("1234.5678");

    // The message is in a participated thread but doesn't @mention the bot
    let text = "hey can someone help with this?";
    let is_explicit = is_mention(text, "U_BOT", None);
    let is_participant = channel.has_participated("1234.5678");

    assert!(!is_explicit, "should not be an explicit mention");
    assert!(is_participant, "should detect participation");
    // triage_required = is_participant && !is_explicit
    assert!(is_participant && !is_explicit);
}

#[test]
fn explicit_mention_in_participated_thread_skips_triage() {
    let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
    channel.record_participation("1234.5678");

    let text = "<@U_BOT> what do you think?";
    let is_explicit = is_mention(text, "U_BOT", None);

    assert!(is_explicit);
    // triage_required should be false when explicitly mentioned
}

#[test]
fn non_participated_thread_message_is_buffered() {
    let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
    // Bot has NOT participated in thread "9999.0000"

    let text = "just chatting";
    let is_explicit = is_mention(text, "U_BOT", None);
    let is_participant = channel.has_participated("9999.0000");

    assert!(!is_explicit);
    assert!(!is_participant);
    // Neither mention nor participant → buffer silently
}
```

**Step 2: Run tests to verify they pass (these are logic tests, they should pass now)**

Run: `cargo test --lib channels::slack::tests::participated_thread`
Expected: PASS (these test the logic, not the wiring)

**Step 3: Update the mention gating in the listen loop**

Replace the mention gating block at lines 534-553 in `src/channels/slack.rs`. The current code:

```rust
let text = if self.mention_only {
    let parent_uid = msg
        .get("parent_user_id")
        .and_then(|v| v.as_str());
    let was_mentioned =
        is_mention(text, &bot_user_id, self.mention_regex.as_ref())
            || is_implicit_mention(&bot_user_id, parent_uid);

    if !was_mentioned {
        // Buffer for pending history context
        ...
        continue;
    }

    // Strip @mention from text before sending to LLM
    text.replace(&format!("<@{bot_user_id}>"), "")
        .trim()
        .to_string()
} else {
    text.to_string()
};
```

Replace with:

```rust
let (text, triage_required) = if self.mention_only {
    let explicit_mention =
        is_mention(text, &bot_user_id, self.mention_regex.as_ref());
    let thread_ts_val = msg
        .get("thread_ts")
        .and_then(|v| v.as_str());
    let is_participant = thread_ts_val
        .map(|ts| self.has_participated(ts))
        .unwrap_or(false);

    if explicit_mention {
        // Explicit @mention always responds, no triage needed
        let cleaned = text
            .replace(&format!("<@{bot_user_id}>"), "")
            .trim()
            .to_string();
        (cleaned, false)
    } else if is_participant {
        // Bot participated in thread — send through with triage flag
        (text.to_string(), true)
    } else {
        // Neither mention nor participant — buffer silently
        let buffer = pending_history
            .entry(channel_id.clone())
            .or_insert_with(|| PendingHistoryBuffer::new(50));
        let envelope = format_message_envelope(
            &channel_id, user, &ts_display, text,
        );
        buffer.push(envelope);
        continue;
    }
} else {
    (text.to_string(), false)
};
```

**Step 4: Set `triage_required` on `ChannelMessage` construction**

At line 621, update the ChannelMessage construction to use the `triage_required` variable:

```rust
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
```

**Step 5: Remove `is_implicit_mention` function**

Delete the `is_implicit_mention` function at line 143-146 and its test at line 894-896. The function is now dead code.

**Step 6: Run all Slack tests**

Run: `cargo test --lib channels::slack`
Expected: PASS — all tests including existing mention tests.

**Step 7: Commit**

```bash
git add src/channels/slack.rs
git commit -m "feat(slack): replace is_implicit_mention with participation-based triage routing"
```

---

### Task 5: Add triage check in orchestration layer

**Files:**
- Modify: `src/channels/mod.rs:201-227` (ChannelRuntimeContext — add triage_model field)
- Modify: `src/channels/mod.rs:2138-2180` (run_message_dispatch_loop — add triage check)
- Modify: `src/channels/mod.rs:3309-3343` (ChannelRuntimeContext construction — wire triage_model)

**Step 1: Write failing tests for triage response parsing**

Add a helper function and tests in `src/channels/mod.rs`:

```rust
/// Parse a triage response. Returns true (respond) if the response
/// starts with "YES" (case-insensitive). Anything else = false (skip).
fn parse_triage_response(response: &str) -> bool {
    response.trim().to_uppercase().starts_with("YES")
}

#[cfg(test)]
// In the existing test module:
#[test]
fn triage_response_yes_returns_true() {
    assert!(parse_triage_response("YES"));
    assert!(parse_triage_response("yes"));
    assert!(parse_triage_response("Yes"));
    assert!(parse_triage_response("YES, they need my help"));
}

#[test]
fn triage_response_no_returns_false() {
    assert!(!parse_triage_response("NO"));
    assert!(!parse_triage_response("no"));
    assert!(!parse_triage_response("No"));
    assert!(!parse_triage_response("NO, they're fine"));
}

#[test]
fn triage_response_empty_or_error_returns_false() {
    assert!(!parse_triage_response(""));
    assert!(!parse_triage_response("   "));
    assert!(!parse_triage_response("maybe"));
    assert!(!parse_triage_response("I think so"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib channels::tests::triage_response`
Expected: FAIL — `parse_triage_response` doesn't exist.

**Step 3: Implement `parse_triage_response`**

Add the function in `src/channels/mod.rs` near the other helper functions:

```rust
fn parse_triage_response(response: &str) -> bool {
    response.trim().to_uppercase().starts_with("YES")
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib channels::tests::triage_response`
Expected: PASS

**Step 5: Add `triage_model` to `ChannelRuntimeContext`**

Add field to the struct:

```rust
triage_model: Option<String>,
```

Wire it in the construction site (line 3309+):

```rust
triage_model: config
    .channels_config
    .slack
    .as_ref()
    .and_then(|sl| sl.triage_model.clone()),
```

**Step 6: Build the triage prompt constant**

Add a constant near the top of `src/channels/mod.rs`:

```rust
const TRIAGE_PROMPT: &str = r#"You are monitoring a Slack thread you previously participated in.
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

Respond with exactly YES or NO."#;
```

**Step 7: Add triage check in `process_channel_message`**

At the top of `process_channel_message` (line 1512), after the cancellation check, add the triage gate:

```rust
// Triage gate: if message requires triage, check with LLM first
if msg.triage_required {
    if let Some(ref triage_model) = ctx.triage_model {
        let thread_context = msg.thread_history.as_deref().unwrap_or("");
        let triage_input = format!(
            "{}\n\nThread context:\n{}\n\nNew message:\n{}",
            TRIAGE_PROMPT, thread_context, msg.content
        );

        let should_respond = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            ctx.provider.chat_with_history(
                &[crate::providers::ChatMessage {
                    role: "user".to_string(),
                    content: triage_input,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    images: None,
                }],
                triage_model,
                0.0,
            ),
        )
        .await
        {
            Ok(Ok(response)) => parse_triage_response(&response),
            Ok(Err(e)) => {
                tracing::debug!("Triage LLM error, skipping: {e}");
                false
            }
            Err(_) => {
                tracing::debug!("Triage LLM timeout, skipping");
                false
            }
        };

        if !should_respond {
            tracing::debug!(
                "Triage: skipping message from {} in thread {:?}",
                msg.sender,
                msg.thread_ts
            );
            return;
        }
    } else {
        // triage_model not configured — skip silently (backward-compatible)
        tracing::debug!(
            "Triage required but no triage_model configured, skipping message from {}",
            msg.sender
        );
        return;
    }
}
```

**Step 8: Run all channel tests**

Run: `cargo test --lib channels`
Expected: PASS

**Step 9: Run full test suite**

Run: `cargo test`
Expected: PASS

**Step 10: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat(channels): add LLM-based triage gate for thread-participant messages"
```

---

### Task 6: Final verification

**Files:** None (verification only)

**Step 1: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No new warnings.

**Step 2: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues (or only pre-existing ones).

**Step 3: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

**Step 4: Verify existing mention tests unchanged**

Run: `cargo test --lib channels::slack::tests::detects_explicit_bot_mention`
Run: `cargo test --lib channels::slack::tests::no_mention_for_regular_message`
Run: `cargo test --lib channels::slack::tests::mention_regex_matches_bot_name`
Expected: All pass — existing behavior preserved.

**Step 5: Commit (if clippy/fmt fixes needed)**

```bash
git add -A
git commit -m "chore: fix clippy/fmt issues from thread triage implementation"
```
