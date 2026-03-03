# Gemini Malformed Function Call Recovery — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Handle Gemini's `MALFORMED_FUNCTION_CALL` finish reason by detecting, recovering from, and preventing malformed tool calls.

**Architecture:** Three layers — (1) add `finishReason`/`finishMessage` to `Candidate` and parse malformed calls from text, (2) add `allowed_function_names` to `FunctionCallingConfigMode` and thread `required_tool_names` through `ChatRequest`, (3) wire ANY mode into planner executor and add retry escalation.

**Tech Stack:** Rust, serde, regex (for malformed call parsing)

---

### Task 1: Add `finishReason` and `finishMessage` to Candidate

**Files:**
- Modify: `src/providers/gemini.rs:348-352` (Candidate struct)
- Test: `src/providers/gemini.rs` (tests module, append)

**Step 1: Write the failing test**

Add at end of `mod tests` in `src/providers/gemini.rs` (before the closing `}`):

```rust
#[test]
fn candidate_deserializes_finish_reason_and_message() {
    // Present fields
    let json = serde_json::json!({
        "content": { "parts": [{ "text": "hello" }] },
        "finishReason": "MALFORMED_FUNCTION_CALL",
        "finishMessage": "Malformed function call: call:slack_send{\"ch\": \"C1\"}"
    });
    let c: Candidate = serde_json::from_value(json).unwrap();
    assert_eq!(c.finish_reason.as_deref(), Some("MALFORMED_FUNCTION_CALL"));
    assert!(c.finish_message.as_ref().unwrap().contains("slack_send"));

    // Absent fields (backward compat)
    let json = serde_json::json!({
        "content": { "parts": [{ "text": "hello" }] }
    });
    let c: Candidate = serde_json::from_value(json).unwrap();
    assert!(c.finish_reason.is_none());
    assert!(c.finish_message.is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw candidate_deserializes_finish_reason -- --nocapture`
Expected: FAIL — `Candidate` has no `finish_reason` or `finish_message` fields.

**Step 3: Write minimal implementation**

In `src/providers/gemini.rs`, replace the `Candidate` struct (lines 348-352):

```rust
#[derive(Debug, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<CandidateContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
    #[serde(default, rename = "finishMessage")]
    finish_message: Option<String>,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p zeroclaw candidate_deserializes_finish_reason -- --nocapture`
Expected: PASS

**Step 5: Run full test suite to verify no regressions**

Run: `cargo test -p zeroclaw`
Expected: All existing tests pass. The new `Option` fields with `#[serde(default)]` are backward-compatible.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): deserialize finishReason and finishMessage from Candidate"
```

---

### Task 2: Add `allowed_function_names` to `FunctionCallingConfigMode`

**Files:**
- Modify: `src/providers/gemini.rs:212-215` (FunctionCallingConfigMode struct)
- Test: `src/providers/gemini.rs` (tests module, append)

**Step 1: Write the failing test**

Add at end of `mod tests`:

```rust
#[test]
fn function_calling_config_any_mode_serializes_allowed_names() {
    let config = FunctionCallingConfigMode {
        mode: "ANY".into(),
        allowed_function_names: Some(vec!["slack_send".into(), "shell".into()]),
    };
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["mode"], "ANY");
    let names = json["allowedFunctionNames"].as_array().unwrap();
    assert_eq!(names.len(), 2);
    assert_eq!(names[0], "slack_send");
}

