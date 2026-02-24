//! Integration test: verify Gemini sanitization functions are accessible
//! from the crate's public API and work end-to-end.
//!
//! The production code and 26 unit tests already exist (committed in prior tasks).
//! These integration tests verify the public re-export path works from outside
//! the crate — no new production code is being added in this cycle.

use zeroclaw::providers::gemini_sanitize::{
    sanitize_schema_for_gemini, sanitize_transcript_for_gemini,
};
use zeroclaw::providers::traits::ChatMessage;

#[test]
fn schema_sanitizer_strips_unsupported_keywords() {
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
fn transcript_sanitizer_fixes_turn_ordering() {
    let messages = vec![ChatMessage::assistant("oops"), ChatMessage::user("hello")];
    let result = sanitize_transcript_for_gemini(&messages);
    assert_eq!(result[0].role, "user");
    assert_eq!(result[0].content, "(session bootstrap)");
    assert_eq!(result[1].role, "assistant");
    assert_eq!(result[2].role, "user");
}

#[test]
fn transcript_sanitizer_merges_consecutive_roles() {
    let messages = vec![
        ChatMessage::user("part 1"),
        ChatMessage::user("part 2"),
        ChatMessage::assistant("response"),
    ];
    let result = sanitize_transcript_for_gemini(&messages);
    assert_eq!(result.len(), 2);
    assert!(result[0].content.contains("part 1"));
    assert!(result[0].content.contains("part 2"));
}
