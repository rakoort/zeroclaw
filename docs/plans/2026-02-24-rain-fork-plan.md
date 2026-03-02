# Rain Fork Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Port three capabilities from OpenClaw to ZeroClaw: Gemini schema/transcript preprocessing, Slack context awareness (thread hydration, mention gating, pending history), and Linear-as-brain workspace config.

**Architecture:** Surgical changes in isolated files. Gemini sanitization is a new pure-function module (`gemini_sanitize.rs`). Slack changes are confined to `slack.rs`, `traits.rs`, and `mod.rs`. Linear is config-only (no Rust). All changes stay close to upstream for easy rebasing.

**Tech Stack:** Rust, serde_json, reqwest, tokio (async), Slack Web API

**Design doc:** `docs/plans/2026-02-24-rain-fork-design.md`

---

## Why These Changes Work in OpenClaw But Not ZeroClaw

### Gemini Preprocessing

OpenClaw (TypeScript) has two dedicated sanitization layers totaling ~660 lines:

1. **Schema sanitizer** (`clean-for-gemini.ts`): Strips 22 unsupported JSON Schema keywords, resolves `$ref` inline with cycle detection, converts `const` to `enum`, flattens `anyOf`/`oneOf` literal unions, strips null variants, normalizes type arrays. Without this, Gemini rejects tool schemas containing `patternProperties`, `additionalProperties`, `$ref`, `const`, `format`, etc.

2. **Transcript sanitizer** (`google.ts` + `session-transcript-repair.ts`): Rewrites tool call IDs to alphanumeric-only (Gemini rejects IDs with `_`, `-`, etc.), merges consecutive same-role messages (Gemini requires strict alternation), prepends synthetic user message if history starts with assistant, repairs orphaned tool_use/tool_result pairs, strips invalid thought signatures.

ZeroClaw has **zero** sanitization. Messages and tool schemas pass through raw to Gemini. The Gemini provider doesn't even implement `chat_with_tools` — it uses PromptGuided text format. But even basic `chat_with_history` fails because conversation history accumulated from tool-calling loops (with other providers) contains non-alphanumeric tool call IDs, consecutive same-role messages, and orphaned tool artifacts that Gemini rejects with 500s.

### Slack Context

OpenClaw has a sophisticated Slack pipeline (~800 lines across `src/slack/monitor/`):

1. **Thread hydration**: Fetches `conversations.replies` with cursor pagination, sliding-window retention (latest N messages), 6-hour cached thread starters, batch user name resolution. ZeroClaw only polls `conversations.history` — thread replies are invisible.