#[test]
fn function_calling_config_auto_mode_omits_allowed_names() {
    let config = FunctionCallingConfigMode {
        mode: "AUTO".into(),
        allowed_function_names: None,
    };
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["mode"], "AUTO");
    assert!(json.get("allowedFunctionNames").is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw function_calling_config -- --nocapture`
Expected: FAIL — `FunctionCallingConfigMode` has no `allowed_function_names` field.

**Step 3: Write minimal implementation**

In `src/providers/gemini.rs`, replace `FunctionCallingConfigMode` (lines 212-215):

```rust
#[derive(Debug, Serialize, Clone)]
struct FunctionCallingConfigMode {
    mode: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "allowedFunctionNames"
    )]
    allowed_function_names: Option<Vec<String>>,
}
```

Then fix all existing construction sites that build `FunctionCallingConfigMode` to include the new field. There are three locations:

1. `src/providers/gemini.rs:2073-2077` (`chat()` method):
```rust
let tool_config = gemini_tools.as_ref().map(|_| GeminiToolConfig {
    function_calling_config: FunctionCallingConfigMode {
        mode: "AUTO".into(),
        allowed_function_names: None,
    },
});
```

2. `src/providers/gemini.rs:2292-2300` (`chat_with_tools()` method):
```rust
let tool_config = if gemini_tools.is_some() {
    Some(GeminiToolConfig {
        function_calling_config: FunctionCallingConfigMode {
            mode: "AUTO".into(),
            allowed_function_names: None,
        },
    })
} else {
    None
};
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw function_calling_config -- --nocapture`
Expected: PASS

**Step 5: Run full suite**

Run: `cargo test -p zeroclaw`
Expected: All pass

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): add allowed_function_names to FunctionCallingConfigMode"
```

---

### Task 3: Add `required_tool_names` to `ChatRequest`

**Files:**
- Modify: `src/providers/traits.rs:110-118` (ChatRequest struct)
- Modify: `src/agent/loop_.rs:2184-2189` (ChatRequest construction in tool loop)
- Modify: `src/agent/planner.rs:311-329` (ChatRequest construction in executor)
- Test: verify compilation only (no behavior change yet)

**Step 1: Add the field to ChatRequest**

In `src/providers/traits.rs`, update `ChatRequest` (lines 112-118):

```rust
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
    /// Semantic hint for the request context (e.g., "triage", "simple", "complex").
    /// Providers may use this to adjust behavior (e.g., thinking budget).
    pub route_hint: Option<&'a str>,
    /// When set, forces providers to require a structured tool call using only
    /// these function names (e.g., Gemini `mode: "ANY"` + `allowedFunctionNames`).
    /// `None` means the provider decides (AUTO mode).
    pub required_tool_names: Option<&'a [String]>,
}
```

Note: `ChatRequest` derives `Copy` — `&'a [String]` is `Copy`, so this works.

**Step 2: Fix all construction sites**

Every place that builds a `ChatRequest` must now include `required_tool_names: None`.

In `src/agent/loop_.rs:2184-2189`:
```rust
let chat_future = provider.chat(
    ChatRequest {
        messages: &prepared_messages.messages,
        tools: request_tools,
        route_hint,
        required_tool_names: None,
    },
    model,
    temperature,
);
```

In `src/providers/traits.rs`, the `chat()` default implementation builds no `ChatRequest` internally, so no change needed there. But check all test code that constructs `ChatRequest` — search for `ChatRequest {` in the codebase and add `required_tool_names: None` to each.

**Step 3: Verify compilation**

Run: `cargo check`
Expected: Compiles cleanly. All existing behavior unchanged.

**Step 4: Run full suite**

Run: `cargo test -p zeroclaw`
Expected: All pass

**Step 5: Commit**

```bash
git add src/providers/traits.rs src/agent/loop_.rs
git commit -m "feat(providers): add required_tool_names to ChatRequest"
```

---

### Task 4: Wire `required_tool_names` into Gemini `chat()` to control ANY/AUTO mode

**Files:**
- Modify: `src/providers/gemini.rs:1868-1877` (Gemini `chat()` method, tool_config construction)
- Test: `src/providers/gemini.rs` (tests module, append)

**Step 1: Write the failing test**

This test verifies that when `required_tool_names` is set, the tool config uses `"ANY"` mode with the specified function names. Since `send_generate_content` makes HTTP calls, we test the request construction indirectly by testing the `GeminiToolConfig` construction logic.

Add a helper and test at end of `mod tests`:

