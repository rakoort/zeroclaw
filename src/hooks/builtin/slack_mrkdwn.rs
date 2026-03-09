use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

use crate::hooks::traits::{HookHandler, HookResult};

/// Converts markdown formatting to Slack mrkdwn in outgoing messages.
///
/// Gemini models frequently emit markdown (`**bold**`, `[text](url)`, `### Header`)
/// instead of Slack mrkdwn (`*bold*`, `<url|text>`, `*Header*`). This hook
/// intercepts all Slack-bound text and silently fixes it.
pub struct SlackMrkdwnHook;

impl SlackMrkdwnHook {
    pub fn new() -> Self {
        Self
    }
}

/// The set of Slack tools whose `message` argument should be sanitised.
const SLACK_MESSAGE_TOOLS: &[&str] = &["slack_send", "slack_dm", "slack_send_thread"];

// Compiled regexes — built once, reused across calls.
static RE_BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*{2,3}([^*]+?)\*{2,3}").unwrap());
static RE_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap());
static RE_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#{1,6}\s+(.+)$").unwrap());
static RE_CODE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[a-zA-Z]*\n(.*?)```").unwrap());

/// Convert markdown formatting to Slack mrkdwn.
///
/// Handles: `**bold**` / `***bold***` → `*bold*`, `[text](url)` → `<url|text>`,
/// `### Header` → `*Header*`, and strips language tags from code fences.
/// Content inside inline code and code blocks is preserved.
fn markdown_to_mrkdwn(input: &str) -> String {
    // Strategy: extract code blocks and inline code spans, replace them with
    // placeholders, run transformations on the remaining text, then restore.
    let mut placeholders: Vec<String> = Vec::new();
    let mut working = input.to_string();

    // 1. Extract code blocks (``` ... ```) — must come before inline code.
    //    Also strip language tags (```python → ```).
    working = RE_CODE_BLOCK
        .replace_all(&working, |caps: &regex::Captures| {
            let idx = placeholders.len();
            let inner = &caps[1];
            placeholders.push(format!("```\n{}```", inner));
            format!("\x00CB{}\x00", idx)
        })
        .into_owned();

    // 2. Extract inline code (`...`).
    let re_inline = Regex::new(r"`[^`]+`").unwrap();
    working = re_inline
        .replace_all(&working, |caps: &regex::Captures| {
            let idx = placeholders.len();
            placeholders.push(caps[0].to_string());
            format!("\x00IC{}\x00", idx)
        })
        .into_owned();

    // 3. Convert markdown headers → bold on own line.
    working = RE_HEADER.replace_all(&working, "*$1*").into_owned();

    // 4. Convert **bold** and ***bold*** → *bold* (must come before link conversion
    //    to avoid mangling link text that contains asterisks).
    working = RE_BOLD.replace_all(&working, "*$1*").into_owned();

    // 5. Convert [text](url) → <url|text>.
    working = RE_LINK
        .replace_all(&working, |caps: &regex::Captures| {
            format!("<{}|{}>", &caps[2], &caps[1])
        })
        .into_owned();

    // 6. Restore placeholders in reverse order.
    for (idx, original) in placeholders.iter().enumerate().rev() {
        let cb_tag = format!("\x00CB{}\x00", idx);
        let ic_tag = format!("\x00IC{}\x00", idx);
        working = working.replace(&cb_tag, original);
        working = working.replace(&ic_tag, original);
    }

    working
}

#[async_trait]
impl HookHandler for SlackMrkdwnHook {
    fn name(&self) -> &str {
        "slack-mrkdwn"
    }

    fn priority(&self) -> i32 {
        // Run early so other hooks see corrected text.
        50
    }

    /// Rewrite the `message` argument of Slack tools.
    async fn before_tool_call(&self, name: String, mut args: Value) -> HookResult<(String, Value)> {
        if SLACK_MESSAGE_TOOLS.contains(&name.as_str()) {
            if let Some(msg) = args.get("message").and_then(Value::as_str) {
                let fixed = markdown_to_mrkdwn(msg);
                if fixed != msg {
                    args["message"] = Value::String(fixed);
                }
            }
        }
        HookResult::Continue((name, args))
    }

