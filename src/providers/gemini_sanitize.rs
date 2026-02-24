//! Recursive JSON Schema sanitizer for the Gemini provider.
//!
//! Gemini rejects 20 JSON Schema keywords it does not support. This module
//! provides a pure-function cleaner that strips unsupported keywords, converts
//! `const` to single-variant `enum`, normalizes `["type", "null"]` arrays to a
//! single type string, resolves `$ref` pointers inline (with cycle detection),
//! and flattens `anyOf`/`oneOf` literal unions into `enum` arrays.  It is called
//! before Gemini API requests so that tool schemas pass validation.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::providers::traits::ChatMessage;

/// JSON Schema keywords that Gemini does not support.
///
/// Any key in this set is silently removed during sanitization.
/// Note: `$ref`, `$defs`, and `definitions` are handled specially (resolved
/// inline) and therefore not in this list.
const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "patternProperties",
    "additionalProperties",
    "$schema",
    "$id",
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

/// Sanitize a JSON Schema value for Gemini compatibility.
///
/// This is a pure function — it does not mutate the input and returns a new
/// [`Value`] with unsupported keywords stripped, `$ref` pointers resolved
/// inline, `const` converted to `enum`, `anyOf`/`oneOf` literal unions
/// flattened, and type arrays normalized to single strings.
pub fn sanitize_schema_for_gemini(schema: &Value) -> Value {
    // Extract $defs / definitions from the top-level schema into a shared map.
    let mut defs = Map::new();
    if let Some(obj) = schema.as_object() {
        if let Some(Value::Object(d)) = obj.get("$defs") {
            defs.extend(d.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        if let Some(Value::Object(d)) = obj.get("definitions") {
            defs.extend(d.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }

    let mut ref_stack = HashSet::new();
    clean_schema_recursive(schema, &defs, &mut ref_stack)
}

/// Return `true` if this schema node represents the JSON Schema `null` type.
///
/// Matches: `{"type": "null"}`, `{"const": null}`, or `{"enum": [null]}`.
fn is_null_schema(schema: &Value) -> bool {
    if let Some(obj) = schema.as_object() {
        if obj.get("type").and_then(Value::as_str) == Some("null") {
            return true;
        }
        if let Some(c) = obj.get("const") {
            if c.is_null() {
                return true;
            }
        }
        if let Some(Value::Array(arr)) = obj.get("enum") {
            if arr.len() == 1 && arr[0].is_null() {
                return true;
            }
        }
    }
    false
}

/// Infer the JSON Schema type string for a `const` value.
fn type_of_value(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        _ => "string",
    }
}

/// Try to flatten an array of schema variants into a single `{type, enum}`.
///
/// Succeeds when every variant is either `{const: X}` or `{type: T, enum: [...]}`,
/// and all inferred types agree.  Returns `None` if flattening is not possible.
fn try_flatten_literal_union(
    variants: &[Value],
    defs: &Map<String, Value>,
    ref_stack: &mut HashSet<String>,
) -> Option<Value> {
    if variants.is_empty() {
        return None;
    }

    let mut all_values: Vec<Value> = Vec::new();
    let mut common_type: Option<String> = None;

    for variant in variants {
        let cleaned = clean_schema_recursive(variant, defs, ref_stack);
        let obj = cleaned.as_object()?;

        if let Some(c) = obj.get("enum").and_then(Value::as_array) {
            // {enum: [...]} or {type: T, enum: [...]}
            let t = obj
                .get("type")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| c.first().map(|v| type_of_value(v).to_string()))?;
            match &common_type {
                Some(ct) if ct != &t => return None,
                _ => common_type = Some(t),
            }
            all_values.extend(c.iter().cloned());
        } else if let Some(c) = obj.get("const") {
            // {const: X} — already converted to {enum: [X]} by clean_schema_recursive,
            // but the original variant may not have been cleaned yet.
            // Handle both pre- and post-clean forms.
            let t = type_of_value(c).to_string();
            match &common_type {
                Some(ct) if ct != &t => return None,
                _ => common_type = Some(t),
            }
            all_values.push(c.clone());
        } else {
            return None;
        }
    }

    let t = common_type?;
    Some(json!({ "type": t, "enum": all_values }))
}

/// Recursively clean a JSON Schema node.
///
/// For each object key:
/// - Resolve `$ref` pointers inline (with cycle detection)
/// - Skip keys in [`UNSUPPORTED_KEYWORDS`] and `$defs`/`definitions`
/// - Convert `const` to a single-variant `enum`
/// - Normalize `type` arrays (strip `"null"`, take first remaining)
/// - Flatten `anyOf`/`oneOf` literal unions into `enum`
/// - Strip null variants from `anyOf`/`oneOf` and unwrap single-variant
/// - Recurse into `properties` values, `items`, `anyOf`, and `oneOf`
/// - Pass through all other keys unchanged
fn clean_schema_recursive(
    schema: &Value,
    defs: &Map<String, Value>,
    ref_stack: &mut HashSet<String>,
) -> Value {
    let obj = match schema.as_object() {
        Some(obj) => obj,
        None => return schema.clone(),
    };

    // ── $ref resolution ─────────────────────────────────────────────────
    if let Some(Value::String(ref_path)) = obj.get("$ref") {
        // Parse paths like "#/$defs/Item" or "#/definitions/Status"
        let segments: Vec<&str> = ref_path.split('/').collect();
        if segments.len() == 3
            && segments[0] == "#"
            && (segments[1] == "$defs" || segments[1] == "definitions")
        {
            let def_name = segments[2];
            if let Some(def_schema) = defs.get(def_name) {
                if ref_stack.contains(def_name) {
                    // Circular reference — return empty object to break the cycle
                    return json!({});
                }
                ref_stack.insert(def_name.to_string());
                let result = clean_schema_recursive(def_schema, defs, ref_stack);
                ref_stack.remove(def_name);
                return result;
            }
        }
        // Unresolvable $ref — fall through; it will be stripped as unsupported
    }

    let mut out = Map::new();
    // Track whether we emitted an anyOf/oneOf that survived flattening
    let mut has_surviving_union = false;

    for (key, value) in obj {
        if UNSUPPORTED_KEYWORDS.contains(&key.as_str()) {
            continue;
        }

        // Skip $ref (handled above), $defs, and definitions
        if key == "$ref" || key == "$defs" || key == "definitions" {
            continue;
        }

        if key == "const" {
            out.insert("enum".to_string(), json!([value]));
            continue;
        }

        if key == "type" {
            if let Some(arr) = value.as_array() {
                let non_null: Vec<&Value> =
                    arr.iter().filter(|v| v.as_str() != Some("null")).collect();
                let single = non_null
                    .first()
                    .map(|v| (*v).clone())
                    .unwrap_or_else(|| Value::String("string".to_string()));
                out.insert("type".to_string(), single);
            } else {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        if key == "properties" {
            if let Some(props) = value.as_object() {
                let cleaned: Map<String, Value> = props
                    .iter()
                    .map(|(k, v)| (k.clone(), clean_schema_recursive(v, defs, ref_stack)))
                    .collect();
                out.insert(key.clone(), Value::Object(cleaned));
            } else {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        if key == "items" {
            out.insert(key.clone(), clean_schema_recursive(value, defs, ref_stack));
            continue;
        }

        if key == "anyOf" || key == "oneOf" {
            if let Some(variants) = value.as_array() {
                // Step 1: Strip null variants
                let non_null: Vec<&Value> =
                    variants.iter().filter(|v| !is_null_schema(v)).collect();

                // Step 2: If only one variant remains, unwrap it
                if non_null.len() == 1 {
                    let unwrapped = clean_schema_recursive(non_null[0], defs, ref_stack);
                    if let Some(unwrapped_obj) = unwrapped.as_object() {
                        for (uk, uv) in unwrapped_obj {
                            out.insert(uk.clone(), uv.clone());
                        }
                    }
                    continue;
                }

                // Step 3: Try to flatten literal unions
                if let Some(flattened) = try_flatten_literal_union(
                    &non_null.iter().map(|v| (*v).clone()).collect::<Vec<_>>(),
                    defs,
                    ref_stack,
                ) {
                    if let Some(flat_obj) = flattened.as_object() {
                        for (fk, fv) in flat_obj {
                            out.insert(fk.clone(), fv.clone());
                        }
                    }
                    continue;
                }

                // Step 4: Union survives — clean variants and keep
                let cleaned: Vec<Value> = non_null
                    .iter()
                    .map(|v| clean_schema_recursive(v, defs, ref_stack))
                    .collect();
                out.insert(key.clone(), Value::Array(cleaned));
                has_surviving_union = true;
            } else {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        out.insert(key.clone(), value.clone());
    }

    // Step 5: Gemini rejects `type` + `anyOf`/`oneOf` together — strip type
    if has_surviving_union {
        out.remove("type");
    }

    Value::Object(out)
}

/// Sanitize a conversation transcript for Gemini compatibility.
///
/// Applies four transforms in order:
/// 1. Rewrite non-alphanumeric tool call IDs to alphanumeric-only equivalents.
/// 2. Preserve system messages at the front unchanged.
/// 3. Prepend a synthetic user turn if non-system messages start with assistant.
/// 4. Merge consecutive same-role messages (content joined with `\n`).
///
/// This is a pure function — no side effects, returns a new `Vec`.
pub fn sanitize_transcript_for_gemini(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return Vec::new();
    }

    // Step 1: Rewrite non-alphanumeric tool call IDs.
    let id_pattern = Regex::new(r#"id="([^"]+)""#).expect("valid regex");
    let mut id_map: HashMap<String, String> = HashMap::new();
    let rewritten: Vec<ChatMessage> = messages
        .iter()
        .map(|msg| {
            let new_content = id_pattern
                .replace_all(&msg.content, |caps: &regex::Captures| {
                    let original_id = &caps[1];
                    if original_id.chars().all(|c| c.is_alphanumeric()) {
                        return format!(r#"id="{}""#, original_id);
                    }
                    let rewritten_id = id_map
                        .entry(original_id.to_string())
                        .or_insert_with(|| {
                            original_id
                                .chars()
                                .filter(|c| c.is_alphanumeric())
                                .collect()
                        })
                        .clone();
                    format!(r#"id="{}""#, rewritten_id)
                })
                .into_owned();
            ChatMessage {
                role: msg.role.clone(),
                content: new_content,
            }
        })
        .collect();

    // Step 2: Separate system messages from non-system messages.
    let mut system_msgs: Vec<ChatMessage> = Vec::new();
    let mut non_system_msgs: Vec<ChatMessage> = Vec::new();
    for msg in rewritten {
        if msg.role == "system" {
            system_msgs.push(msg);
        } else {
            non_system_msgs.push(msg);
        }
    }

    // Step 3: Prepend synthetic user turn if non-system messages start with assistant.
    if non_system_msgs
        .first()
        .is_some_and(|m| m.role == "assistant")
    {
        non_system_msgs.insert(0, ChatMessage::user("(session bootstrap)"));
    }

    // Step 4: Merge consecutive same-role messages.
    let mut merged: Vec<ChatMessage> = Vec::new();
    for msg in non_system_msgs {
        if let Some(last) = merged.last_mut() {
            if last.role == msg.role {
                last.content.push('\n');
                last.content.push_str(&msg.content);
                continue;
            }
        }
        merged.push(msg);
    }

    // Reassemble: system messages at the front, then merged non-system.
    let mut result = system_msgs;
    result.extend(merged);
    result
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::ChatMessage;
    use serde_json::json;

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
        assert_eq!(result.get("type").unwrap(), "object");
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

        assert_eq!(email.get("type").unwrap(), "string");
        assert!(email.get("format").is_none());
        assert!(email.get("minLength").is_none());
        assert!(email.get("maxLength").is_none());
    }

    #[test]
    fn strips_unsupported_keywords_in_array_items() {
        let input = json!({
            "type": "array",
            "minItems": 1,
            "maxItems": 10,
            "uniqueItems": true,
            "items": {
                "type": "string",
                "minLength": 3,
                "pattern": "^[a-z]+$"
            }
        });

        let result = sanitize_schema_for_gemini(&input);

        assert_eq!(result.get("type").unwrap(), "array");
        assert!(result.get("minItems").is_none());
        assert!(result.get("maxItems").is_none());
        assert!(result.get("uniqueItems").is_none());

        let items = &result["items"];
        assert_eq!(items.get("type").unwrap(), "string");
        assert!(items.get("minLength").is_none());
        assert!(items.get("pattern").is_none());
    }

    #[test]
    fn converts_const_to_single_enum() {
        let input = json!({ "const": "delete" });
        let result = sanitize_schema_for_gemini(&input);

        assert!(result.get("const").is_none());
        assert_eq!(result.get("enum").unwrap(), &json!(["delete"]));
    }

    #[test]
    fn normalizes_type_array_to_single_type() {
        let input = json!({
            "type": ["string", "null"],
            "description": "optional name"
        });

        let result = sanitize_schema_for_gemini(&input);

        assert_eq!(result.get("type").unwrap(), "string");
        assert_eq!(result.get("description").unwrap(), "optional name");
    }

    #[test]
    fn normalizes_type_array_strips_null() {
        let input = json!({
            "type": ["integer", "null"]
        });

        let result = sanitize_schema_for_gemini(&input);

        assert_eq!(result.get("type").unwrap(), "integer");
    }

    #[test]
    fn passes_through_simple_valid_schema() {
        let input = json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer" },
                "name": { "type": "string" }
            },
            "required": ["id", "name"],
            "description": "A user record"
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

    // ── $ref resolution tests ───────────────────────────────────────────

    #[test]
    fn resolves_ref_from_defs_inline() {
        let input = json!({
            "type": "object",
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            },
            "properties": {
                "item": { "$ref": "#/$defs/Item" }
            }
        });

        let result = sanitize_schema_for_gemini(&input);

        // $defs should be stripped
        assert!(result.get("$defs").is_none());

        // The property should have the inlined definition
        let item = &result["properties"]["item"];
        assert_eq!(item["type"], "object");
        assert!(item["properties"]["name"].is_object());
    }

    #[test]
    fn resolves_ref_from_definitions() {
        let input = json!({
            "type": "object",
            "definitions": {
                "Status": {
                    "type": "string",
                    "enum": ["active", "inactive"]
                }
            },
            "properties": {
                "status": { "$ref": "#/definitions/Status" }
            }
        });

        let result = sanitize_schema_for_gemini(&input);

        assert!(result.get("definitions").is_none());

        let status = &result["properties"]["status"];
        assert_eq!(status["type"], "string");
        assert_eq!(status["enum"], json!(["active", "inactive"]));
    }

    #[test]
    fn circular_ref_replaced_with_empty_object() {
        let input = json!({
            "type": "object",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": { "$ref": "#/$defs/Node" }
                    }
                }
            },
            "properties": {
                "root": { "$ref": "#/$defs/Node" }
            }
        });

        let result = sanitize_schema_for_gemini(&input);

        // root should be resolved
        let root = &result["properties"]["root"];
        assert_eq!(root["type"], "object");

        // child inside root should be empty object (circular)
        let child = &root["properties"]["child"];
        assert_eq!(child, &json!({}));
    }

    // ── anyOf/oneOf flattening tests ────────────────────────────────────

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
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "type": "object", "properties": { "b": { "type": "integer" } } }
            ]
        });

        let result = sanitize_schema_for_gemini(&input);

        // type should be stripped when anyOf survives
        assert!(result.get("type").is_none());
        assert!(result.get("anyOf").is_some());
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
                        { "type": "string", "enum": ["low", "high"] },
                        { "type": "null" }
                    ]
                }
            }
        });

        let result = sanitize_schema_for_gemini(&input);

        let priority = &result["properties"]["priority"];
        assert!(priority.get("anyOf").is_none());
        assert_eq!(priority["type"], "string");
        assert_eq!(priority["enum"], json!(["low", "high"]));
    }

    // ── Transcript sanitizer tests ─────────────────────────────────────

    #[test]
    fn handles_empty_transcript() {
        let result = sanitize_transcript_for_gemini(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn no_change_for_clean_transcript() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "Hello");
        assert_eq!(result[1].role, "assistant");
        assert_eq!(result[1].content, "Hi there");
    }

    #[test]
    fn preserves_system_messages_at_start() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content, "You are helpful");
        assert_eq!(result[1].role, "user");
        assert_eq!(result[2].role, "assistant");
    }

    #[test]
    fn prepends_user_turn_if_starts_with_assistant() {
        let messages = vec![ChatMessage::assistant("I started"), ChatMessage::user("OK")];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "(session bootstrap)");
        assert_eq!(result[1].role, "assistant");
        assert_eq!(result[1].content, "I started");
        assert_eq!(result[2].role, "user");
        assert_eq!(result[2].content, "OK");
    }

    #[test]
    fn merges_consecutive_same_role_messages() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::user("World"),
            ChatMessage::assistant("Hi"),
            ChatMessage::assistant("There"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "Hello\nWorld");
        assert_eq!(result[1].role, "assistant");
        assert_eq!(result[1].content, "Hi\nThere");
    }

    #[test]
    fn rewrites_non_alphanumeric_tool_call_ids() {
        let messages = vec![
            ChatMessage::user(r#"Tool call id="call_abc-123_def" executed"#),
            ChatMessage::assistant("OK"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        // The ID should have non-alphanumeric chars stripped (digits are alphanumeric)
        assert!(!result[0].content.contains("call_abc-123_def"));
        assert!(result[0].content.contains("callabc123def"));
    }

    #[test]
    fn tool_call_id_rewrite_is_consistent() {
        let messages = vec![
            ChatMessage::user(r#"id="call_abc-123_def" called"#),
            ChatMessage::assistant(r#"result for id="call_abc-123_def" done"#),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        // Extract the rewritten ID from both messages — they should match
        let re = Regex::new(r#"id="([^"]+)""#).unwrap();
        let id1 = re
            .captures(&result[0].content)
            .unwrap()
            .get(1)
            .unwrap()
            .as_str();
        let id2 = re
            .captures(&result[1].content)
            .unwrap()
            .get(1)
            .unwrap()
            .as_str();
        assert_eq!(id1, id2);
        // And should be alphanumeric only
        assert!(id1.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn merges_and_prepends_combined() {
        let messages = vec![
            ChatMessage::assistant("Part 1"),
            ChatMessage::assistant("Part 2"),
            ChatMessage::user("Go"),
        ];
        let result = sanitize_transcript_for_gemini(&messages);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "(session bootstrap)");
        assert_eq!(result[1].role, "assistant");
        assert_eq!(result[1].content, "Part 1\nPart 2");
        assert_eq!(result[2].role, "user");
        assert_eq!(result[2].content, "Go");
    }
}
