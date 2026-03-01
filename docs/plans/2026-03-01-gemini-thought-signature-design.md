# Gemini Thought Signature Round-Trip — Design

**Date:** 2026-03-01
**Slug:** `gemini-thought-signature`
**Risk tier:** High (providers, runtime behavior)

## Problem

Gemini 3 thinking models require `thoughtSignature` to be round-tripped exactly on every `Part` of a model response. Three things break this contract:

1. **`extract_response()`** collapses `Vec<ResponsePart>` into `(Option<String>, Vec<ToolCall>)`. Only signatures on `functionCall` parts survive (via `ToolCall.thought_signature`). Signatures on thinking text parts or standalone signature parts are lost.

2. **History replay** reconstructs model turns from `tool_calls` JSON, rebuilding only `functionCall` parts and an optional leading text part. Original thinking parts with their signatures are not reconstructed.

3. **Text-only model responses** are stored as plain strings, losing all thinking parts and signatures.

After the model's first tool-call exchange, all subsequent messages return zero candidates — Gemini silently rejects the conversation because required signatures are missing from the replayed history.

## Solution

Store the raw response parts alongside the extracted text and tool calls. Replay them verbatim instead of reconstructing from tool calls.

### Data Flow (After Fix)

```
Gemini API Response
  └─ ResponsePart[] (deserialized, all data intact)
       └─ extract_response()
            ├─ (text, tool_calls)           → agent loop for execution + display
            └─ Vec<Part> (raw_parts)        → faithful copy of all response parts
                 └─ ChatResponse.provider_parts (serialized as Vec<Value>)
                      └─ build_native_assistant_history()
                           └─ JSON: {"content": "...", "tool_calls": [...], "raw_model_parts": [...]}
                                └─ ChatMessage::assistant(json_string)
                                     └─ Next call: Gemini replay reads raw_model_parts
                                          └─ Vec<Part> deserialized directly
                                               └─ Sent to Gemini API (faithful round-trip)
```

### Changes by File

**`src/providers/gemini.rs`**

1. `Part` struct: add `Deserialize` derive, add `#[serde(default)]` on `Option` fields for round-trip.
2. `FunctionCallPart`: add `Deserialize` derive.
3. `extract_response()`: return `(Option<String>, Vec<ToolCall>, Vec<Part>)`. Third element is `ResponsePart` → `Part` conversion preserving all fields.
4. `GeminiResponse`: add `raw_parts: Vec<Part>` field.
5. `chat()` and `chat_with_tools()` response construction: populate `provider_parts` on `ChatResponse` by serializing `raw_parts` to `Vec<serde_json::Value>`.
6. `chat()` history replay (~lines 1786-1831): delete reconstruction-from-tool_calls block. Replace with: deserialize `raw_model_parts` → `Vec<Part>`, populate `tool_id_to_name` from `tool_calls`.
7. `chat_with_tools()` history replay (~lines 1977-2023): same replacement.

**`src/providers/traits.rs`**

8. `ChatResponse`: add `provider_parts: Option<Vec<serde_json::Value>>` field.

**`src/agent/loop_.rs`**

9. `build_native_assistant_history()`: accept `provider_parts: Option<&[serde_json::Value]>` parameter. Serialize as `"raw_model_parts"` key in the JSON envelope when present.
10. Serialization decision: if `provider_parts.is_some()`, always use JSON envelope (even for text-only responses with empty `tool_calls`).
11. All call sites for `build_native_assistant_history`: pass `provider_parts` through.

**All other providers**

12. Set `provider_parts: None` in their `ChatResponse` construction. No behavior change.

### ResponsePart → Part Conversion

```rust
Part {
    text: rp.text,
    thought: if rp.thought { Some(true) } else { None },
    thought_signature: rp.thought_signature,
    function_call: rp.function_call.map(|fc| FunctionCallPart {
        name: fc.name,
        args: fc.args,
    }),
    function_response: None,
}
```

### Serialized History Format (After Fix)

Tool-call response:
```json
{
  "content": "Let me check that for you.",
  "tool_calls": [
    {"id": "gemini_call_0", "name": "search", "arguments": "{}", "thought_signature": "sig2"}
  ],
  "raw_model_parts": [
    {"thought": true, "text": "I should search for this.", "thoughtSignature": "sig1"},
    {"functionCall": {"name": "search", "args": {}}, "thoughtSignature": "sig2"}
  ]
}
```

Text-only response with thinking:
```json
{
  "content": "Here is the answer.",
  "tool_calls": [],
  "raw_model_parts": [
    {"thought": true, "text": "Let me reason through this.", "thoughtSignature": "sig1"},
    {"text": "Here is the answer.", "thoughtSignature": "sig2"}
  ]
}
```

### Backward Compatibility

Not maintained. Old history entries without `raw_model_parts` will fall through to the plain-text assistant fallback (bare text part, no signatures). This is acceptable because:
- The old format was already broken (missing signatures caused zero candidates).
- In-memory conversation history is cleared on restart.
- There is no persistent history store that would contain old-format entries.

### What Gets Deleted

- Reconstruction-from-tool_calls blocks in both `chat()` and `chat_with_tools()` (~80 lines total).
- The `thought_signature` field on `ToolCall` becomes vestigial for Gemini replay (still serialized for completeness, still used by `build_native_assistant_history` for the `tool_calls` array, but replay never reads it).

### Error Handling

- If `raw_model_parts` is present but deserializes to an empty `Vec<Part>`, log a warning and fall through to the plain-text fallback. This prevents a corrupt entry from crashing the conversation.
- If `raw_model_parts` is absent on a structured message (has `tool_calls`), fall through to plain-text fallback. This handles edge cases where a non-thinking Gemini model produced the turn.

## Non-Goals

- Changing the `ToolCall` struct or removing `thought_signature` from it.
- Modifying how other providers serialize/deserialize history.
- Persistent history migration (no persistent store exists).

## Rollback

Revert the commit. Behavior returns to pre-fix state (signatures lost, zero candidates after tool calls with thinking models).