    /// Rewrite auto-reply text (the model's direct response sent to the channel).
    async fn on_message_sending(
        &self,
        channel: String,
        recipient: String,
        content: String,
    ) -> HookResult<(String, String, String)> {
        let fixed = markdown_to_mrkdwn(&content);
        HookResult::Continue((channel, recipient, fixed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── markdown_to_mrkdwn unit tests ─────────────────────────────

    #[test]
    fn converts_bold() {
        assert_eq!(markdown_to_mrkdwn("**hello**"), "*hello*");
    }

    #[test]
    fn converts_bold_mid_sentence() {
        assert_eq!(
            markdown_to_mrkdwn("this is **important** stuff"),
            "this is *important* stuff"
        );
    }

    #[test]
    fn converts_multiple_bolds() {
        assert_eq!(markdown_to_mrkdwn("**one** and **two**"), "*one* and *two*");
    }

    #[test]
    fn leaves_single_asterisk_bold_alone() {
        // Already correct mrkdwn — do not double-convert.
        assert_eq!(markdown_to_mrkdwn("*already bold*"), "*already bold*");
    }

    #[test]
    fn converts_markdown_links() {
        assert_eq!(
            markdown_to_mrkdwn("[click here](https://example.com)"),
            "<https://example.com|click here>"
        );
    }

    #[test]
    fn converts_link_with_complex_url() {
        assert_eq!(
            markdown_to_mrkdwn("[SPO-45](https://linear.app/spore/issue/SPO-45)"),
            "<https://linear.app/spore/issue/SPO-45|SPO-45>"
        );
    }

    #[test]
    fn preserves_existing_mrkdwn_links() {
        let input = "<https://example.com|click here>";
        assert_eq!(markdown_to_mrkdwn(input), input);
    }

    #[test]
    fn converts_headers_to_bold() {
        assert_eq!(markdown_to_mrkdwn("### Header"), "*Header*");
        assert_eq!(markdown_to_mrkdwn("## Header"), "*Header*");
        assert_eq!(markdown_to_mrkdwn("# Header"), "*Header*");
    }

    #[test]
    fn converts_header_mid_text() {
        assert_eq!(
            markdown_to_mrkdwn("intro\n### Section\ncontent"),
            "intro\n*Section*\ncontent"
        );
    }

    #[test]
    fn preserves_inline_code() {
        assert_eq!(
            markdown_to_mrkdwn("use `**bold**` for emphasis"),
            "use `**bold**` for emphasis"
        );
    }

    #[test]
    fn preserves_code_blocks() {
        let input = "before\n```\n**not bold**\n[not a link](url)\n```\nafter **bold**";
        let expected = "before\n```\n**not bold**\n[not a link](url)\n```\nafter *bold*";
        assert_eq!(markdown_to_mrkdwn(input), expected);
    }

    #[test]
    fn strips_language_tag_from_code_blocks() {
        let input = "```python\nprint('hello')\n```";
        let expected = "```\nprint('hello')\n```";
        assert_eq!(markdown_to_mrkdwn(input), expected);
    }

    #[test]
    fn converts_bold_italic_triple_asterisks() {
        // Slack mrkdwn doesn't support nested styles.
        // ***text*** → *text*
        assert_eq!(markdown_to_mrkdwn("***emphasis***"), "*emphasis*");
    }

    #[test]
    fn mixed_formatting() {
        let input = "**Overview of Findings:** Uncaptured Commitments\n\n**1. New Issues Needed:**\n- **Typesense Auth Error:** SE ETL Sync failed\n- [NET-55](https://linear.app/netspore/issue/NET-55) needs update";
        let expected = "*Overview of Findings:* Uncaptured Commitments\n\n*1. New Issues Needed:*\n- *Typesense Auth Error:* SE ETL Sync failed\n- <https://linear.app/netspore/issue/NET-55|NET-55> needs update";
        assert_eq!(markdown_to_mrkdwn(input), expected);
    }

    #[test]
    fn passthrough_clean_mrkdwn() {
        let input = "*Bold header*\n\n- <https://linear.app/spore/issue/SPO-45|SPO-45: Billing> — in progress\n- Assigned to <@U05TBBNT94G>";
        assert_eq!(markdown_to_mrkdwn(input), input);
    }

    // ── Hook integration tests ────────────────────────────────────

    #[tokio::test]
    async fn before_tool_call_rewrites_slack_send() {
        let hook = SlackMrkdwnHook::new();
        let args = serde_json::json!({
            "channel_id": "C0AFC38118C",
            "message": "**bold** and [link](https://example.com)"
        });
        match hook.before_tool_call("slack_send".into(), args).await {
            HookResult::Continue((name, args)) => {
                assert_eq!(name, "slack_send");
                assert_eq!(
                    args["message"].as_str().unwrap(),
                    "*bold* and <https://example.com|link>"
                );
            }
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }

    #[tokio::test]
    async fn before_tool_call_rewrites_slack_dm() {
        let hook = SlackMrkdwnHook::new();
        let args = serde_json::json!({
            "user_id": "U05TBBNT94G",
            "message": "**reminder:** check this"
        });
        match hook.before_tool_call("slack_dm".into(), args).await {
            HookResult::Continue((_, args)) => {
                assert_eq!(args["message"].as_str().unwrap(), "*reminder:* check this");
            }
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }

    #[tokio::test]
    async fn before_tool_call_ignores_non_slack_tools() {
        let hook = SlackMrkdwnHook::new();
        let args = serde_json::json!({"query": "**bold**"});
        match hook
            .before_tool_call("linear_issues".into(), args.clone())
            .await
        {
            HookResult::Continue((_, result_args)) => {
                assert_eq!(result_args, args);
            }
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }

    #[tokio::test]
    async fn on_message_sending_rewrites_content() {
        let hook = SlackMrkdwnHook::new();
        match hook
            .on_message_sending(
                "slack".into(),
                "C0AFC38118C".into(),
                "**bold** message".into(),
            )
            .await
        {
            HookResult::Continue((_, _, content)) => {
                assert_eq!(content, "*bold* message");
            }
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }
}
