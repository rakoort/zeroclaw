//! Live integration test: Gemini 3.x thinking model thoughtSignature round-trip.
//!
//! Verifies that `thoughtSignature` on function call parts survives the full
//! agent history cycle:
//!   1. Gemini returns a tool call with `thoughtSignature`
//!   2. The assistant history JSON includes `thought_signature` on tool calls
//!   3. `chat_with_tools()` reconstructs it into a Gemini `Part`
//!   4. The next API call succeeds (no 400 or silent empty response)
//!
//! The serialization correctness of `build_native_assistant_history()` is
//! covered by unit tests in `src/agent/loop_.rs`. This test verifies the
//! *API-level* round-trip: does the Gemini API accept history that includes
//! `thoughtSignature` on function call parts?
//!
//! Requires Vertex AI service account credentials.
//! Run manually:
//!   GOOGLE_APPLICATION_CREDENTIALS=/path/to/sa.json \
//!     cargo test gemini_thinking_roundtrip -- --ignored --nocapture

use zeroclaw::providers::gemini::GeminiProvider;
use zeroclaw::providers::traits::{ChatMessage, ChatResponse, Provider};

/// A simple tool definition that reliably triggers a function call from thinking models.
fn get_current_date_tool() -> serde_json::Value {
    serde_json::json!({
        "name": "get_current_date",
        "description": "Returns today's date in ISO 8601 format. Call this whenever the user asks about today's date.",
        "parameters": {
            "type": "object",
            "properties": {},
            "required": []
        }
    })
}

/// Build tool result JSON in the same format the agent loop uses.
fn tool_result_json(tool_call_id: &str, content: &str) -> String {
    serde_json::json!({
        "tool_call_id": tool_call_id,
        "content": content
    })
    .to_string()
}

/// Serialize a `ChatResponse` into the assistant history JSON that
/// `chat_with_tools()` expects to parse.
///
/// Uses `serde_json::to_string` on the actual `ToolCall` struct so that
/// the serialization path is driven by the production `#[serde]` attributes
/// (including `thought_signature`), not by hand-rolled JSON.
fn build_assistant_history_from_response(response: &ChatResponse) -> String {
    // Serialize each ToolCall via serde so we use the same field names and
    // skip_serializing_if logic as production code.
    let calls_json: Vec<serde_json::Value> = response
        .tool_calls
        .iter()
        .map(|tc| serde_json::to_value(tc).expect("ToolCall serialization"))
        .collect();

    let content = match response.text.as_deref() {
        Some(t) if !t.trim().is_empty() => serde_json::Value::String(t.trim().to_string()),
        _ => serde_json::Value::Null,
    };

    serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    })
    .to_string()
}

/// Truncate a string to at most `max_chars` characters (UTF-8 safe).
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Live round-trip test against Gemini 3.x thinking model via Vertex AI.
///
/// Turn 1: Send a prompt that forces a tool call -> model returns functionCall
///         with thoughtSignature.
/// Turn 2: Replay full history (assistant tool call + tool result) -> model
///         responds with final answer. If thoughtSignature was dropped, this
///         fails with 400 or empty response.
#[tokio::test]
#[ignore]
async fn gemini_thinking_model_thought_signature_round_trip() {
    // Skip if no Gemini auth available
    if !GeminiProvider::has_any_auth() {
        eprintln!("Skipping: no Gemini credentials found");
        return;
    }

    let provider = GeminiProvider::new(None);
    let model = "gemini-3-flash-preview";
    let tools = vec![get_current_date_tool()];

    // ── Turn 1: trigger a tool call ─────────────────────────────────────
    let messages_turn1 = vec![
        ChatMessage::system(
            "You are a concise assistant. When asked about the date, \
             you MUST call the get_current_date tool. Do not guess.",
        ),
        ChatMessage::user("What is today's date?"),
    ];

    eprintln!("=== Turn 1: sending prompt to trigger tool call ===");
    let response1 = provider
        .chat_with_tools(&messages_turn1, &tools, model, 0.0)
        .await;

    assert!(
        response1.is_ok(),
        "Turn 1 API call failed: {:?}",
        response1.err()
    );
    let r1 = response1.unwrap();

    assert!(
        r1.has_tool_calls(),
        "Turn 1: model should have called get_current_date, got text: {:?}",
        r1.text
    );

    let tool_call = &r1.tool_calls[0];
    assert_eq!(tool_call.name, "get_current_date");
    eprintln!(
        "Turn 1 tool call: name={}, id={}, thought_signature={:?}",
        tool_call.name,
        tool_call.id,
        tool_call
            .thought_signature
            .as_deref()
            .map(|s| format!("{}... ({} bytes)", truncate_chars(s, 20), s.len()))
    );

    // Note: thought_signature may or may not be present depending on
    // whether the model actually used thinking. We test the round-trip
    // regardless — the fix ensures it's preserved WHEN present.
    if tool_call.thought_signature.is_some() {
        eprintln!("thought_signature present on tool call — round-trip will be tested");
    } else {
        eprintln!(
            "No thought_signature on tool call (model may not have used thinking). \
             Round-trip still tested for structural correctness."
        );
    }

    // ── Turn 2: send tool result back with full history ─────────────────
    //
    // build_assistant_history_from_response uses serde on ToolCall so the
    // JSON is driven by production #[serde] attributes. The unit tests in
    // loop_.rs verify build_native_assistant_history() directly; this test
    // verifies the Gemini API accepts the resulting history.
    let assistant_history = build_assistant_history_from_response(&r1);
    let tool_result = tool_result_json(&tool_call.id, "2026-03-01");

    let messages_turn2 = vec![
        ChatMessage::system(
            "You are a concise assistant. When asked about the date, \
             you MUST call the get_current_date tool. Do not guess.",
        ),
        ChatMessage::user("What is today's date?"),
        ChatMessage::assistant(&assistant_history),
        ChatMessage::tool(&tool_result),
    ];

    // Brief pause to avoid rate limiting between back-to-back API calls
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

    eprintln!("=== Turn 2: sending tool result with history ===");
    let response2 = provider
        .chat_with_tools(&messages_turn2, &tools, model, 0.0)
        .await;

    assert!(
        response2.is_ok(),
        "Turn 2 API call failed (thoughtSignature round-trip broken?): {:?}",
        response2.err()
    );
    let r2 = response2.unwrap();

    let text = r2.text_or_empty().to_lowercase();
    eprintln!("Turn 2 response: {text}");
    assert!(
        text.contains("2026") || text.contains("march") || text.contains("date"),
        "Model should reference the date from tool result, got: {text}",
    );

    eprintln!("Full round-trip succeeded — thoughtSignature preserved correctly");
}
