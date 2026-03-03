# Gemini Malformed Function Call Recovery

**Date:** 2026-03-03
**Status:** Approved
**Scope:** `src/providers/gemini.rs`, `src/providers/traits.rs`, `src/agent/loop_.rs`, `src/agent/planner.rs`

## Problem

Gemini flash occasionally returns tool calls as text instead of structured `functionCall` response parts. The API signals this via `finishReason: "MALFORMED_FUNCTION_CALL"` with empty `content: {}` and the intended call in `finishMessage`.

**Observed formats in `finishMessage`:**
- JSON-ish: `call:tool_name{"key": "value", ...}`
- Python-ish: `call:tool_name(key='value', ...)`

**Current impact:**
1. Empty content → "No response" → classified as retryable → all 4 retries fail identically (~15s wasted)
2. Action fails entirely despite the model producing correct tool parameters
3. Primarily affects executor actions with short prompts (~200 tokens)

## Design

Three layers: prevent, detect, recover.

### Layer 1: Prevent — ANY Mode for Executor Actions

**Struct change:** Add `allowed_function_names` to `FunctionCallingConfigMode`:

```rust
#[derive(Debug, Serialize)]
struct FunctionCallingConfigMode {
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowedFunctionNames")]
    allowed_function_names: Option<Vec<String>>,
}
```

**Signal threading:** Add `required_tool_names: Option<Vec<String>>` to `ChatRequest`.

**Behavior by context:**
- **Planner executor:** `PlanAction.tools` → `required_tool_names: Some(tools)` → `mode: "ANY"` + `allowedFunctionNames`
- **General agent loop:** `required_tool_names: None` → `mode: "AUTO"` (unchanged)
- **MALFORMED retry:** escalates to `mode: "ANY"` (see Layer 3)

Other providers ignore `required_tool_names` (they have their own `tool_choice` mechanisms).

### Layer 2: Detect — finishReason Deserialization

**Struct change:** Add fields to `Candidate`:

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

**Detection point:** After deserializing the candidate, check `finish_reason == "MALFORMED_FUNCTION_CALL"`.

### Layer 3: Recover — Parse and Escalate

**Recovery function:** `parse_malformed_function_call(finish_message: &str) -> Option<FunctionCallResponse>`

Parsing steps:
1. Strip `"Malformed function call: "` prefix
2. Extract tool name via `call:(\w+)`
3. Extract argument body between `{...}` or `(...)`
4. `{...}` → parse as JSON
5. `(...)` → convert Python kwargs (`key='value'`) to JSON object
6. Return `Some(FunctionCallResponse)` on success, `None` on failure

**Flow when MALFORMED_FUNCTION_CALL detected:**

```
Parse finishMessage
  ├── Success → construct GeminiResponse with recovered ToolCall, return normally
  └── Failure → return MalformedCallError (non-retryable)
        └── chat_with_tools() catches, retries once with mode: "ANY"
              ├── Success → return response
              └── Failure → propagate error
```

**Constraints:**
- At most one escalation retry per call
- If already in ANY mode and still MALFORMED, fail immediately
- Existing retry loop (OAuth/rate-limit) does NOT retry MALFORMED_FUNCTION_CALL

**Internal error type:**

```rust
enum GeminiCallError {
    Http(reqwest::Error),
    Api(ApiError),
    MalformedFunctionCall { finish_message: Option<String> },
}
```

Stays internal to Gemini provider. The `Provider` trait continues to return `anyhow::Result<ChatResponse>`.

## Testing

1. `candidate_deserializes_finish_reason_and_message` — field deserialization including absent/null
2. `parse_malformed_function_call_json_variant` — `call:name{json}` → FunctionCallResponse
3. `parse_malformed_function_call_python_variant` — `call:name(kwargs)` → FunctionCallResponse
4. `parse_malformed_function_call_returns_none_on_garbage` — unrecognizable → None
5. `malformed_call_recovered_produces_tool_call` — full integration: MALFORMED response → recovered ToolCall
6. `any_mode_sets_allowed_function_names` — request body correctness with required_tool_names
7. `auto_mode_omits_allowed_function_names` — request body correctness without required_tool_names

## Risk

- **Low:** Struct field additions are backward-compatible (serde defaults)
- **Low:** ANY mode scoped to executor actions only; general loop unchanged
- **Low:** Parser failures are handled gracefully (escalate to retry, then fail)
- **Medium:** Python kwargs parser may encounter edge cases with nested quotes or non-string values. Mitigated by falling through to ANY retry.

## Rollback

Revert the commit. All changes are additive; removal restores prior behavior exactly.
