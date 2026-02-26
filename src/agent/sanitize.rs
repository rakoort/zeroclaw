//! Response sanitization for model-internal artifacts.
//!
//! Strips reasoning tags (`<think>`, `<thinking>`, `<thought>`, `<antthinking>`),
//! tool execution tags (`<tool_code>`, `<tool_result>`), and handles unclosed tags.
//! Preserves tags inside markdown code fences and inline code spans.
//!
//! Lives in the agent layer so ALL channels benefit from sanitization.

use regex::Regex;

/// Tags that should be stripped from model responses.
const STRIPPED_TAGS: &[&str] = &[
    "think",
    "thinking",
    "thought",
    "antthinking",
    "tool_code",
    "tool_result",
];

/// Build ranges of byte positions that fall inside markdown code spans.
///
/// Detects both fenced code blocks (`` ``` ... ``` ``) and inline code (`` ` ... ` ``).
/// Returns a sorted, non-overlapping list of `(start, end)` byte ranges.
fn code_span_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();

    // Fenced code blocks: ```...```
    let fenced = Regex::new(r"(?s)```.*?```").expect("valid fenced code regex");
    for m in fenced.find_iter(text) {
        ranges.push((m.start(), m.end()));
    }

    // Inline code: `...` (but not inside already-found fenced blocks)
    let inline = Regex::new(r"`[^`]+`").expect("valid inline code regex");
    for m in inline.find_iter(text) {
        let start = m.start();
        if !ranges.iter().any(|&(rs, re)| start >= rs && start < re) {
            ranges.push((m.start(), m.end()));
        }
    }

    ranges.sort_by_key(|&(s, _)| s);
    ranges
}

/// Check if a byte position falls inside any code span.
fn in_code_span(pos: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(start, end)| pos >= start && pos < end)
}

/// Strip a specific tag pattern (opening + content + closing), respecting code spans.
///
/// Handles both closed tags (`<tag>...</tag>`) and unclosed tags (`<tag>...` to end).
/// Tag matching is case-insensitive and tolerates whitespace inside angle brackets.
fn strip_tag_blocks(text: &str, tag_name: &str) -> String {
    let ranges = code_span_ranges(text);

    // First pass: strip closed tag pairs
    let closed_pattern = Regex::new(&format!(
        r"(?si)<\s*{tag}\s*>.*?<\s*/\s*{tag}\s*>",
        tag = tag_name
    ))
    .expect("valid closed tag regex");

    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;

    for m in closed_pattern.find_iter(text) {
        if in_code_span(m.start(), &ranges) {
            continue;
        }
        result.push_str(&text[last_end..m.start()]);
        last_end = m.end();
    }
    result.push_str(&text[last_end..]);

    // Second pass: strip unclosed tags (opening tag with no matching close, to end of string)
    let unclosed_ranges = code_span_ranges(&result);
    let unclosed_pattern = Regex::new(&format!(r"(?si)<\s*{tag}\s*>.*$", tag = tag_name))
        .expect("valid unclosed tag regex");

    let mut final_result = String::with_capacity(result.len());
    let mut last_end = 0;

    for m in unclosed_pattern.find_iter(&result) {
        if in_code_span(m.start(), &unclosed_ranges) {
            continue;
        }
        final_result.push_str(&result[last_end..m.start()]);
        last_end = m.end();
    }
    final_result.push_str(&result[last_end..]);

    final_result
}

