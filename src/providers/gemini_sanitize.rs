//! Recursive JSON Schema sanitizer for the Gemini provider.
//!
//! Gemini rejects 22 JSON Schema keywords it does not support. This module
//! provides a pure-function cleaner that strips unsupported keywords, converts
//! `const` to single-variant `enum`, and normalizes `["type", "null"]` arrays
//! to a single type string.  It is called before Gemini API requests so that
//! tool schemas pass validation.

use serde_json::{json, Map, Value};

/// JSON Schema keywords that Gemini does not support.
///
/// Any key in this set is silently removed during sanitization.
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

/// Sanitize a JSON Schema value for Gemini compatibility.
///
/// This is a pure function — it does not mutate the input and returns a new
/// [`Value`] with unsupported keywords stripped, `const` converted to `enum`,
/// and type arrays normalized to single strings.
pub fn sanitize_schema_for_gemini(schema: &Value) -> Value {
    clean_schema_recursive(schema)
}

/// Recursively clean a JSON Schema node.
///
/// For each object key:
/// - Skip keys in [`UNSUPPORTED_KEYWORDS`]
/// - Convert `const` to a single-variant `enum`
/// - Normalize `type` arrays (strip `"null"`, take first remaining)
/// - Recurse into `properties` values, `items`, `anyOf`, and `oneOf`
/// - Pass through all other keys unchanged
fn clean_schema_recursive(schema: &Value) -> Value {
    let obj = match schema.as_object() {
        Some(obj) => obj,
        None => return schema.clone(),
    };

    let mut out = Map::new();

    for (key, value) in obj {
        if UNSUPPORTED_KEYWORDS.contains(&key.as_str()) {
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
                    .map(|(k, v)| (k.clone(), clean_schema_recursive(v)))
                    .collect();
                out.insert(key.clone(), Value::Object(cleaned));
            } else {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        if key == "items" {
            out.insert(key.clone(), clean_schema_recursive(value));
            continue;
        }

        if key == "anyOf" || key == "oneOf" {
            if let Some(variants) = value.as_array() {
                let cleaned: Vec<Value> = variants.iter().map(clean_schema_recursive).collect();
                out.insert(key.clone(), Value::Array(cleaned));
            } else {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        out.insert(key.clone(), value.clone());
    }

    Value::Object(out)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
}