```rust
#[test]
fn tool_config_uses_any_mode_when_required_tool_names_set() {
    let required = vec!["slack_send".to_string(), "shell".to_string()];
    let config = build_tool_config_for_request(true, Some(&required));
    let tc = config.unwrap();
    assert_eq!(tc.function_calling_config.mode, "ANY");
    let names = tc.function_calling_config.allowed_function_names.as_ref().unwrap();
    assert_eq!(names, &["slack_send", "shell"]);
}

#[test]
fn tool_config_uses_auto_mode_when_required_tool_names_none() {
    let config = build_tool_config_for_request(true, None);
    let tc = config.unwrap();
    assert_eq!(tc.function_calling_config.mode, "AUTO");
    assert!(tc.function_calling_config.allowed_function_names.is_none());
}

#[test]
fn tool_config_is_none_when_no_tools() {
    let config = build_tool_config_for_request(false, None);
    assert!(config.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw tool_config_uses -- --nocapture`
Expected: FAIL — `build_tool_config_for_request` function does not exist.

**Step 3: Extract helper and wire into chat()**

Add a helper function (above the `impl GeminiProvider` block or within it as a private associated function):

```rust
/// Build tool config based on whether tools are present and whether specific
/// tool names are required (ANY mode) or optional (AUTO mode).
fn build_tool_config_for_request(
    has_tools: bool,
    required_tool_names: Option<&[String]>,
) -> Option<GeminiToolConfig> {
    if !has_tools {
        return None;
    }
    Some(GeminiToolConfig {
        function_calling_config: match required_tool_names {
            Some(names) => FunctionCallingConfigMode {
                mode: "ANY".into(),
                allowed_function_names: Some(names.to_vec()),
            },
            None => FunctionCallingConfigMode {
                mode: "AUTO".into(),
                allowed_function_names: None,
            },
        },
    })
}
```

Then update the `chat()` method (around line 2073) to use it:

```rust
let tool_config = build_tool_config_for_request(
    gemini_tools.is_some(),
    request.required_tool_names,
);
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw tool_config_uses -- --nocapture`
Expected: PASS

**Step 5: Run full suite**

Run: `cargo test -p zeroclaw`
Expected: All pass

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): wire required_tool_names into ANY/AUTO mode selection"
```

---

### Task 5: Parse malformed function calls from `finishMessage`

**Files:**
- Modify: `src/providers/gemini.rs` (new function, above `impl GeminiProvider`)
- Test: `src/providers/gemini.rs` (tests module, append)

**Step 1: Write the failing tests**

Add at end of `mod tests`:

```rust
#[test]
fn parse_malformed_function_call_json_variant() {
    let msg = r#"Malformed function call: call:slack_send{"channel_id": "C0AG29ZDQUC", "message": "hello world"}"#;
    let result = parse_malformed_function_call(msg).unwrap();
    assert_eq!(result.name, "slack_send");
    assert_eq!(result.args["channel_id"], "C0AG29ZDQUC");
    assert_eq!(result.args["message"], "hello world");
}

#[test]
fn parse_malformed_function_call_python_variant() {
    let msg = "Malformed function call: call:slack_send(channel_id='C0AG29ZDQUC', message='hello world')";
    let result = parse_malformed_function_call(msg).unwrap();
    assert_eq!(result.name, "slack_send");
    assert_eq!(result.args["channel_id"], "C0AG29ZDQUC");
    assert_eq!(result.args["message"], "hello world");
}

#[test]
fn parse_malformed_function_call_python_double_quotes() {
    let msg = r#"Malformed function call: call:slack_send(channel_id="C0AG29ZDQUC", message="hello")"#;
    let result = parse_malformed_function_call(msg).unwrap();
    assert_eq!(result.name, "slack_send");
    assert_eq!(result.args["channel_id"], "C0AG29ZDQUC");
}