/// Strip model-internal artifacts from text intended for user-facing channels.
///
/// Removes reasoning tags (`<think>`, `<thinking>`, `<thought>`, `<antthinking>`),
/// tool execution tags (`<tool_code>`, `<tool_result>`), and handles unclosed tags.
/// Preserves tags inside markdown code fences and inline code spans.
///
/// Returns the sanitized text. Returns empty string if only whitespace remains.
pub fn sanitize_model_response(text: &str) -> String {
    let mut result = text.to_string();

    for tag in STRIPPED_TAGS {
        result = strip_tag_blocks(&result, tag);
    }

    // Collapse runs of 3+ newlines down to 2 (one blank line)
    let excessive_newlines = Regex::new(r"\n{3,}").expect("valid newline regex");
    result = excessive_newlines.replace_all(&result, "\n\n").to_string();

    let trimmed = result.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Basic stripping --

    #[test]
    fn strips_thinking_tags() {
        let input = "Hello <thinking>internal reasoning here</thinking> world";
        let result = sanitize_model_response(input);
        assert!(!result.contains("thinking"));
        assert!(!result.contains("internal reasoning"));
        assert!(result.contains("Hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn strips_think_tags() {
        let input = "Before <think>some thoughts</think> after";
        let result = sanitize_model_response(input);
        assert!(!result.contains("think"));
        assert!(!result.contains("some thoughts"));
        assert!(result.contains("Before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn strips_thought_tags() {
        let input = "Start <thought>deep thought</thought> end";
        let result = sanitize_model_response(input);
        assert!(!result.contains("thought"));
        assert!(!result.contains("deep thought"));
        assert!(result.contains("Start"));
        assert!(result.contains("end"));
    }

    #[test]
    fn strips_antthinking_tags() {
        let input = "Prefix <antthinking>anthropic internal</antthinking> suffix";
        let result = sanitize_model_response(input);
        assert!(!result.contains("antthinking"));
        assert!(!result.contains("anthropic internal"));
        assert!(result.contains("Prefix"));
        assert!(result.contains("suffix"));
    }

    #[test]
    fn strips_tool_code_tags() {
        let input = "Text <tool_code>print('hello')</tool_code> more text";
        let result = sanitize_model_response(input);
        assert!(!result.contains("tool_code"));
        assert!(!result.contains("print"));
        assert!(result.contains("Text"));
        assert!(result.contains("more text"));
    }

    #[test]
    fn strips_tool_result_tags() {
        let input = "Before <tool_result>output data</tool_result> after";
        let result = sanitize_model_response(input);
        assert!(!result.contains("tool_result"));
        assert!(!result.contains("output data"));
        assert!(result.contains("Before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn strips_multiple_tag_types() {
        let input =
            "<think>thought</think>Hello<tool_code>code</tool_code> world<thinking>more</thinking>";
        let result = sanitize_model_response(input);
        assert!(!result.contains("think"));
        assert!(!result.contains("tool_code"));
        assert!(!result.contains("thought"));
        assert!(!result.contains("code"));
        assert!(!result.contains("more"));
        assert!(result.contains("Hello"));
        assert!(result.contains("world"));
    }

    // -- Case insensitivity --

    #[test]
    fn strips_tags_case_insensitive() {
        let input = "<THINKING>loud thoughts</THINKING> result";
        let result = sanitize_model_response(input);
        assert!(!result.contains("THINKING"));
        assert!(!result.contains("loud thoughts"));
        assert!(result.contains("result"));
    }

    #[test]
    fn strips_tags_mixed_case() {
        let input = "<Think>mixed</Think> ok";
        let result = sanitize_model_response(input);
        assert!(!result.contains("Think"));
        assert!(!result.contains("mixed"));
        assert!(result.contains("ok"));
    }

    // -- Whitespace in tags --

    #[test]
    fn strips_tags_with_whitespace() {
        let input = "< thinking >padded content</ thinking > done";
        let result = sanitize_model_response(input);
        assert!(!result.contains("padded content"));
        assert!(result.contains("done"));
    }

    // -- Multiline content --

    #[test]
    fn strips_multiline_content() {
        let input = "Start\n<thinking>\nLine 1\nLine 2\nLine 3\n</thinking>\nEnd";
        let result = sanitize_model_response(input);
        assert!(!result.contains("Line 1"));
        assert!(!result.contains("Line 2"));
        assert!(!result.contains("Line 3"));
        assert!(result.contains("Start"));
        assert!(result.contains("End"));
    }

    // -- Code span preservation --

    #[test]
    fn preserves_tags_in_fenced_code_blocks() {
        let input = "Look at this:\n```\n<thinking>example tag</thinking>\n```\nDone";
        let result = sanitize_model_response(input);
        assert!(result.contains("<thinking>example tag</thinking>"));
        assert!(result.contains("Look at this:"));
        assert!(result.contains("Done"));
    }

    #[test]
    fn preserves_tags_in_inline_code() {
        let input = "Use `<thinking>` to denote thoughts";
        let result = sanitize_model_response(input);
        assert!(result.contains("`<thinking>`"));
    }

    #[test]
    fn preserves_tags_in_fenced_code_with_language() {
        let input = "Example:\n```xml\n<tool_code>some code</tool_code>\n```\nEnd";
        let result = sanitize_model_response(input);
        assert!(result.contains("<tool_code>some code</tool_code>"));
    }

    #[test]
    fn strips_outside_but_preserves_inside_code() {
        let input = "<thinking>strip me</thinking>\n```\n<thinking>keep me</thinking>\n```";
        let result = sanitize_model_response(input);
        assert!(!result.contains("strip me"));
        assert!(result.contains("keep me"));
    }

    // -- Unclosed tags --

    #[test]
    fn strips_unclosed_tag_to_end() {
        let input = "Visible text\n<tool_code>\nconst x = 1;\nconst y = 2;";
        let result = sanitize_model_response(input);
        assert!(!result.contains("tool_code"));
        assert!(!result.contains("const x"));
        assert!(result.contains("Visible text"));
    }

    #[test]
    fn strips_unclosed_thinking_to_end() {
        let input = "Hello <thinking>this never closes";
        let result = sanitize_model_response(input);
        assert!(!result.contains("thinking"));
        assert!(!result.contains("this never closes"));
        assert!(result.contains("Hello"));
    }

    // -- Empty result --

    #[test]
    fn returns_empty_when_all_content_stripped() {
        let input = "<thinking>everything is internal</thinking>";
        let result = sanitize_model_response(input);
        assert_eq!(result, "");
    }

    #[test]
    fn returns_empty_when_only_whitespace_remains() {
        let input = "  <think>all content</think>  ";
        let result = sanitize_model_response(input);
        assert_eq!(result, "");
    }

    // -- No artifacts = unchanged --

    #[test]
    fn leaves_normal_text_unchanged() {
        let input = "This is a perfectly normal response with no artifacts.";
        let result = sanitize_model_response(input);
        assert_eq!(result, input);
    }

    #[test]
    fn leaves_html_tags_unchanged() {
        let input = "Use <b>bold</b> and <i>italic</i> formatting.";
        let result = sanitize_model_response(input);
        assert_eq!(result, input);
    }

    // -- Real-world Gemini example --

    #[test]
    fn strips_gemini_tool_code_with_json() {
        let input = "First, I'll check for stale and unassigned issues.\n<tool_code>\nconst issues = [\n  {\n    \"id\": \"6505ed6c-ffa2-40f9-a863-869f59a7b3c4\",\n    \"identifier\": \"SPO-42\",\n    \"title\": \"Companies page\"\n  }\n]\n</tool_code>\nHere are the results.";
        let result = sanitize_model_response(input);
        assert!(!result.contains("tool_code"));
        assert!(!result.contains("SPO-42"));
        assert!(result.contains("check for stale"));
        assert!(result.contains("Here are the results"));
    }

    #[test]
    fn strips_gemini_tool_result_with_output() {
        let input = "Processing request.\n<tool_result>\n{\"status\": \"ok\", \"count\": 5}\n</tool_result>\nDone processing.";
        let result = sanitize_model_response(input);
        assert!(!result.contains("tool_result"));
        assert!(!result.contains("status"));
        assert!(result.contains("Processing request."));
        assert!(result.contains("Done processing."));
    }

    // -- Mixed: real content + artifacts --

    #[test]
    fn preserves_surrounding_text() {
        let input = "Introduction.\n<thinking>Let me analyze this carefully.</thinking>\nThe answer is 42.\n<tool_code>compute(42)</tool_code>\nHope that helps!";
        let result = sanitize_model_response(input);
        assert!(result.contains("Introduction."));
        assert!(result.contains("The answer is 42."));
        assert!(result.contains("Hope that helps!"));
        assert!(!result.contains("analyze this carefully"));
        assert!(!result.contains("compute(42)"));
    }

    // -- Whitespace cleanup after stripping --

    #[test]
    fn collapses_excess_blank_lines() {
        let input = "Line 1\n\n\n<thinking>removed</thinking>\n\n\nLine 2";
        let result = sanitize_model_response(input);
        assert!(result.contains("Line 1"));
        assert!(result.contains("Line 2"));
        assert!(!result.contains("\n\n\n"));
    }

    // -- Helper function tests --

    #[test]
    fn code_span_ranges_detects_fenced_blocks() {
        let text = "before\n```\ncode here\n```\nafter";
        let ranges = code_span_ranges(text);
        assert!(!ranges.is_empty());
        let code_pos = text.find("code here").unwrap();
        assert!(in_code_span(code_pos, &ranges));
    }

    #[test]
    fn code_span_ranges_detects_inline_code() {
        let text = "use `<thinking>` in your code";
        let ranges = code_span_ranges(text);
        assert!(!ranges.is_empty());
        let tag_pos = text.find("<thinking>").unwrap();
        assert!(in_code_span(tag_pos, &ranges));
    }

    #[test]
    fn in_code_span_returns_false_for_normal_text() {
        let text = "normal <thinking>text</thinking> here";
        let ranges = code_span_ranges(text);
        let tag_pos = text.find("<thinking>").unwrap();
        assert!(!in_code_span(tag_pos, &ranges));
    }

    #[test]
    fn strip_tag_blocks_handles_single_tag() {
        let text = "before <think>content</think> after";
        let result = strip_tag_blocks(text, "think");
        assert!(!result.contains("content"));
        assert!(result.contains("before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn strip_tag_blocks_preserves_code_spans() {
        let text = "```\n<think>keep</think>\n```\n<think>remove</think>";
        let result = strip_tag_blocks(text, "think");
        assert!(result.contains("keep"));
        assert!(!result.contains("remove"));
    }

    // -- Multiple occurrences --

    #[test]
    fn strips_multiple_occurrences_of_same_tag() {
        let input = "<thinking>first</thinking> middle <thinking>second</thinking> end";
        let result = sanitize_model_response(input);
        assert!(!result.contains("first"));
        assert!(!result.contains("second"));
        assert!(result.contains("middle"));
        assert!(result.contains("end"));
    }

    // -- Edge cases --

    #[test]
    fn handles_empty_input() {
        let result = sanitize_model_response("");
        assert_eq!(result, "");
    }

    #[test]
    fn handles_whitespace_only_input() {
        let result = sanitize_model_response("   \n\n  ");
        assert_eq!(result, "");
    }

    #[test]
    fn handles_empty_tag_content() {
        let input = "Before <thinking></thinking> after";
        let result = sanitize_model_response(input);
        assert!(!result.contains("thinking"));
        assert!(result.contains("Before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn handles_nested_angle_brackets_in_content() {
        let input = "Text <thinking>if x < 5 && y > 3 then ok</thinking> done";
        let result = sanitize_model_response(input);
        assert!(!result.contains("if x < 5"));
        assert!(result.contains("Text"));
        assert!(result.contains("done"));
    }
}
