# Gemini Thought Signature Round-Trip — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Preserve all Gemini response parts (including thinking parts with thoughtSignature) through the history round-trip so thinking models don't reject conversations after tool-call exchanges.

**Architecture:** Store the raw response parts as `Vec<serde_json::Value>` alongside extracted text/tool_calls. On history replay, deserialize and use these parts directly instead of reconstructing from tool_calls. Falls through to plain-text fallback when raw parts are absent.

**Tech Stack:** Rust, serde_json, existing provider trait system.

---

### Task 1: Add Deserialize to Part and FunctionCallPart

**Files:**
- Modify: `src/providers/gemini.rs:264-284`

**Step 1: Write the failing test**

Add a round-trip test that serializes a `Part` to JSON and deserializes it back. This will fail because `Part` doesn't derive `Deserialize`.

```rust
#[test]
fn part_round_trips_through_json() {
    let original = Part {
        text: Some("reasoning".into()),
        thought: Some(true),
        thought_signature: Some("sig123".into()),
        function_call: None,
        function_response: None,
    };
    let json = serde_json::to_value(&original).unwrap();
    let restored: Part = serde_json::from_value(json).unwrap();
    assert_eq!(restored.text.as_deref(), Some("reasoning"));
    assert_eq!(restored.thought, Some(true));
    assert_eq!(restored.thought_signature.as_deref(), Some("sig123"));
}
```