#[test]
fn parse_malformed_function_call_returns_none_on_garbage() {
    assert!(parse_malformed_function_call("random text").is_none());
    assert!(parse_malformed_function_call("").is_none());
    assert!(parse_malformed_function_call("Malformed function call: no_call_prefix").is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw parse_malformed_function_call -- --nocapture`
Expected: FAIL — function does not exist.

**Step 3: Write the parser**

Add to `src/providers/gemini.rs` (e.g., after `build_tool_config_for_request`):

```rust
/// Attempt to recover a tool call from a Gemini `finishMessage` string.
///
/// Gemini flash sometimes returns tool calls as text instead of structured
/// `functionCall` parts. Two formats observed:
/// - JSON-ish:   `call:tool_name{"key": "value", ...}`
/// - Python-ish: `call:tool_name(key='value', ...)`
fn parse_malformed_function_call(message: &str) -> Option<FunctionCallResponse> {
    // Strip prefix if present
    let body = message
        .strip_prefix("Malformed function call: ")
        .unwrap_or(message);

    // Extract tool name after "call:"
    let after_call = body.strip_prefix("call:")?;
    let name_end = after_call.find(|c: char| c == '{' || c == '(')?;
    let name = after_call[..name_end].trim().to_string();
    if name.is_empty() {
        return None;
    }

    let args_body = &after_call[name_end..];

    // Try JSON variant: {...}
    if args_body.starts_with('{') {
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_body) {
            return Some(FunctionCallResponse { name, args });
        }
        // JSON parse failed — try to find matching brace
        if let Some(end) = find_matching_brace(args_body) {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&args_body[..=end]) {
                return Some(FunctionCallResponse { name, args });
            }
        }
    }

    // Try Python kwargs variant: (key='value', key2="value2")
    if args_body.starts_with('(') && args_body.ends_with(')') {
        let inner = &args_body[1..args_body.len() - 1];
        if let Some(obj) = parse_python_kwargs(inner) {
            return Some(FunctionCallResponse { name, args: obj });
        }
    }

    None
}

/// Find the index of the matching closing brace for an opening `{`.
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Parse Python-style keyword arguments into a JSON object.
///
/// Input: `channel_id='C0AG29ZDQUC', message='hello world'`
/// Output: `{"channel_id": "C0AG29ZDQUC", "message": "hello world"}`
fn parse_python_kwargs(input: &str) -> Option<serde_json::Value> {
    let mut map = serde_json::Map::new();
    // Split on ", " but respect quoted strings
    let pairs = split_kwargs(input);
    for pair in pairs {
        let eq_pos = pair.find('=')?;
        let key = pair[..eq_pos].trim();
        let val_raw = pair[eq_pos + 1..].trim();
        // Strip surrounding quotes (single or double)
        let val = val_raw
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| {
                val_raw
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
            })
            .unwrap_or(val_raw);
        map.insert(key.to_string(), serde_json::Value::String(val.to_string()));
    }
    if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map))
    }
}

