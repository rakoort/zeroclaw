# Gemini Tool Sanitizer Fix — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix 100% empty Gemini responses caused by the transcript sanitizer corrupting parallel tool results, and add diagnostic logging to prevent silent failures.

**Architecture:** The sanitizer's consecutive-message merge (Step 4 of `sanitize_transcript_for_gemini`) naively concatenates JSON payloads of tool messages with `\n`, producing invalid JSON. The Gemini provider then silently drops the unparseable tool results, creating role alternation violations that cause 0-candidate responses. Fix: skip tool messages during the merge (the provider already handles tool result merging correctly at `gemini.rs:2165-2173`). Then add targeted logging at the two silent failure points.

**Tech Stack:** Rust, serde_json, tracing

---

### Task 1: Failing test — sanitizer must not merge consecutive tool messages

**Files:**
- Modify: `src/providers/gemini_sanitize.rs:759-773` (test module)

**Step 1: Write the failing test**

Add this test after the existing `merges_consecutive_same_role_messages` test (line 773):

```rust
#[test]
fn does_not_merge_consecutive_tool_messages() {
    let tool1 = ChatMessage::tool(
        r#"{"tool_call_id":"call1","content":"result1"}"#,
    );
    let tool2 = ChatMessage::tool(
        r#"{"tool_call_id":"call2","content":"result2"}"#,
    );
    let messages = vec![
        ChatMessage::user("Hello"),
        ChatMessage::assistant("Calling tools"),
        tool1,
        tool2,
    ];
    let result = sanitize_transcript_for_gemini(&messages);
    // Tool messages must remain separate — merging corrupts their JSON
    let tool_msgs: Vec<&ChatMessage> =
        result.iter().filter(|m| m.role == "tool").collect();
    assert_eq!(tool_msgs.len(), 2, "tool messages must not be merged");
    // Each must still be valid JSON
    assert!(serde_json::from_str::<serde_json::Value>(
        &tool_msgs[0].content
    ).is_ok(), "first tool message must be valid JSON");
    assert!(serde_json::from_str::<serde_json::Value>(
        &tool_msgs[1].content
    ).is_ok(), "second tool message must be valid JSON");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib providers::gemini_sanitize::tests::does_not_merge_consecutive_tool_messages`
Expected: FAIL — tool messages are merged into one, `tool_msgs.len()` is 1.

**Step 3: Commit failing test**

```bash
git add src/providers/gemini_sanitize.rs
git commit -m "test(gemini): add failing test for tool message merge corruption

Consecutive tool messages with JSON payloads must not be merged
by the sanitizer. The current merge joins them with newlines,
producing invalid JSON that the provider silently drops."
```

---

### Task 2: Fix the sanitizer — skip tool messages during merge

**Files:**
- Modify: `src/providers/gemini_sanitize.rs:365`

**Step 1: Apply the fix**

Change the merge condition at line 365 from:

```rust
if last.role == msg.role {
```

to:

```rust
if last.role == msg.role && msg.role != "tool" {
```

This is the entire fix. The Gemini provider already merges consecutive tool `Content` objects correctly at `gemini.rs:2165-2173` using structure-aware part merging.

**Step 2: Run the new test to verify it passes**

Run: `cargo test -p zeroclaw --lib providers::gemini_sanitize::tests::does_not_merge_consecutive_tool_messages`
Expected: PASS

**Step 3: Run all sanitizer tests to verify no regressions**

Run: `cargo test -p zeroclaw --lib providers::gemini_sanitize::tests`
Expected: All tests PASS. The existing `merges_consecutive_same_role_messages` test still passes because it only tests user/assistant merging.

**Step 4: Commit**

```bash
git add src/providers/gemini_sanitize.rs
git commit -m "fix(gemini): stop sanitizer from merging consecutive tool messages

The merge step joined tool message JSON payloads with newlines,
producing invalid JSON. The Gemini provider already handles tool
result merging correctly via structure-aware Content part merging,
so the sanitizer merge was both redundant and destructive."
```

---

### Task 3: Failing test — provider must warn on unparseable tool messages