Append this test to the `#[cfg(test)] mod tests` block at the bottom of `gemini.rs`.

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw part_round_trips_through_json 2>&1 | tail -20`
Expected: Compile error — `Part` does not implement `Deserialize`.

**Step 3: Add Deserialize derives and serde(default) attributes**

Change `Part` (line 264):
```rust
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Thinking models: marks this part as internal reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
    /// Opaque signature for thinking context — must be replayed exactly as received.
    #[serde(default, rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
    #[serde(default, rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<FunctionCallPart>,
    #[serde(default, rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<FunctionResponsePart>,
}
```

Change `FunctionCallPart` (line 280):
```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
struct FunctionCallPart {
```

Change `FunctionResponsePart` (line 286):
```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
struct FunctionResponsePart {
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw part_round_trips_through_json 2>&1 | tail -10`
Expected: PASS

**Step 5: Commit**

```
feat(gemini): add Deserialize to Part structs for round-trip support
```

---

### Task 2: Expand extract_response() to return raw parts

**Files:**
- Modify: `src/providers/gemini.rs:390-423` (extract_response)
- Modify: `src/providers/gemini.rs:426-430` (GeminiResponse)
- Modify: `src/providers/gemini.rs:1625-1648` (send_generate_content caller)

**Step 1: Write the failing test**

Add a test that calls `extract_response()` on a `CandidateContent` with thinking parts and verifies the third return element contains all parts.

```rust
#[test]
fn extract_response_returns_raw_parts_with_thinking() {
    let content = CandidateContent {
        parts: vec![
            ResponsePart {
                text: Some("reasoning...".into()),
                thought: true,
                thought_signature: Some("sig1".into()),
                function_call: None,
            },
            ResponsePart {
                text: None,
                thought: false,
                thought_signature: Some("sig2".into()),
                function_call: Some(FunctionCallResponse {
                    name: "search".into(),
                    args: serde_json::json!({"q": "test"}),
                }),
            },
        ],
    };
    let (text, calls, raw_parts) = content.extract_response();

    // text should be the thinking text (fallback since no non-thinking text)
    assert_eq!(text.as_deref(), Some("reasoning..."));
    // one tool call
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "search");
    // raw_parts preserves ALL parts
    assert_eq!(raw_parts.len(), 2);
    assert_eq!(raw_parts[0].thought, Some(true));
    assert_eq!(raw_parts[0].thought_signature.as_deref(), Some("sig1"));
    assert_eq!(raw_parts[1].function_call.as_ref().unwrap().name, "search");
    assert_eq!(raw_parts[1].thought_signature.as_deref(), Some("sig2"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw extract_response_returns_raw_parts_with_thinking 2>&1 | tail -20`
Expected: Compile error — `extract_response()` returns a 2-tuple, not 3-tuple.

**Step 3: Implement the changes**

Change `extract_response()` signature and body (line 390) to return `(Option<String>, Vec<ToolCall>, Vec<Part>)`. Build `all_parts` by converting each `ResponsePart` to a `Part` during the existing iteration:

```rust
fn extract_response(self) -> (Option<String>, Vec<ToolCall>, Vec<Part>) {
    let mut answer_parts: Vec<String> = Vec::new();
    let mut first_thinking: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut all_parts: Vec<Part> = Vec::new();

    for part in self.parts {
        // Convert ResponsePart → outbound Part (preserves all fields)
        all_parts.push(Part {
            text: part.text.clone(),
            thought: if part.thought { Some(true) } else { None },
            thought_signature: part.thought_signature.clone(),
            function_call: part.function_call.as_ref().map(|fc| FunctionCallPart {
                name: fc.name.clone(),
                args: fc.args.clone(),
            }),
            function_response: None,
        });

        if let Some(fc) = part.function_call {
            tool_calls.push(ToolCall {
                id: format!("gemini_call_{}", tool_calls.len()),
                name: fc.name,
                arguments: fc.args.to_string(),
                thought_signature: part.thought_signature.clone(),
            });
        }
        if let Some(text) = part.text {
            if text.is_empty() {
                continue;
            }
            if !part.thought {
                answer_parts.push(text);
            } else if first_thinking.is_none() {
                first_thinking = Some(text);
            }
        }
    }

    let text = if answer_parts.is_empty() {
        first_thinking
    } else {
        Some(answer_parts.join(""))
    };

    (text, tool_calls, all_parts)
}
```

Update `GeminiResponse` (line 426):
```rust
struct GeminiResponse {
    text: Option<String>,
    tool_calls: Vec<ToolCall>,
    raw_parts: Vec<Part>,
    usage: Option<TokenUsage>,
}
```

Update `send_generate_content` caller (line 1630):
```rust
let (text, tool_calls, raw_parts) = match content {
    Some(c) => c.extract_response(),
    None => (None, Vec::new(), Vec::new()),
};
```

Update `GeminiResponse` construction (line 1644):
```rust
Ok(GeminiResponse {
    text,
    tool_calls,
    raw_parts,
    usage,
})
```

Fix existing tests that destructure `extract_response()` — they return 2-tuples. Update:
- `candidate_content_extracts_function_calls` (line 3389): `let (text, calls) =` → `let (text, calls, _) =`
- `candidate_content_extracts_mixed_text_and_calls` (line 3416): same change

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw extract_response_returns_raw_parts 2>&1 | tail -10`
Expected: PASS

Run: `cargo test -p zeroclaw candidate_content_extracts 2>&1 | tail -10`
Expected: PASS (existing tests still pass)

**Step 5: Commit**

```
feat(gemini): expand extract_response to return raw parts
```

---

### Task 3: Add provider_parts to ChatResponse

**Files:**
- Modify: `src/providers/traits.rs:74-88` (ChatResponse struct)
- Modify: `src/providers/gemini.rs:1945-1950` (chat() response)
- Modify: `src/providers/gemini.rs:2125-2130` (chat_with_tools() response)

**Step 1: Write the failing test**

This is a structural change. The test is that the project compiles with the new field. Add a test in `traits.rs` tests (or use an existing test) that constructs a `ChatResponse` with `provider_parts: Some(...)`.

Actually, the compile will break immediately because all existing `ChatResponse { ... }` constructions lack the new field. So the "test" is: compile the project.

**Step 2: Add provider_parts field to ChatResponse**

In `src/providers/traits.rs` line 76, add after `reasoning_content`:
```rust
/// Opaque provider-specific parts for faithful history replay.
/// Gemini uses this to preserve raw response parts (including thinking
/// signatures). Other providers leave this as None.
pub provider_parts: Option<Vec<serde_json::Value>>,
```

**Step 3: Add `provider_parts: None` to every ChatResponse construction site**

There are ~113 construction sites across the codebase. All non-Gemini ones get `provider_parts: None`.

For the two Gemini construction sites, serialize `raw_parts` to JSON values:

`chat()` (line 1945):
```rust
Ok(ChatResponse {
    text: resp.text,
    tool_calls: resp.tool_calls,
    usage: resp.usage,
    reasoning_content: None,
    provider_parts: if resp.raw_parts.is_empty() {
        None
    } else {
        Some(resp.raw_parts.iter().filter_map(|p| serde_json::to_value(p).ok()).collect())
    },
})
```

`chat_with_tools()` (line 2125):
```rust
Ok(ChatResponse {
    text: resp.text,
    tool_calls: resp.tool_calls,
    usage: resp.usage,
    reasoning_content: None,
    provider_parts: if resp.raw_parts.is_empty() {
        None
    } else {
        Some(resp.raw_parts.iter().filter_map(|p| serde_json::to_value(p).ok()).collect())
    },
})
```

All other ChatResponse constructions across the codebase get `provider_parts: None`. These are in:
- `src/providers/traits.rs` (lines 377, 389, 424, and test helpers around 549-577)
- `src/providers/ollama.rs` (lines 647, 665, 712)
- `src/providers/reliable.rs` (lines 1768, 1953)
- `src/agent/agent.rs` (multiple test mocks)
- `src/agent/dispatcher.rs` (lines 247, 264)
- `src/agent/loop_.rs` (lines 3496, 3514)
- `src/agent/planner.rs` (test mocks)
- `src/agent/tests.rs` (test mocks and helpers)
- `src/tools/delegate.rs` (lines 600, 607, 637)
- `src/tools/file_read.rs` (lines 742, 798, 805, 887, 893)
- `tests/provider_schema.rs` (lines 141, 154, 168, 180)
- `tests/agent_e2e.rs` (lines 64, 190, 238-248, 356)
- `tests/agent_loop_robustness.rs` (lines 59, 180-190, 335, 350)
- `benches/agent_benchmarks.rs` (lines 39, 51, 57, 88, 148, 162, 196)

**Step 4: Verify compilation and tests**

Run: `cargo test -p zeroclaw 2>&1 | tail -20`
Expected: All tests pass.

**Step 5: Commit**

```
feat(providers): add provider_parts to ChatResponse for faithful history replay
```

---

### Task 4: Serialize provider_parts in build_native_assistant_history

**Files:**
- Modify: `src/agent/loop_.rs:1744-1786` (build_native_assistant_history)
- Modify: `src/agent/loop_.rs:2269-2286` (serialization decision)

**Step 1: Write the failing test**

Add a test that calls `build_native_assistant_history` with `provider_parts` and verifies `raw_model_parts` appears in the output JSON.

```rust
#[test]
fn build_native_assistant_history_includes_raw_model_parts() {
    let calls = vec![ToolCall::new("gemini_call_0", "search", r#"{"q":"test"}"#)];
    let provider_parts = vec![
        serde_json::json!({"thought": true, "text": "reasoning", "thoughtSignature": "sig1"}),
        serde_json::json!({"functionCall": {"name": "search", "args": {"q": "test"}}, "thoughtSignature": "sig2"}),
    ];
    let result = build_native_assistant_history("", &calls, None, Some(&provider_parts));
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();

    let raw = parsed.get("raw_model_parts").expect("raw_model_parts must be present");
    let parts = raw.as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["thoughtSignature"].as_str(), Some("sig1"));
    assert_eq!(parts[1]["thoughtSignature"].as_str(), Some("sig2"));
}

#[test]
fn build_native_assistant_history_omits_raw_model_parts_when_none() {
    let calls = vec![ToolCall::new("call_0", "shell", "{}")];
    let result = build_native_assistant_history("text", &calls, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert!(parsed.get("raw_model_parts").is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw build_native_assistant_history_includes_raw 2>&1 | tail -20`
Expected: Compile error — wrong number of arguments.

**Step 3: Add provider_parts parameter and serialization**

Update `build_native_assistant_history` signature (line 1744):
```rust
fn build_native_assistant_history(
    text: &str,
    tool_calls: &[ToolCall],
    reasoning_content: Option<&str>,
    provider_parts: Option<&[serde_json::Value]>,
) -> String {
```

After the `reasoning_content` insertion (after line 1783), add:
```rust
    if let Some(parts) = provider_parts {
        obj.as_object_mut().unwrap().insert(
            "raw_model_parts".to_string(),
            serde_json::Value::Array(parts.to_vec()),
        );
    }
```

Update the call site at the serialization decision (line 2281):
```rust
build_native_assistant_history(
    &response_text,
    &resp.tool_calls,
    reasoning_content.as_deref(),
    resp.provider_parts.as_deref(),
)
```

Update all other call sites of `build_native_assistant_history` to pass `None` as the fourth argument. Search the codebase for all invocations — they are in the existing tests (lines 5611, 5626, 5658) and possibly elsewhere in loop_.rs.

**Step 4: Add text-only provider_parts envelope path**

Update the serialization decision (around line 2269):
```rust
let assistant_history_content = if !resp.tool_calls.is_empty() {
    build_native_assistant_history(
        &response_text,
        &resp.tool_calls,
        reasoning_content.as_deref(),
        resp.provider_parts.as_deref(),
    )
} else if resp.provider_parts.is_some() {
    // Text-only but has provider parts (e.g. Gemini thinking signatures)
    build_native_assistant_history(
        &response_text,
        &[],
        reasoning_content.as_deref(),
        resp.provider_parts.as_deref(),
    )
} else if use_native_tools {
    build_native_assistant_history_from_parsed_calls(
        &response_text,
        &calls,
        reasoning_content.as_deref(),
    )
    .unwrap_or_else(|| response_text.clone())
} else {
    response_text.clone()
};
```

**Step 5: Run tests to verify they pass**

Run: `cargo test -p zeroclaw build_native_assistant_history 2>&1 | tail -20`
Expected: All PASS (new and existing)

**Step 6: Commit**

```
feat(agent): serialize provider_parts as raw_model_parts in history
```

---

### Task 5: Replace reconstruction with raw_model_parts replay

**Files:**
- Modify: `src/providers/gemini.rs:1786-1833` (chat() history replay)
- Modify: `src/providers/gemini.rs:1977-2024` (chat_with_tools() history replay)

**Step 1: Write the failing test**

Add a unit test that constructs a serialized assistant history JSON with `raw_model_parts`, feeds it through the chat history builder, and verifies the resulting `Content` parts match the raw parts.

This is harder to test directly since `chat()` is async and hits the API. Instead, write a focused test that simulates the parsing logic. Extract the replay parsing into a helper if needed, or test at the integration level.

For now, write a test that verifies the JSON round-trip: `Part` → serialize → store as `raw_model_parts` → deserialize back → identical `Part`.

```rust
#[test]
fn raw_model_parts_round_trip_preserves_thinking_signatures() {
    // Simulate what extract_response produces
    let original_parts = vec![
        Part {
            text: Some("reasoning...".into()),
            thought: Some(true),
            thought_signature: Some("sig_thinking".into()),
            ..Default::default()
        },
        Part {
            function_call: Some(FunctionCallPart {
                name: "search".into(),
                args: serde_json::json!({"q": "test"}),
            }),
            thought_signature: Some("sig_call".into()),
            ..Default::default()
        },
    ];

    // Serialize to JSON values (as stored in provider_parts)
    let json_values: Vec<serde_json::Value> = original_parts
        .iter()
        .map(|p| serde_json::to_value(p).unwrap())
        .collect();

    // Simulate storing in history JSON
    let history_json = serde_json::json!({
        "content": null,
        "tool_calls": [],
        "raw_model_parts": json_values,
    });

    // Simulate replay: extract raw_model_parts and deserialize
    let raw = history_json.get("raw_model_parts").unwrap().as_array().unwrap();
    let restored: Vec<Part> = raw
        .iter()
        .filter_map(|p| serde_json::from_value(p.clone()).ok())
        .collect();

    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].thought, Some(true));
    assert_eq!(restored[0].thought_signature.as_deref(), Some("sig_thinking"));
    assert_eq!(restored[0].text.as_deref(), Some("reasoning..."));
    assert_eq!(restored[1].function_call.as_ref().unwrap().name, "search");
    assert_eq!(restored[1].thought_signature.as_deref(), Some("sig_call"));
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test -p zeroclaw raw_model_parts_round_trip 2>&1 | tail -10`
Expected: PASS (this test validates the serialization contract; the actual replay change is next).

**Step 3: Replace reconstruction in chat() history replay**

In `chat()`, replace the assistant history reconstruction block (lines ~1788-1833). The new logic:

```rust
"assistant" => {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
        // Use raw_model_parts for faithful replay when available
        if let Some(raw_parts) = parsed.get("raw_model_parts").and_then(|v| v.as_array()) {
            let parts: Vec<Part> = raw_parts
                .iter()
                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                .collect();
            if !parts.is_empty() {
                // Populate tool_id_to_name from tool_calls for result matching
                if let Some(tcs) = parsed.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        if let (Some(id), Some(name)) = (
                            tc.get("id").and_then(|v| v.as_str()),
                            tc.get("name").and_then(|v| v.as_str()),
                        ) {
                            tool_id_to_name.insert(id.to_string(), name.to_string());
                        }
                    }
                }
                contents.push(Content {
                    role: Some("model".into()),
                    parts,
                });
                continue;
            }
        }

        // Fallback: reconstruct from tool_calls (non-thinking models, legacy)
        if let Some(tool_calls) = parsed.get("tool_calls").and_then(|tc| tc.as_array()) {
            // ... keep existing reconstruction code as fallback ...
```

Keep the existing reconstruction-from-tool_calls block as a fallback for non-thinking Gemini models that don't produce `raw_model_parts`. This makes the change safe for mixed-model conversations.

**Step 4: Apply the same change to chat_with_tools()**

Replace the identical reconstruction block in `chat_with_tools()` (lines ~1979-2024) with the same `raw_model_parts`-first logic.

**Step 5: Run full test suite**

Run: `cargo test -p zeroclaw 2>&1 | tail -20`
Expected: All PASS

**Step 6: Commit**

```
fix(gemini): use raw_model_parts for faithful thinking signature replay

Gemini 3 thinking models require thoughtSignature on every Part to be
round-tripped exactly. The previous reconstruction from tool_calls
dropped signatures on thinking text parts, causing zero-candidate
rejections after the first tool-call exchange.
```

---

### Task 6: Run full validation

**Files:** None (validation only)

**Step 1: Run cargo fmt**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -30`
Expected: No warnings.

**Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -30`
Expected: All tests pass.

**Step 4: If any issues, fix and re-run**

---

### Summary of All Changes

| File | Change |
|------|--------|
| `src/providers/gemini.rs` | Add `Deserialize` to `Part`, `FunctionCallPart`, `FunctionResponsePart`. Expand `extract_response()` to return raw parts. Add `raw_parts` to `GeminiResponse`. Serialize to `provider_parts` on `ChatResponse`. Replace history reconstruction with `raw_model_parts` replay in both `chat()` and `chat_with_tools()`. |
| `src/providers/traits.rs` | Add `provider_parts: Option<Vec<serde_json::Value>>` to `ChatResponse`. |
| `src/agent/loop_.rs` | Add `provider_parts` param to `build_native_assistant_history()`. Serialize as `raw_model_parts`. Add text-only provider_parts envelope path in serialization decision. |
| All other files with `ChatResponse` | Add `provider_parts: None` to every construction. |