/// Split kwargs string on `, ` while respecting quoted values.
fn split_kwargs(input: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut in_quote = None;
    for (i, ch) in input.char_indices() {
        match ch {
            '\'' | '"' => {
                if in_quote == Some(ch) {
                    in_quote = None;
                } else if in_quote.is_none() {
                    in_quote = Some(ch);
                }
            }
            ',' if in_quote.is_none() => {
                let segment = input[start..i].trim();
                if !segment.is_empty() {
                    result.push(segment);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = input[start..].trim();
    if !last.is_empty() {
        result.push(last);
    }
    result
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw parse_malformed_function_call -- --nocapture`
Expected: PASS

**Step 5: Run full suite**

Run: `cargo test -p zeroclaw`
Expected: All pass

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): add parser for malformed function call recovery"
```

---

### Task 6: Wire malformed call detection into `send_generate_content`

**Files:**
- Modify: `src/providers/gemini.rs:1700-1737` (response processing in `send_generate_content`)
- Test: `src/providers/gemini.rs` (tests module, append)

**Step 1: Write the failing test**

Add at end of `mod tests`:

```rust
#[test]
fn malformed_call_recovered_produces_tool_call() {
    // Simulate what Gemini returns: a candidate with empty content,
    // finishReason=MALFORMED_FUNCTION_CALL, and the call in finishMessage.
    let json = serde_json::json!({
        "candidates": [{
            "content": {},
            "finishReason": "MALFORMED_FUNCTION_CALL",
            "finishMessage": "Malformed function call: call:slack_send{\"channel_id\": \"C1\", \"message\": \"hi\"}"
        }]
    });
    let response: GenerateContentResponse = serde_json::from_value(json).unwrap();
    let result = response.into_effective_response();

    // Extract the first candidate and attempt recovery
    let candidate = result.candidates.unwrap().into_iter().next().unwrap();
    assert_eq!(
        candidate.finish_reason.as_deref(),
        Some("MALFORMED_FUNCTION_CALL")
    );

    // Content should be empty/None
    let content_empty = candidate
        .content
        .as_ref()
        .map(|c| c.parts.is_empty())
        .unwrap_or(true);
    assert!(content_empty);

    // Recovery should work
    let recovered =
        parse_malformed_function_call(candidate.finish_message.as_deref().unwrap()).unwrap();
    assert_eq!(recovered.name, "slack_send");
    assert_eq!(recovered.args["channel_id"], "C1");
}
```

**Step 2: Run test to verify it passes (this is a validation test, not TDD)**

Run: `cargo test -p zeroclaw malformed_call_recovered -- --nocapture`
Expected: PASS (the parser and struct changes are already done).

**Step 3: Wire recovery into `send_generate_content`**

In `src/providers/gemini.rs`, modify the response processing section (lines ~1713-1730). Currently:

```rust
let content = result
    .candidates
    .and_then(|c| c.into_iter().next())
    .and_then(|c| c.content);

let (text, tool_calls, raw_parts) = match content {
    Some(c) => c.extract_response(),
    None => (None, Vec::new(), Vec::new()),
};

// When no tool calls and no text, report as error
if text.is_none() && tool_calls.is_empty() {
    tracing::warn!(...);
    anyhow::bail!("No response from Gemini");
}
```

Replace with:

```rust
let candidate = result.candidates.and_then(|c| c.into_iter().next());

let (text, tool_calls, raw_parts) = match &candidate {
    Some(c) if c.finish_reason.as_deref() == Some("MALFORMED_FUNCTION_CALL") => {
        // Attempt recovery from finishMessage
        if let Some(recovered) = c
            .finish_message
            .as_deref()
            .and_then(parse_malformed_function_call)
        {
            tracing::warn!(
                tool = %recovered.name,
                "Recovered malformed function call from finishMessage"
            );
            let tool_call = ToolCall {
                id: "gemini_call_0".to_string(),
                name: recovered.name.clone(),
                arguments: recovered.args.to_string(),
                thought_signature: None,
            };
            (None, vec![tool_call], Vec::new())
        } else {
            tracing::warn!(
                finish_message = c.finish_message.as_deref().unwrap_or("<empty>"),
                "MALFORMED_FUNCTION_CALL but could not parse finishMessage"
            );
            anyhow::bail!("Gemini returned MALFORMED_FUNCTION_CALL and recovery failed");
        }
    }
    Some(c) => match &c.content {
        Some(content) => {
            // Need to take ownership — destructure the candidate
            // We checked the ref above, now consume it
            let c = candidate.unwrap();
            c.content.unwrap().extract_response()
        }
        None => (None, Vec::new(), Vec::new()),
    },
    None => (None, Vec::new(), Vec::new()),
};

// When no tool calls and no text, report as error (mirrors old behavior)
if text.is_none() && tool_calls.is_empty() {
    tracing::warn!(
        body = &body_text[..body_text.len().min(1000)],
        "Gemini returned no extractable text or tool calls"
    );
    anyhow::bail!("No response from Gemini");
}
```

Note: The borrow checker may require restructuring. The key logic is:
1. Check `finish_reason` on the candidate **before** consuming `content`
2. If `MALFORMED_FUNCTION_CALL`: attempt parse from `finish_message`, construct tool call on success, bail on failure
3. Otherwise: proceed with existing `extract_response()` flow

A cleaner approach to avoid the borrow-after-move issue:

```rust
let candidate = result.candidates.and_then(|c| c.into_iter().next());

let (text, tool_calls, raw_parts) = if let Some(candidate) = candidate {
    if candidate.finish_reason.as_deref() == Some("MALFORMED_FUNCTION_CALL") {
        // Attempt recovery from finishMessage
        match candidate
            .finish_message
            .as_deref()
            .and_then(parse_malformed_function_call)
        {
            Some(recovered) => {
                tracing::warn!(
                    tool = %recovered.name,
                    "Recovered malformed function call from finishMessage"
                );
                let tool_call = ToolCall {
                    id: "gemini_call_0".to_string(),
                    name: recovered.name.clone(),
                    arguments: recovered.args.to_string(),
                    thought_signature: None,
                };
                (None, vec![tool_call], Vec::new())
            }
            None => {
                tracing::warn!(
                    finish_message = candidate.finish_message.as_deref().unwrap_or("<empty>"),
                    "MALFORMED_FUNCTION_CALL but could not parse finishMessage"
                );
                anyhow::bail!("Gemini returned MALFORMED_FUNCTION_CALL and recovery failed");
            }
        }
    } else {
        match candidate.content {
            Some(c) => c.extract_response(),
            None => (None, Vec::new(), Vec::new()),
        }
    }
} else {
    (None, Vec::new(), Vec::new())
};
```

**Step 4: Run full suite**

Run: `cargo test -p zeroclaw`
Expected: All pass. Existing tests provide candidates without `finishReason`, so they hit the `else` branch unchanged.

**Step 5: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): recover tool calls from MALFORMED_FUNCTION_CALL finishMessage"
```

---

### Task 7: Thread `required_tool_names` through `run_tool_call_loop` into planner executor

**Files:**
- Modify: `src/agent/loop_.rs:2083-2101` (add parameter to `run_tool_call_loop`)
- Modify: `src/agent/loop_.rs:2184-2189` (pass through to `ChatRequest`)
- Modify: `src/agent/planner.rs:311-329` (pass `wanted_tools` as `required_tool_names`)
- Modify: all callers of `run_tool_call_loop` (add `None` for new parameter)

**Step 1: Add parameter to `run_tool_call_loop`**

In `src/agent/loop_.rs`, update the function signature (line 2083):

```rust
pub(crate) async fn run_tool_call_loop(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    route_hint: Option<&str>,
    required_tool_names: Option<&[String]>,  // NEW
) -> Result<String> {
```

**Step 2: Pass through to ChatRequest**

In the same file (around line 2184), update the `ChatRequest` construction:

```rust
let chat_future = provider.chat(
    ChatRequest {
        messages: &prepared_messages.messages,
        tools: request_tools,
        route_hint,
        required_tool_names,
    },
    model,
    temperature,
);
```

**Step 3: Fix all callers**

Search for all calls to `run_tool_call_loop` in the codebase. For each existing call site, append `None` as the last argument (no forced tool names by default).

Key call sites to update:
- `src/agent/agent.rs` — main agent turn
- `src/agent/planner.rs:311` — executor actions
- `src/agent/loop_.rs` — test helpers

For `src/agent/planner.rs:311`, pass the executor's `wanted_tools`:

```rust
let result = crate::agent::loop_::run_tool_call_loop(
    provider,
    &mut action_messages,
    tools_registry,
    observer,
    provider_name,
    executor_model,
    temperature,
    true,
    None,
    channel_name,
    &crate::config::MultimodalConfig::default(),
    max_tool_iterations.min(max_executor_iterations),
    ct,
    None,
    hooks,
    &combined_excluded,
    None,
    if wanted_tools.is_empty() { None } else { Some(&wanted_tools) },  // NEW
)
.await;
```

Note: `wanted_tools` is already computed at line 280 as `filter_tool_names(&all_tool_names, &action.tools)`. Capture it into the async block alongside `combined_excluded`.

For all other callers, pass `None`.

**Step 4: Verify compilation**

Run: `cargo check`
Expected: Compiles cleanly.

**Step 5: Run full suite**

Run: `cargo test -p zeroclaw`
Expected: All pass. No behavior change for `None` callers. Executor actions with non-empty tool lists now signal `required_tool_names` through to the Gemini provider.

**Step 6: Commit**

```bash
git add src/agent/loop_.rs src/agent/planner.rs src/agent/agent.rs
git commit -m "feat(agent): thread required_tool_names from planner executor into provider"
```

---

### Task 8: Run full validation

**Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues.

**Step 2: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings.

**Step 3: Full test suite**

Run: `cargo test`
Expected: All pass.

**Step 4: Final commit (if any fixups needed)**

Only if formatting or clippy required changes:

```bash
git add -u
git commit -m "style(gemini): fix formatting from malformed call recovery"
```