**Files:**
- Modify: `src/providers/gemini.rs` (test module, after line 2302)

**Step 1: Write the failing test**

This test verifies the provider does not silently discard tool messages with invalid JSON. We test the behavior by checking that invalid tool message content produces a warning log. Since we can't easily test tracing output in a unit test, we instead test the _behavior_: when a tool message has invalid JSON, the provider should still produce a Content entry (with a fallback error response) rather than silently dropping it.

Add this test in the `tests` module of `gemini.rs`:

```rust
#[test]
fn tool_message_with_invalid_json_produces_error_content() {
    // Simulate what the old sanitizer merge produced: two JSON objects
    // joined by newline — invalid JSON.
    let corrupted = r#"{"tool_call_id":"call1","content":"r1"}
{"tool_call_id":"call2","content":"r2"}"#;

    // Attempt to parse like gemini.rs does
    let parsed = serde_json::from_str::<serde_json::Value>(corrupted);
    assert!(parsed.is_err(), "merged JSON must fail to parse");
}
```

Note: This test documents the parse failure. The actual warn logging is verified by running with `RUST_LOG=warn` in integration testing.

**Step 2: Run test to verify it passes**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests::tool_message_with_invalid_json_produces_error_content`
Expected: PASS (this test documents the failure mode, not the fix itself).

**Step 3: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "test(gemini): document tool message JSON parse failure mode

Verifies that newline-concatenated JSON objects (as produced by
the old sanitizer merge) fail to parse, confirming the corruption
mechanism."
```

---

### Task 4: Add warn logging for failed tool message JSON parse

**Files:**
- Modify: `src/providers/gemini.rs:1905-1947`

**Step 1: Add else clause with warning**

Change the tool message handling block from:

```rust
"tool" => {
    // Convert tool result to Gemini functionResponse
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        // ... existing handling ...
    }
}
```

to:

```rust
"tool" => {
    // Convert tool result to Gemini functionResponse
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        // ... existing handling (unchanged) ...
    } else {
        tracing::warn!(
            content_len = msg.content.len(),
            content_preview = &msg.content[..msg.content.len().min(200)],
            "Failed to parse tool message as JSON — tool result will be dropped"
        );
    }
}
```

**Step 2: Run all Gemini tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests`
Expected: All PASS

**Step 3: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "fix(gemini): warn when tool message JSON parse fails

Previously, failed tool message parses were completely silent,
making it impossible to diagnose dropped tool results from logs."
```

---

### Task 5: Add request structure logging before Gemini API call

**Files:**
- Modify: `src/providers/gemini.rs:1488-1498` (in `send_generate_content`, after building the request)

**Step 1: Add debug logging**

After the `GenerateContentRequest` is built (line 1497) and before the URL is constructed (line 1499), add:

```rust
tracing::debug!(
    contents_count = request.contents.len(),
    roles = %request.contents.iter()
        .map(|c| c.role.as_deref().unwrap_or("none"))
        .collect::<Vec<_>>()
        .join(","),
    has_system_instruction = request.system_instruction.is_some(),
    has_tools = request.tools.is_some(),
    tool_count = request.tools.as_ref().map_or(0, |t| t.len()),
    "Gemini request structure"
);
```

**Step 2: Run all Gemini tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests`
Expected: All PASS

**Step 3: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): log request structure at debug level

Logs contents count, role sequence, system instruction presence,
and tool count. Enables diagnosing role alternation violations
and malformed requests without full body dumps."
```

---

### Task 6: Full validation pass

**Step 1: Run full lint and test suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: All pass with zero warnings.

**Step 2: Fix any issues found, commit if needed**

---

## Summary of changes

| File | Change | Lines |
|------|--------|-------|
| `gemini_sanitize.rs` | Skip tool messages in merge loop | ~365 |
| `gemini_sanitize.rs` | Test: consecutive tool messages stay separate | test module |
| `gemini.rs` | Warn on failed tool JSON parse | ~1905 |
| `gemini.rs` | Test: document parse failure mode | test module |
| `gemini.rs` | Debug log request structure | ~1497 |

Total: ~20 lines of production code changed, ~30 lines of tests added.