2. **Mention gating**: Three detection paths — explicit `<@BOT_USER_ID>`, configurable regex patterns, implicit mention (reply in bot's thread). Non-mention messages are silently buffered. ZeroClaw responds to every message from an allowed user.

3. **Pending history buffer**: Per-channel ring buffer (max 50) of non-mention messages. When bot IS mentioned, buffer is prepended as `[Chat messages since your last reply - for context]`. ZeroClaw has no such buffer.

4. **Session scoping**: OpenClaw uses `{agent}:{channel}:thread:{thread_ts}` — per-thread, not per-sender. ZeroClaw uses `{channel}_{thread_ts}_{sender}` — per-sender isolation means the bot sees siloed views of the same thread.

5. **Envelope format**: OpenClaw wraps messages as `[Slack #channel sender +elapsed timestamp] sender: text`. ZeroClaw sends raw message content.

### Linear Brain

This is pure workspace config (AGENTS.md rules, skills). No Rust changes needed. OpenClaw uses prompt-level instructions to enforce Linear-first behavior. ZeroClaw's skill/workspace system can do the same.

---

## Phase 1: Gemini Schema Sanitization

### Task 1: Schema keyword stripping and basic transforms

**Files:**
- Create: `src/providers/gemini_sanitize.rs`
- Modify: `src/providers/mod.rs` (add `pub mod gemini_sanitize;`)
- Test: inline `#[cfg(test)]` module

This task implements the recursive JSON Schema cleaner that strips unsupported keywords, converts `const` to `enum`, and normalizes type arrays. These are the simplest transforms — no recursion into `$ref` or union flattening yet.

**Step 1: Write the failing tests**

Create `src/providers/gemini_sanitize.rs` with:

```rust
//! Gemini API preprocessing: tool-schema sanitization and transcript repair.
//!
//! Gemini rejects JSON Schema keywords it does not support and requires strict
//! transcript formatting. These pure functions sanitize schemas and message
//! history before they reach the Gemini API.
//!
//! Ported from OpenClaw's `clean-for-gemini.ts` and `google.ts`.

use serde_json::Value;

/// JSON Schema keywords that Gemini does not support.
const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "patternProperties",
    "additionalProperties",
    "$schema",
    "$id",
    "$ref",
    "$defs",
    "definitions",
    "examples",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "multipleOf",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];

/// Sanitize a JSON Schema for Gemini compatibility.
///
/// Recursively walks the schema tree, stripping unsupported keywords,
/// resolving `$ref` references, flattening unions, and normalizing types.
/// Returns a new schema value; the input is not modified.
pub fn sanitize_schema_for_gemini(schema: &Value) -> Value {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Keyword stripping ───────────────────────────────────────────

    #[test]
    fn strips_unsupported_top_level_keywords() {
        let input = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "additionalProperties": false,
            "patternProperties": { "^x-": {} },
            "$schema": "http://json-schema.org/draft-07/schema#"
        });
        let result = sanitize_schema_for_gemini(&input);
        assert!(result.get("additionalProperties").is_none());
        assert!(result.get("patternProperties").is_none());
        assert!(result.get("$schema").is_none());
        // Keeps supported keywords
        assert_eq!(result["type"], "object");
        assert!(result.get("properties").is_some());
    }

    #[test]
    fn strips_unsupported_keywords_in_nested_properties() {
        let input = json!({
            "type": "object",
            "properties": {
                "email": {
                    "type": "string",
                    "format": "email",
                    "minLength": 1,
                    "maxLength": 255
                }
            }
        });
        let result = sanitize_schema_for_gemini(&input);
        let email = &result["properties"]["email"];
        assert_eq!(email["type"], "string");
        assert!(email.get("format").is_none());
        assert!(email.get("minLength").is_none());
        assert!(email.get("maxLength").is_none());
    }

    #[test]
    fn strips_unsupported_keywords_in_array_items() {
        let input = json!({
            "type": "array",
            "items": {
                "type": "string",
                "minLength": 1,
                "pattern": "^[a-z]+$"
            },
            "minItems": 1,
            "maxItems": 10,
            "uniqueItems": true
        });
        let result = sanitize_schema_for_gemini(&input);
        assert!(result.get("minItems").is_none());
        assert!(result.get("maxItems").is_none());
        assert!(result.get("uniqueItems").is_none());
        let items = &result["items"];
        assert_eq!(items["type"], "string");
        assert!(items.get("minLength").is_none());
        assert!(items.get("pattern").is_none());
    }

    // ── const to enum conversion ────────────────────────────────────

    #[test]
    fn converts_const_to_single_enum() {
        let input = json!({
            "type": "object",
            "properties": {
                "action": { "const": "delete" }
            }
        });
        let result = sanitize_schema_for_gemini(&input);
        let action = &result["properties"]["action"];
        assert!(action.get("const").is_none());
        assert_eq!(action["enum"], json!(["delete"]));
    }

    // ── type array normalization ────────────────────────────────────

    #[test]
    fn normalizes_type_array_to_single_type() {
        let input = json!({
            "type": ["string", "null"]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert_eq!(result["type"], "string");
    }

    #[test]
    fn normalizes_type_array_strips_null() {
        let input = json!({
            "type": ["integer", "null"]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert_eq!(result["type"], "integer");
    }

    // ── passthrough ─────────────────────────────────────────────────

    #[test]
    fn passes_through_simple_valid_schema() {
        let input = json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                }
            },
            "required": ["query"]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert_eq!(result, input);
    }

    #[test]
    fn handles_empty_schema() {
        let input = json!({});
        let result = sanitize_schema_for_gemini(&input);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn handles_non_object_schema() {
        let input = json!("string");
        let result = sanitize_schema_for_gemini(&input);
        assert_eq!(result, json!("string"));
    }
}
```

**Step 2: Register the module**

Add `pub mod gemini_sanitize;` to `src/providers/mod.rs`.

**Step 3: Run tests to verify they fail**

Run: `cargo test --lib providers::gemini_sanitize -- --nocapture`
Expected: All tests FAIL with `not yet implemented`

**Step 4: Implement the schema sanitizer**

Replace the `todo!()` in `sanitize_schema_for_gemini` with:

```rust
pub fn sanitize_schema_for_gemini(schema: &Value) -> Value {
    clean_schema_recursive(schema, &mut std::collections::HashSet::new())
}

fn clean_schema_recursive(
    schema: &Value,
    ref_stack: &mut std::collections::HashSet<String>,
) -> Value {
    let obj = match schema.as_object() {
        Some(o) => o,
        None => return schema.clone(),
    };

    let mut result = serde_json::Map::new();

    for (key, value) in obj {
        // Strip unsupported keywords
        if UNSUPPORTED_KEYWORDS.contains(&key.as_str()) {
            continue;
        }

        match key.as_str() {
            // Convert const to enum
            "const" => {
                result.insert("enum".to_string(), Value::Array(vec![value.clone()]));
            }
            // Recurse into properties
            "properties" => {
                if let Some(props) = value.as_object() {
                    let mut cleaned = serde_json::Map::new();
                    for (prop_name, prop_schema) in props {
                        cleaned.insert(
                            prop_name.clone(),
                            clean_schema_recursive(prop_schema, ref_stack),
                        );
                    }
                    result.insert(key.clone(), Value::Object(cleaned));
                } else {
                    result.insert(key.clone(), value.clone());
                }
            }
            // Recurse into items
            "items" => {
                result.insert(key.clone(), clean_schema_recursive(value, ref_stack));
            }
            // Normalize type arrays
            "type" => {
                if let Some(arr) = value.as_array() {
                    let non_null: Vec<&Value> = arr
                        .iter()
                        .filter(|v| v.as_str() != Some("null"))
                        .collect();
                    if non_null.len() == 1 {
                        result.insert(key.clone(), non_null[0].clone());
                    } else if non_null.is_empty() {
                        result.insert(key.clone(), Value::String("string".to_string()));
                    } else {
                        result.insert(key.clone(), non_null[0].clone());
                    }
                } else {
                    result.insert(key.clone(), value.clone());
                }
            }
            // Keep everything else
            _ => {
                result.insert(key.clone(), value.clone());
            }
        }
    }

    Value::Object(result)
}
```

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib providers::gemini_sanitize -- --nocapture`
Expected: All tests PASS

**Step 6: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 7: Commit**

```bash
git add src/providers/gemini_sanitize.rs src/providers/mod.rs
git commit -m "feat(gemini): add schema sanitizer — keyword stripping, const-to-enum, type normalization

Gemini rejects 22 JSON Schema keywords it doesn't support. Port the
recursive schema cleaner from OpenClaw's clean-for-gemini.ts. This is
the first of three sanitization layers needed to unblock Gemini 3 Flash."
```

---

### Task 2: Schema $ref resolution and union flattening

**Files:**
- Modify: `src/providers/gemini_sanitize.rs`

This task adds the more complex transforms: `$ref` inline resolution with cycle detection, `anyOf`/`oneOf` literal flattening, null variant stripping, and single-variant unwrapping. These are the transforms that handle real-world tool schemas from MCP servers and complex tool definitions.

**Step 1: Write the failing tests**

Add to the `tests` module:

```rust
    // ── $ref resolution ─────────────────────────────────────────────

    #[test]
    fn resolves_ref_inline() {
        let input = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" }
            },
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            }
        });
        let result = sanitize_schema_for_gemini(&input);
        // $ref replaced with inlined definition
        let item = &result["properties"]["item"];
        assert_eq!(item["type"], "object");
        assert!(item["properties"]["name"]["type"] == "string");
        // $defs stripped
        assert!(result.get("$defs").is_none());
    }

    #[test]
    fn resolves_ref_from_definitions() {
        let input = json!({
            "type": "object",
            "properties": {
                "status": { "$ref": "#/definitions/Status" }
            },
            "definitions": {
                "Status": {
                    "type": "string",
                    "enum": ["active", "inactive"]
                }
            }
        });
        let result = sanitize_schema_for_gemini(&input);
        let status = &result["properties"]["status"];
        assert_eq!(status["type"], "string");
        assert_eq!(status["enum"], json!(["active", "inactive"]));
    }

    #[test]
    fn circular_ref_replaced_with_empty_object() {
        let input = json!({
            "type": "object",
            "properties": {
                "child": { "$ref": "#/$defs/Node" }
            },
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": { "$ref": "#/$defs/Node" }
                    }
                }
            }
        });
        let result = sanitize_schema_for_gemini(&input);
        // Should not infinite loop; circular ref becomes {}
        let child = &result["properties"]["child"];
        assert_eq!(child["type"], "object");
        let nested = &child["properties"]["child"];
        assert_eq!(*nested, json!({}));
    }

    // ── anyOf/oneOf flattening ──────────────────────────────────────

    #[test]
    fn flattens_literal_any_of_to_enum() {
        let input = json!({
            "anyOf": [
                { "const": "low" },
                { "const": "medium" },
                { "const": "high" }
            ]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert!(result.get("anyOf").is_none());
        assert_eq!(result["type"], "string");
        assert_eq!(result["enum"], json!(["low", "medium", "high"]));
    }

    #[test]
    fn flattens_enum_any_of_to_merged_enum() {
        let input = json!({
            "anyOf": [
                { "type": "string", "enum": ["a", "b"] },
                { "type": "string", "enum": ["c", "d"] }
            ]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert!(result.get("anyOf").is_none());
        assert_eq!(result["type"], "string");
        assert_eq!(result["enum"], json!(["a", "b", "c", "d"]));
    }

    #[test]
    fn strips_null_from_any_of_and_unwraps_single_variant() {
        let input = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "null" }
            ]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert!(result.get("anyOf").is_none());
        assert_eq!(result["type"], "string");
    }

    #[test]
    fn strips_type_when_any_of_present() {
        let input = json!({
            "type": "object",
            "anyOf": [
                {
                    "type": "object",
                    "properties": { "a": { "type": "string" } }
                },
                {
                    "type": "object",
                    "properties": { "b": { "type": "integer" } }
                }
            ]
        });
        let result = sanitize_schema_for_gemini(&input);
        // When anyOf survives (non-literal), type should be stripped
        // because Gemini rejects type + anyOf together
        if result.get("anyOf").is_some() {
            assert!(result.get("type").is_none());
        }
    }

    #[test]
    fn one_of_treated_same_as_any_of() {
        let input = json!({
            "oneOf": [
                { "const": "read" },
                { "const": "write" }
            ]
        });
        let result = sanitize_schema_for_gemini(&input);
        assert!(result.get("oneOf").is_none());
        assert_eq!(result["enum"], json!(["read", "write"]));
    }

    #[test]
    fn nested_any_of_in_properties_flattened() {
        let input = json!({
            "type": "object",
            "properties": {
                "priority": {
                    "anyOf": [
                        { "const": "p0" },
                        { "const": "p1" },
                        { "type": "null" }
                    ]
                }
            }
        });
        let result = sanitize_schema_for_gemini(&input);
        let priority = &result["properties"]["priority"];
        assert!(priority.get("anyOf").is_none());
        assert_eq!(priority["enum"], json!(["p0", "p1"]));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib providers::gemini_sanitize -- --nocapture`
Expected: New tests FAIL (existing tests still pass)

**Step 3: Implement $ref resolution**

Add a `defs` parameter to `clean_schema_recursive` and resolve `$ref` pointers:

```rust
/// Top-level entry: extract $defs/definitions, then recurse.
pub fn sanitize_schema_for_gemini(schema: &Value) -> Value {
    let defs = extract_defs(schema);
    clean_schema_recursive(schema, &defs, &mut std::collections::HashSet::new())
}

/// Collect definitions from $defs and definitions blocks.
fn extract_defs(schema: &Value) -> serde_json::Map<String, Value> {
    let mut defs = serde_json::Map::new();
    if let Some(obj) = schema.as_object() {
        for key in &["$defs", "definitions"] {
            if let Some(Value::Object(d)) = obj.get(*key) {
                for (name, def) in d {
                    defs.insert(name.clone(), def.clone());
                }
            }
        }
    }
    defs
}

/// Resolve a $ref pointer like "#/$defs/Item" or "#/definitions/Status".
fn resolve_ref<'a>(
    ref_path: &str,
    defs: &'a serde_json::Map<String, Value>,
) -> Option<&'a Value> {
    // Only handle local refs: #/$defs/Name or #/definitions/Name
    let stripped = ref_path.strip_prefix("#/")?;
    let parts: Vec<&str> = stripped.splitn(2, '/').collect();
    if parts.len() == 2 && (parts[0] == "$defs" || parts[0] == "definitions") {
        defs.get(parts[1])
    } else {
        None
    }
}
```

Then update `clean_schema_recursive` to handle `$ref`:

- Check if object has `$ref` key
- If so, resolve it from defs
- Check for cycles using ref_stack (insert ref path, recurse, remove)
- If circular, return `{}`
- If resolved, recursively clean the resolved schema

**Step 4: Implement union flattening**

Add helper functions:

```rust
/// Check if a schema represents null type.
fn is_null_schema(schema: &Value) -> bool {
    match schema {
        Value::Object(obj) => {
            obj.get("type").and_then(|t| t.as_str()) == Some("null")
                || obj.get("const") == Some(&Value::Null)
                || obj.get("enum").and_then(|e| e.as_array())
                    == Some(&vec![Value::Null])
        }
        _ => false,
    }
}

/// Try to flatten anyOf/oneOf into a single enum.
fn try_flatten_literal_union(variants: &[Value]) -> Option<Value> {
    let mut values = Vec::new();
    let mut common_type: Option<String> = None;

    for variant in variants {
        if is_null_schema(variant) {
            continue;
        }
        let obj = variant.as_object()?;
        if let Some(c) = obj.get("const") {
            let typ = infer_type(c);
            match &common_type {
                None => common_type = Some(typ),
                Some(ct) if *ct != typ => return None,
                _ => {}
            }
            values.push(c.clone());
        } else if let Some(arr) = obj.get("enum").and_then(|e| e.as_array()) {
            let typ = obj.get("type").and_then(|t| t.as_str())
                .unwrap_or("string").to_string();
            match &common_type {
                None => common_type = Some(typ),
                Some(ct) if *ct != typ => return None,
                _ => {}
            }
            values.extend(arr.iter().cloned());
        } else {
            return None;
        }
    }

    if values.is_empty() {
        return None;
    }

    Some(json!({
        "type": common_type.unwrap_or_else(|| "string".to_string()),
        "enum": values
    }))
}
```

Then update the recursive cleaner to handle `anyOf`/`oneOf`:
1. Strip null variants
2. Try literal flattening
3. If only one variant remains after null stripping, unwrap it
4. If flattening fails and anyOf survives, strip `type` (Gemini rejects type + anyOf)
5. As last resort, pick representative type from first variant

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib providers::gemini_sanitize -- --nocapture`
Expected: All tests PASS

**Step 6: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 7: Commit**

```bash
git add src/providers/gemini_sanitize.rs
git commit -m "feat(gemini): add \$ref resolution and union flattening to schema sanitizer

Resolves \$ref pointers inline with cycle detection (circular refs become
empty objects). Flattens anyOf/oneOf literal unions into enum arrays.
Strips null variants from unions and unwraps single-variant wrappers.
Handles nested unions in properties — OpenClaw PR #22825 found that
first-pass cleaning missed these."
```

---

### Task 3: Transcript sanitization

**Files:**
- Modify: `src/providers/gemini_sanitize.rs`

This task implements transcript preprocessing: tool call ID rewriting, turn ordering fixes, consecutive same-role merging, and tool_use/tool_result pairing repair. These are the changes that fix the actual 500 errors from Gemini.

**Step 1: Write the failing tests**

Add to `gemini_sanitize.rs`:

```rust
use crate::providers::traits::ChatMessage;

/// Sanitize a conversation transcript for Gemini compatibility.
///
/// Rewrites non-alphanumeric tool call IDs, merges consecutive same-role
/// messages, prepends a synthetic user turn if history starts with assistant,
/// and repairs orphaned tool_use/tool_result pairs.
pub fn sanitize_transcript_for_gemini(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    todo!()
}
```

Tests:

```rust
    // ── Transcript: tool call ID sanitization ───────────────────────

    #[test]
    fn rewrites_non_alphanumeric_tool_call_ids() {
        let content = r#"<tool_call id="call_abc-123_def">{"name":"test"}</tool_call>"#;
        let messages = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant(content),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        // IDs should be alphanumeric only
        assert!(!result[1].content.contains("call_abc-123_def"));
        // The rewritten ID should only contain [a-zA-Z0-9]
        if let Some(start) = result[1].content.find("id=\"") {
            let after = &result[1].content[start + 4..];
            let end = after.find('"').unwrap();
            let id = &after[..end];
            assert!(id.chars().all(|c| c.is_ascii_alphanumeric()), "ID must be alphanumeric: {id}");
        }
    }

    // ── Transcript: turn ordering ───────────────────────────────────

    #[test]
    fn prepends_user_turn_if_starts_with_assistant() {
        let messages = vec![
            ChatMessage::assistant("I can help"),
            ChatMessage::user("thanks"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[1].role, "assistant");
        assert_eq!(result[1].content, "I can help");
    }

    #[test]
    fn merges_consecutive_same_role_messages() {
        let messages = vec![
            ChatMessage::user("hello"),
            ChatMessage::user("how are you"),
            ChatMessage::assistant("I'm fine"),
            ChatMessage::assistant("How can I help?"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "user");
        assert!(result[0].content.contains("hello"));
        assert!(result[0].content.contains("how are you"));
        assert_eq!(result[1].role, "assistant");
        assert!(result[1].content.contains("I'm fine"));
        assert!(result[1].content.contains("How can I help?"));
    }

    #[test]
    fn preserves_system_messages_at_start() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[1].role, "user");
        assert_eq!(result[2].role, "assistant");
    }

    #[test]
    fn no_change_for_clean_transcript() {
        let messages = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[1].content, "hi");
    }

    #[test]
    fn handles_empty_transcript() {
        let result = sanitize_transcript_for_gemini(&[]);
        assert!(result.is_empty());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib providers::gemini_sanitize -- --nocapture`
Expected: New transcript tests FAIL, schema tests still PASS

**Step 3: Implement transcript sanitizer**

```rust
pub fn sanitize_transcript_for_gemini(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return vec![];
    }

    let mut result: Vec<ChatMessage> = Vec::with_capacity(messages.len());

    // Step 1: Rewrite non-alphanumeric tool call IDs
    let messages: Vec<ChatMessage> = messages
        .iter()
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: rewrite_tool_call_ids(&m.content),
        })
        .collect();

    // Step 2: Collect, skipping system messages (pass through)
    let mut system_msgs: Vec<ChatMessage> = vec![];
    let mut non_system: Vec<ChatMessage> = vec![];
    for m in &messages {
        if m.role == "system" {
            system_msgs.push(m.clone());
        } else {
            non_system.push(m.clone());
        }
    }

    // Step 3: Prepend synthetic user turn if starts with assistant
    if non_system.first().map(|m| m.role.as_str()) == Some("assistant") {
        non_system.insert(0, ChatMessage::user("(session bootstrap)"));
    }

    // Step 4: Merge consecutive same-role messages
    let mut merged: Vec<ChatMessage> = vec![];
    for m in non_system {
        if let Some(last) = merged.last_mut() {
            if last.role == m.role {
                last.content.push('\n');
                last.content.push_str(&m.content);
                continue;
            }
        }
        merged.push(m);
    }

    // Reassemble: system first, then merged
    result.extend(system_msgs);
    result.extend(merged);
    result
}

/// Rewrite tool_call id attributes to alphanumeric-only.
fn rewrite_tool_call_ids(content: &str) -> String {
    use std::collections::HashMap;
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut result = content.to_string();

    // Match id="..." patterns in tool_call tags
    let re_pattern = "id=\"";
    let mut search_from = 0;
    while let Some(start) = result[search_from..].find(re_pattern) {
        let abs_start = search_from + start + re_pattern.len();
        if let Some(end) = result[abs_start..].find('"') {
            let abs_end = abs_start + end;
            let old_id = result[abs_start..abs_end].to_string();
            if !old_id.chars().all(|c| c.is_ascii_alphanumeric()) {
                let new_id = id_map
                    .entry(old_id.clone())
                    .or_insert_with(|| {
                        old_id.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
                    })
                    .clone();
                result.replace_range(abs_start..abs_end, &new_id);
                search_from = abs_start + new_id.len() + 1;
            } else {
                search_from = abs_end + 1;
            }
        } else {
            break;
        }
    }
    result
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib providers::gemini_sanitize -- --nocapture`
Expected: All tests PASS

**Step 5: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 6: Commit**

```bash
git add src/providers/gemini_sanitize.rs
git commit -m "feat(gemini): add transcript sanitizer — ID rewriting, turn ordering, merge

Gemini requires strict user/assistant alternation and alphanumeric-only
tool call IDs. Prepend synthetic user turn when transcript starts with
assistant. Merge consecutive same-role messages. Rewrite tool_call IDs
to strip non-alphanumeric characters. Ported from OpenClaw's
sanitizeSessionHistory pipeline."
```

---

### Task 4: Integrate sanitization into Gemini provider

**Files:**
- Modify: `src/providers/gemini.rs`

Wire the sanitization functions into the Gemini provider's request path.

**Step 1: Write integration test**

Create `tests/gemini_sanitize_integration.rs`:

```rust
//! Integration test: verify Gemini sanitization functions are accessible
//! from the provider crate's public API.

use zeroclaw::providers::gemini_sanitize::{
    sanitize_schema_for_gemini, sanitize_transcript_for_gemini,
};
use zeroclaw::providers::traits::ChatMessage;

#[test]
fn schema_sanitizer_accessible_and_functional() {
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "q": { "type": "string", "format": "uri" }
        }
    });
    let result = sanitize_schema_for_gemini(&schema);
    assert!(result.get("additionalProperties").is_none());
    assert!(result["properties"]["q"].get("format").is_none());
}

#[test]
fn transcript_sanitizer_accessible_and_functional() {
    let messages = vec![
        ChatMessage::assistant("oops"),
        ChatMessage::user("hello"),
    ];
    let result = sanitize_transcript_for_gemini(&messages);
    assert_eq!(result[0].role, "user");
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test --test gemini_sanitize_integration -- --nocapture`
Expected: PASS (functions are already public)

**Step 3: Add sanitization call in `chat_with_history`**

In `src/providers/gemini.rs`, at the start of `chat_with_history`, add:

```rust
use crate::providers::gemini_sanitize::sanitize_transcript_for_gemini;

// Inside chat_with_history, before the message conversion loop:
let messages = sanitize_transcript_for_gemini(messages);
```

**Step 4: Add sanitization call in `chat` method**

In the `chat` method, similarly sanitize `request.messages` before conversion.

**Step 5: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 6: Commit**

```bash
git add src/providers/gemini.rs tests/gemini_sanitize_integration.rs
git commit -m "feat(gemini): wire transcript sanitizer into chat_with_history and chat

Apply sanitize_transcript_for_gemini before message conversion in the
Gemini provider. This fixes 500 errors caused by non-alphanumeric tool
call IDs, consecutive same-role messages, and transcripts starting with
assistant turns."
```

---

## Phase 2: Slack Context Awareness

### Task 5: Add context fields to ChannelMessage and fix session scoping

**Files:**
- Modify: `src/channels/traits.rs`
- Modify: `src/channels/mod.rs`
- Test: `tests/channel_routing.rs` (add session scoping test)

This task adds the structural fields needed by later tasks and fixes the per-sender session scoping bug.

**Step 1: Write failing test for session scoping**

Add to `tests/channel_routing.rs` (or create new test file if needed):

```rust
#[test]
fn conversation_history_key_is_per_thread_not_per_sender() {
    // Two users in the same thread should share a conversation key
    let msg_alice = ChannelMessage {
        id: "1".into(),
        sender: "alice".into(),
        reply_target: "C123".into(),
        content: "hello".into(),
        channel: "slack".into(),
        timestamp: 1000,
        thread_ts: Some("thread_001".into()),
        thread_starter_body: None,
        thread_history: None,
    };
    let msg_bob = ChannelMessage {
        id: "2".into(),
        sender: "bob".into(),
        reply_target: "C123".into(),
        content: "hi".into(),
        channel: "slack".into(),
        timestamp: 1001,
        thread_ts: Some("thread_001".into()),
        thread_starter_body: None,
        thread_history: None,
    };
    let key_alice = conversation_history_key(&msg_alice);
    let key_bob = conversation_history_key(&msg_bob);
    assert_eq!(key_alice, key_bob, "same thread should have same history key");
}
```

Note: This test requires `conversation_history_key` to be `pub(crate)` or the test needs to be in-module. Adjust access as needed.

**Step 2: Run test to verify it fails**

Expected: FAIL because current key includes sender: `slack_thread_001_alice` != `slack_thread_001_bob`

**Step 3: Add fields to ChannelMessage**

In `src/channels/traits.rs`, add to `ChannelMessage`:

```rust
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub reply_target: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
    pub thread_ts: Option<String>,
    /// Body of the thread's root message (fetched via thread hydration).
    pub thread_starter_body: Option<String>,
    /// Formatted thread history (prepended to context for thread replies).
    pub thread_history: Option<String>,
}
```

**Step 4: Fix all ChannelMessage construction sites**

Every place that constructs a `ChannelMessage` needs the new fields set to `None`. Search for `ChannelMessage {` across the codebase and add:
```rust
thread_starter_body: None,
thread_history: None,
```

This includes `slack.rs`, `discord.rs`, `telegram.rs`, `whatsapp.rs`, and any test files.

**Step 5: Fix session scoping**

In `src/channels/mod.rs`, change `conversation_history_key`:

```rust
fn conversation_history_key(msg: &traits::ChannelMessage) -> String {
    // Per-thread isolation (not per-sender) so the bot sees the full thread
    match &msg.thread_ts {
        Some(tid) => format!("{}_{}", msg.channel, tid),
        None => format!("{}_{}", msg.channel, msg.sender),
    }
}
```

The key change: remove `msg.sender` from the threaded case. Non-threaded messages keep per-sender scoping (DMs and top-level channel messages).

**Step 6: Run tests to verify they pass**

Run: `cargo test -- --nocapture`
Expected: All tests PASS including new session scoping test

**Step 7: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 8: Commit**

```bash
git add src/channels/traits.rs src/channels/mod.rs src/channels/slack.rs \
  src/channels/discord.rs src/channels/telegram.rs src/channels/whatsapp.rs \
  tests/channel_routing.rs
git commit -m "feat(slack): add thread context fields to ChannelMessage, fix session scoping

Add thread_starter_body and thread_history fields to ChannelMessage for
thread hydration support. Change conversation_history_key to scope by
thread (not sender) so the bot sees the full thread conversation instead
of per-sender silos. OpenClaw uses per-thread session keys for the same
reason."
```

---

### Task 6: Thread hydration

**Files:**
- Modify: `src/channels/slack.rs`

Fetch thread replies when an incoming message has `thread_ts`, format them as context, and populate the new `ChannelMessage` fields.

**Step 1: Write the failing test**

Add an inline test module to `slack.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_thread_reply_labels_bot_as_assistant() {
        let reply = serde_json::json!({
            "user": "U_BOT",
            "text": "I can help with that",
            "ts": "1234567890.000200"
        });
        let formatted = format_thread_reply(&reply, "U_BOT", "bot_name", &HashMap::new());
        assert!(formatted.contains("(assistant)"));
        assert!(formatted.contains("I can help with that"));
    }

    #[test]
    fn format_thread_reply_labels_human_as_user() {
        let reply = serde_json::json!({
            "user": "U_HUMAN",
            "text": "hello there",
            "ts": "1234567890.000100"
        });
        let formatted = format_thread_reply(&reply, "U_BOT", "bot_name", &HashMap::new());
        assert!(formatted.contains("(user)"));
        assert!(formatted.contains("hello there"));
    }

    #[test]
    fn thread_history_format_includes_envelope() {
        let replies = vec![
            serde_json::json!({
                "user": "U_ALICE",
                "text": "started the thread",
                "ts": "1234567890.000100"
            }),
            serde_json::json!({
                "user": "U_BOT",
                "text": "how can I help?",
                "ts": "1234567890.000200"
            }),
        ];
        let formatted = format_thread_history(&replies, "U_BOT", "Rain", "#general", &HashMap::new());
        assert!(formatted.contains("[Slack #general"));
        assert!(formatted.contains("(user)"));
        assert!(formatted.contains("(assistant)"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib channels::slack::tests -- --nocapture`
Expected: FAIL — functions don't exist yet

**Step 3: Implement thread reply formatting helpers**

```rust
use std::collections::HashMap;
use std::time::{Duration, UNIX_EPOCH};

/// Format a single thread reply with role label and envelope.
fn format_thread_reply(
    reply: &serde_json::Value,
    bot_user_id: &str,
    bot_name: &str,
    user_names: &HashMap<String, String>,
) -> String {
    let user = reply["user"].as_str().unwrap_or("unknown");
    let text = reply["text"].as_str().unwrap_or("");
    let ts = reply["ts"].as_str().unwrap_or("0");

    let is_bot = user == bot_user_id;
    let role = if is_bot { "assistant" } else { "user" };
    let name = if is_bot {
        bot_name.to_string()
    } else {
        user_names.get(user).cloned().unwrap_or_else(|| user.to_string())
    };

    let timestamp = ts.split('.').next()
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|secs| {
            let dt = UNIX_EPOCH + Duration::from_secs(secs);
            Some(format_timestamp(dt))
        })
        .unwrap_or_default();

    format!("[Slack {{channel}} {name} ({role}) {timestamp}] {name}: {text}")
}

/// Format full thread history from a list of replies.
fn format_thread_history(
    replies: &[serde_json::Value],
    bot_user_id: &str,
    bot_name: &str,
    channel_name: &str,
    user_names: &HashMap<String, String>,
) -> String {
    replies
        .iter()
        .map(|r| {
            format_thread_reply(r, bot_user_id, bot_name, user_names)
                .replace("{channel}", channel_name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

**Step 4: Implement thread hydration in the listen loop**

Add a thread reply cache and fetch logic:

```rust
use std::sync::Mutex;
use std::time::Instant;

struct ThreadCache {
    entries: HashMap<(String, String), (Vec<serde_json::Value>, Instant)>,
}

impl ThreadCache {
    fn new() -> Self {
        Self { entries: HashMap::new() }
    }

    fn get(&self, channel: &str, thread_ts: &str) -> Option<&Vec<serde_json::Value>> {
        let key = (channel.to_string(), thread_ts.to_string());
        self.entries.get(&key).and_then(|(replies, cached_at)| {
            if cached_at.elapsed() < Duration::from_secs(60) {
                Some(replies)
            } else {
                None
            }
        })
    }

    fn insert(&mut self, channel: String, thread_ts: String, replies: Vec<serde_json::Value>) {
        // Evict stale entries (keep max 500)
        if self.entries.len() > 500 {
            self.entries.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(60));
        }
        self.entries.insert((channel, thread_ts), (replies, Instant::now()));
    }
}
```

In the `listen` method, after creating `channel_msg`, fetch thread context if `thread_ts` is set:

```rust
if let Some(ref tts) = channel_msg.thread_ts {
    // Fetch thread replies (cached 60s)
    if let Ok(replies) = fetch_thread_replies(&client, &channel_id, tts, 20).await {
        let starter_text = replies.first()
            .and_then(|r| r["text"].as_str())
            .map(String::from);
        let history = format_thread_history(&replies, &bot_user_id, "Rain", &channel_name, &user_names);
        channel_msg.thread_starter_body = starter_text;
        channel_msg.thread_history = Some(history);
    }
}
```

**Step 5: Implement fetch_thread_replies**

```rust
async fn fetch_thread_replies(
    client: &Client,
    channel: &str,
    thread_ts: &str,
    limit: usize,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let resp = client
        .get("https://slack.com/api/conversations.replies")
        .bearer_auth(&self.bot_token)  // adjust for method signature
        .query(&[
            ("channel", channel),
            ("ts", thread_ts),
            ("limit", &limit.to_string()),
        ])
        .send()
        .await?;

    let body: serde_json::Value = resp.json().await?;
    if body["ok"].as_bool() != Some(true) {
        anyhow::bail!("Slack conversations.replies failed: {}", body["error"]);
    }

    Ok(body["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}
```

**Step 6: Wire thread context into process_channel_message**

In `src/channels/mod.rs`, in the `process_channel_message` flow, prepend thread context to the message content before it enters the LLM:

```rust
// After building the user message content but before appending to history
let mut enriched_content = msg.content.clone();
if let Some(ref history) = msg.thread_history {
    enriched_content = format!(
        "[Thread context]\n{}\n\n[Current message]\n{}",
        history, enriched_content
    );
}
```

**Step 7: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 8: Commit**

```bash
git add src/channels/slack.rs src/channels/mod.rs
git commit -m "feat(slack): add thread hydration via conversations.replies

Fetch thread replies when incoming message has thread_ts. Cache replies
per (channel, thread_ts) with 60s TTL. Format each reply with role
label (assistant/user) and structured envelope. Prepend thread context
to message content before LLM processing. OpenClaw does this with a
6-hour cache and sliding-window pagination; we start simpler with 60s
TTL and a single fetch of up to 20 replies."
```

---

### Task 7: Mention gating

**Files:**
- Modify: `src/channels/slack.rs`
- Modify: `src/config/schema.rs`

Add `mention_only` config and filter messages that don't mention the bot.

**Step 1: Write the failing tests**

```rust
#[test]
fn detects_explicit_bot_mention() {
    assert!(is_mention("<@U_BOT> what do you think?", "U_BOT", None));
}

#[test]
fn detects_implicit_mention_in_bot_thread() {
    // Message in a thread where parent_user_id == bot_user_id
    assert!(is_implicit_mention("U_BOT", Some("U_BOT")));
}

#[test]
fn no_mention_for_regular_message() {
    assert!(!is_mention("hey everyone", "U_BOT", None));
}

#[test]
fn mention_regex_matches_bot_name() {
    let regex = regex::Regex::new(r"(?i)\brain\b").unwrap();
    assert!(is_mention("Rain what do you think?", "U_BOT", Some(&regex)));
}
```

**Step 2: Run tests to verify they fail**

**Step 3: Add `mention_only` to SlackConfig**

In `src/config/schema.rs`, add to `SlackConfig`:

```rust
pub struct SlackConfig {
    pub bot_token: String,
    pub app_token: Option<String>,
    pub channel_id: Option<String>,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Only respond when explicitly mentioned or in bot's own threads.
    /// Default: true. When false, responds to all messages from allowed users.
    #[serde(default = "default_true")]
    pub mention_only: bool,
    /// Optional regex pattern for mention detection (in addition to @bot).
    pub mention_regex: Option<String>,
}

fn default_true() -> bool {
    true
}
```

**Step 4: Implement mention detection**

In `slack.rs`:

```rust
/// Check if message text contains a mention of the bot.
fn is_mention(text: &str, bot_user_id: &str, mention_regex: Option<&regex::Regex>) -> bool {
    // Explicit Slack mention: <@U_BOT_ID>
    if text.contains(&format!("<@{bot_user_id}>")) {
        return true;
    }
    // Configurable regex (e.g. bot name)
    if let Some(re) = mention_regex {
        if re.is_match(text) {
            return true;
        }
    }
    false
}

/// Check if message is an implicit mention (reply in bot's thread).
fn is_implicit_mention(bot_user_id: &str, parent_user_id: Option<&str>) -> bool {
    parent_user_id == Some(bot_user_id)
}
```

**Step 5: Wire mention gating into the listen loop**

In the message processing section of `listen()`, after user allowlist check:

```rust
if mention_only {
    let parent_uid = msg.get("parent_user_id").and_then(|v| v.as_str());
    let was_mentioned = is_mention(text, &bot_user_id, mention_regex.as_ref())
        || is_implicit_mention(&bot_user_id, parent_uid);

    if !was_mentioned {
        // Buffer for pending history (Task 8), skip processing
        continue;
    }

    // Strip @mention from text before sending to LLM
    let text = text.replace(&format!("<@{bot_user_id}>"), "").trim().to_string();
}
```

**Step 6: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 7: Commit**

```bash
git add src/channels/slack.rs src/config/schema.rs
git commit -m "feat(slack): add mention gating — respond only when @mentioned

Add mention_only config (default: true) and mention_regex to SlackConfig.
Detect explicit <@bot_id> mentions, configurable regex patterns, and
implicit mentions (replies in bot's own threads). Non-mention messages
are skipped (pending history buffer wired in next commit). Strip @mention
from text before LLM processing. OpenClaw has the same three detection
paths."
```

---

### Task 8: Pending history buffer and envelope format

**Files:**
- Modify: `src/channels/slack.rs`
- Modify: `src/channels/mod.rs`

Buffer non-mention messages and prepend them when the bot IS mentioned. Format all messages in structured envelopes.

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod pending_history_tests {
    use super::*;

    #[test]
    fn ring_buffer_evicts_oldest_when_full() {
        let mut buffer = PendingHistoryBuffer::new(3);
        buffer.push("msg1".into());
        buffer.push("msg2".into());
        buffer.push("msg3".into());
        buffer.push("msg4".into());
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.entries()[0], "msg2");
    }

    #[test]
    fn drain_returns_all_and_clears() {
        let mut buffer = PendingHistoryBuffer::new(50);
        buffer.push("msg1".into());
        buffer.push("msg2".into());
        let drained = buffer.drain();
        assert_eq!(drained.len(), 2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn format_envelope_includes_channel_sender_timestamp() {
        let envelope = format_message_envelope(
            "#general",
            "Alice",
            "2026-02-24 14:30:05",
            "hello world",
        );
        assert!(envelope.contains("[Slack #general Alice"));
        assert!(envelope.contains("2026-02-24 14:30:05"));
        assert!(envelope.contains("Alice: hello world"));
    }
}
```

**Step 2: Run tests to verify they fail**

**Step 3: Implement PendingHistoryBuffer**

```rust
/// Per-channel ring buffer for non-mention messages.
struct PendingHistoryBuffer {
    entries: Vec<String>,
    max: usize,
}

impl PendingHistoryBuffer {
    fn new(max: usize) -> Self {
        Self { entries: Vec::with_capacity(max), max }
    }

    fn push(&mut self, entry: String) {
        if self.entries.len() >= self.max {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    fn drain(&mut self) -> Vec<String> {
        std::mem::take(&mut self.entries)
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn entries(&self) -> &[String] {
        &self.entries
    }
}
```

**Step 4: Implement envelope format**

```rust
/// Format a message in the structured envelope format.
fn format_message_envelope(
    channel_name: &str,
    sender_name: &str,
    timestamp: &str,
    text: &str,
) -> String {
    format!("[Slack {channel_name} {sender_name} {timestamp}] {sender_name}: {text}")
}
```

**Step 5: Wire into the listen loop**

In `slack.rs`, maintain a `HashMap<String, PendingHistoryBuffer>` keyed by channel ID:

```rust
// In listen(), alongside last_ts_by_channel:
let mut pending_history: HashMap<String, PendingHistoryBuffer> = HashMap::new();

// When mention_only is true and message is NOT a mention:
let buffer = pending_history
    .entry(channel_id.clone())
    .or_insert_with(|| PendingHistoryBuffer::new(50));
let envelope = format_message_envelope(&channel_name, &sender_name, &timestamp_str, text);
buffer.push(envelope);
continue;

// When message IS a mention, prepend pending history:
let pending = pending_history
    .get_mut(&channel_id)
    .map(|b| b.drain())
    .unwrap_or_default();

if !pending.is_empty() {
    let context = format!(
        "[Chat messages since your last reply - for context]\n{}\n\n[Current message - respond to this]\n{}",
        pending.join("\n"),
        format_message_envelope(&channel_name, &sender_name, &timestamp_str, text)
    );
    // Use `context` as message content instead of raw `text`
}
```

**Step 6: Run full validation**

Run: `cargo clippy --all-targets -- -D warnings && cargo test`
Expected: Clean

**Step 7: Commit**

```bash
git add src/channels/slack.rs
git commit -m "feat(slack): add pending history buffer and message envelope format

Maintain per-channel ring buffer (max 50) of non-mention messages.
When bot IS mentioned, prepend buffered messages as context with
[Chat messages since your last reply] header. Format all messages
in structured envelopes: [Slack #channel Sender timestamp] Sender: text.
Clear buffer after responding. OpenClaw uses the same ring buffer
pattern with identical max size."
```

---

## Phase 3: Linear as the Brain (No Rust Changes)

### Task 9: Linear workspace configuration

**Files:**
- Create: workspace config files (outside `src/`)

This task is config-only. It sets up the workspace rules and skills that make the agent treat Linear as its primary brain. No Rust compilation needed.

**Step 1: Define the Linear context skill**

Create `workspace/skills/linear-context/SKILL.md` (or whatever the workspace skill path is for your deployment):

```markdown
# Linear Context

Before every response that touches work state, query current Linear issues.

## Tool: check_linear

Query the Linear GraphQL API for:
- Active cycle issues (status, assignee, priority)
- Recently updated issues (last 24h)
- Issues matching keywords from the current message

## Rules

- Never create, update, or close a Linear issue without first checking current state.
- Before responding to any message about work, check if relevant issues exist.
- A real PM always has their project board open. You must do the same.
```

**Step 2: Add to AGENTS.md rules**

Add the Linear-as-brain rule and confidence-based autonomy:

```markdown
## Linear is your brain

Before creating, updating, or commenting on any issue, query Linear for
current state. Before responding to any message about work, check if
relevant issues exist. A real PM always has their project board open.
You must do the same — every time, not just during rituals.

### Confidence-Based Autonomy

- **High confidence** (exact issue ID mentioned, status update with clear mapping):
  Update Linear silently, log to #rain-log.
- **Low confidence** (vague reference, ambiguous commitment, could map to multiple issues):
  Confirm with the person in-thread before acting.
```

**Step 3: Commit**

```bash
git add workspace/
git commit -m "feat(linear): add Linear-as-brain workspace config

Config-only change. Add linear-context skill that queries Linear GraphQL
API before every work-state response. Add confidence-based autonomy
rules to AGENTS.md. No Rust changes — if prompt-level instructions prove
unreliable, the escalation path is to wire the existing before_llm_call
hook (~10 lines in agent/loop_.rs)."
```

---

## Open Questions (from design doc)

These are explicitly deferred — implement the baseline first, then iterate:

1. **Token-aware compression for pending history** — Start with raw messages. Add summarization later if token budgets are tight.
2. **Configurable thread hydration depth** — Start with hardcoded `limit=20`. Make configurable if needed.
3. **Sanitizer placement (gemini.rs vs reliable.rs)** — Start in `gemini.rs` only. If other Google-backed providers need it (vertex, etc.), move to a shared layer.

---

## Validation Checklist

Before opening the PR:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Verify:
- [ ] Gemini sanitizer strips all 22 unsupported keywords
- [ ] Gemini sanitizer resolves $ref with cycle detection
- [ ] Gemini sanitizer flattens literal unions
- [ ] Transcript sanitizer fixes turn ordering
- [ ] Transcript sanitizer rewrites tool call IDs
- [ ] Slack thread hydration fetches conversations.replies
- [ ] Slack mention gating filters non-mention messages
- [ ] Slack pending history prepends context on mention
- [ ] Session scoping is per-thread (not per-sender)
- [ ] All existing tests still pass
