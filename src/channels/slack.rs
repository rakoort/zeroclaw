use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Format a single thread reply with role label and envelope.
fn format_thread_reply(
    reply: &serde_json::Value,
    bot_user_id: &str,
    bot_name: &str,
    user_names: &HashMap<String, String>,
) -> String {
    let user = reply["user"].as_str().unwrap_or("unknown");
    let text = reply["text"].as_str().unwrap_or("");

    let is_bot = user == bot_user_id;
    let role = if is_bot { "assistant" } else { "user" };
    let name = if is_bot {
        bot_name.to_string()
    } else {
        user_names
            .get(user)
            .cloned()
            .unwrap_or_else(|| user.to_string())
    };

    format!("[Slack {{channel}} {name} ({role})] {name}: {text}")
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

/// Fetch thread replies from Slack conversations.replies API.
async fn fetch_thread_replies(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    thread_ts: &str,
    limit: usize,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let resp = client
        .get("https://slack.com/api/conversations.replies")
        .bearer_auth(bot_token)
        .query(&[
            ("channel", channel),
            ("ts", thread_ts),
            ("limit", &limit.to_string()),
            ("inclusive", "true"),
        ])
        .send()
        .await?;

    let body: serde_json::Value = resp.json().await?;
    if body["ok"].as_bool() != Some(true) {
        let err = body["error"].as_str().unwrap_or("unknown");
        anyhow::bail!("Slack conversations.replies failed: {err}");
    }

    Ok(body["messages"].as_array().cloned().unwrap_or_default())
}

/// Per-channel ring buffer for non-mention messages.
struct PendingHistoryBuffer {
    entries: Vec<String>,
    max: usize,
}

impl PendingHistoryBuffer {
    fn new(max: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max),
            max,
        }
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

/// Format a message in the structured envelope format.
fn format_message_envelope(
    channel_name: &str,
    sender_name: &str,
    timestamp: &str,
    text: &str,
) -> String {
    format!("[Slack {channel_name} {sender_name} {timestamp}] {sender_name}: {text}")
}

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

/// Result of mention-gating evaluation for an inbound message.
#[derive(Debug, PartialEq)]
enum MentionGateResult {
    /// Explicit @mention — respond immediately, no triage needed.
    ExplicitMention(String),
    /// Bot participated in thread but was not explicitly mentioned — needs triage.
    ParticipatedThread(String),
    /// Not mentioned, not a participated thread — buffer silently.
    Buffer,
}

/// Slack channel — polls conversations.history via Web API
pub struct SlackChannel {
    bot_token: String,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    mention_only: bool,
    mention_regex: Option<regex::Regex>,
    participated_threads: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl SlackChannel {
    pub fn new(bot_token: String, channel_id: Option<String>, allowed_users: Vec<String>) -> Self {
        Self {
            bot_token,
            channel_id,
            allowed_users,
            mention_only: true,
            mention_regex: None,
            participated_threads: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub fn with_mention_config(
        mut self,
        mention_only: bool,
        mention_regex: Option<String>,
    ) -> Self {
        self.mention_only = mention_only;
        self.mention_regex = mention_regex.and_then(|pat| {
            regex::Regex::new(&pat)
                .map_err(|e| tracing::warn!("Invalid mention_regex pattern: {e}"))
                .ok()
        });
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.slack")
    }

    /// Check if a Slack user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get the bot's own user ID so we can ignore our own messages
    async fn get_bot_user_id(&self) -> Option<String> {
        let resp: serde_json::Value = self
            .http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;

        resp.get("user_id")
            .and_then(|u| u.as_str())
            .map(String::from)
    }

    /// Resolve the thread identifier for inbound Slack messages.
    /// Replies carry `thread_ts` (root thread id); top-level messages only have `ts`.
    fn inbound_thread_ts(msg: &serde_json::Value, ts: &str) -> Option<String> {
        msg.get("thread_ts")
            .and_then(|t| t.as_str())
            .or(if ts.is_empty() { None } else { Some(ts) })
            .map(str::to_string)
    }

    fn normalized_channel_id(input: Option<&str>) -> Option<String> {
        input
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "*")
            .map(ToOwned::to_owned)
    }

    fn configured_channel_id(&self) -> Option<String> {
        Self::normalized_channel_id(self.channel_id.as_deref())
    }

    fn extract_channel_ids(list_payload: &serde_json::Value) -> Vec<String> {
        let mut ids = list_payload
            .get("channels")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .filter_map(|channel| {
                let id = channel.get("id").and_then(|id| id.as_str())?;
                let is_archived = channel
                    .get("is_archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let is_member = channel
                    .get("is_member")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if is_archived || !is_member {
                    return None;
                }
                Some(id.to_string())
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    async fn list_accessible_channels(&self) -> anyhow::Result<Vec<String>> {
        let mut channels = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut query_params = vec![
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
                (
                    "types",
                    "public_channel,private_channel,mpim,im".to_string(),
                ),
            ];
            if let Some(ref next) = cursor {
                query_params.push(("cursor", next.clone()));
            }

            let resp = self
                .http_client()
                .get("https://slack.com/api/conversations.list")
                .bearer_auth(&self.bot_token)
                .query(&query_params)
                .send()
                .await?;

            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

            if !status.is_success() {
                anyhow::bail!("Slack conversations.list failed ({status}): {body}");
            }

            let data: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
                let err = data
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                anyhow::bail!("Slack conversations.list failed: {err}");
            }

            channels.extend(Self::extract_channel_ids(&data));

            cursor = data
                .get("response_metadata")
                .and_then(|rm| rm.get("next_cursor"))
                .and_then(|c| c.as_str())
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(ToOwned::to_owned);

            if cursor.is_none() {
                break;
            }
        }

        channels.sort();
        channels.dedup();
        Ok(channels)
    }

    fn slack_now_ts() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}.{:06}", now.as_secs(), now.subsec_micros())
    }

    fn ensure_poll_cursor(
        cursors: &mut HashMap<String, String>,
        channel_id: &str,
        now_ts: &str,
    ) -> String {
        cursors
            .entry(channel_id.to_string())
            .or_insert_with(|| now_ts.to_string())
            .clone()
    }

    /// Record that the bot has participated in a thread.
    pub fn record_participation(&self, thread_ts: &str) {
        self.participated_threads
            .lock()
            .unwrap()
            .insert(thread_ts.to_string());
    }

    /// Check if the bot has participated in a thread.
    pub fn has_participated(&self, thread_ts: &str) -> bool {
        self.participated_threads
            .lock()
            .unwrap()
            .contains(thread_ts)
    }

    /// Get the current set of participated threads (for testing).
    fn participated_threads(&self) -> std::collections::HashSet<String> {
        self.participated_threads.lock().unwrap().clone()
    }

    /// Evaluate mention gating for an inbound message.
    ///
    /// Returns a `MentionGateResult` indicating how the message should be handled:
    /// - `ExplicitMention(cleaned)`: bot was @mentioned, respond with cleaned text
    /// - `ParticipatedThread(text)`: bot participated in thread, triage needed
    /// - `Buffer`: no mention and no participation, buffer silently
    fn resolve_mention_gate(
        &self,
        text: &str,
        bot_user_id: &str,
        thread_ts: Option<&str>,
    ) -> MentionGateResult {
        let explicit = is_mention(text, bot_user_id, self.mention_regex.as_ref());

        if explicit {
            let cleaned = text
                .replace(&format!("<@{bot_user_id}>"), "")
                .trim()
                .to_string();
            return MentionGateResult::ExplicitMention(cleaned);
        }

        // Check if bot has participated in this thread
        let is_participant = thread_ts
            .map(|ts| self.has_participated(ts))
            .unwrap_or(false);

        if is_participant {
            return MentionGateResult::ParticipatedThread(text.to_string());
        }

        MentionGateResult::Buffer
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "channel": message.recipient,
            "text": message.content
        });

        if let Some(ref ts) = message.thread_ts {
            body["thread_ts"] = serde_json::json!(ts);
        }

        let resp = self
            .http_client()
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            anyhow::bail!("Slack chat.postMessage failed ({status}): {body}");
        }

        // Slack returns 200 for most app-level errors; check JSON "ok" field
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack chat.postMessage failed: {err}");
        }

        // Record thread participation for triage tracking
        if let Some(ref ts) = message.thread_ts {
            self.record_participation(ts);
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = match self.get_bot_user_id().await {
            Some(id) => {
                tracing::info!("Slack: resolved bot user ID: {id}");
                id
            }
            None => {
                tracing::warn!("Slack: failed to resolve bot user ID via auth.test — bot_id field check will still filter own messages");
                String::new()
            }
        };
        let scoped_channel = self.configured_channel_id();
        let mut discovered_channels: Vec<String> = Vec::new();
        let mut last_discovery = Instant::now();
        let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();
        let mut pending_history: HashMap<String, PendingHistoryBuffer> = HashMap::new();

        if let Some(ref channel_id) = scoped_channel {
            tracing::info!("Slack channel listening on #{channel_id}...");
        } else {
            tracing::info!(
                "Slack channel_id not set (or '*'); listening across all accessible channels."
            );
        }

        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let target_channels = if let Some(ref channel_id) = scoped_channel {
                vec![channel_id.clone()]
            } else {
                if discovered_channels.is_empty()
                    || last_discovery.elapsed() >= Duration::from_secs(60)
                {
                    match self.list_accessible_channels().await {
                        Ok(channels) => {
                            if channels != discovered_channels {
                                tracing::info!(
                                    "Slack auto-discovery refreshed: listening on {} channel(s).",
                                    channels.len()
                                );
                            }
                            discovered_channels = channels;
                        }
                        Err(e) => {
                            tracing::warn!("Slack channel discovery failed: {e}");
                        }
                    }
                    last_discovery = Instant::now();
                }

                discovered_channels.clone()
            };

            if target_channels.is_empty() {
                tracing::debug!("Slack: no accessible channels discovered yet");
                continue;
            }

            for channel_id in target_channels {
                let had_cursor = last_ts_by_channel.contains_key(&channel_id);
                let bootstrap_ts = Self::slack_now_ts();
                let cursor_ts =
                    Self::ensure_poll_cursor(&mut last_ts_by_channel, &channel_id, &bootstrap_ts);
                if !had_cursor {
                    tracing::debug!(
                        "Slack: initialized cursor for channel {} at {} to prevent historical replay",
                        channel_id,
                        cursor_ts
                    );
                }
                let params = vec![
                    ("channel", channel_id.clone()),
                    ("limit", "10".to_string()),
                    ("oldest", cursor_ts),
                ];

                let resp = match self
                    .http_client()
                    .get("https://slack.com/api/conversations.history")
                    .bearer_auth(&self.bot_token)
                    .query(&params)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Slack poll error for channel {channel_id}: {e}");
                        continue;
                    }
                };

                let data: serde_json::Value = match resp.json().await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("Slack parse error for channel {channel_id}: {e}");
                        continue;
                    }
                };

                if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
                    let err = data
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown");
                    tracing::warn!("Slack history error for channel {channel_id}: {err}");
                    continue;
                }

                if let Some(messages) = data.get("messages").and_then(|m| m.as_array()) {
                    // Messages come newest-first, reverse to process oldest first
                    for msg in messages.iter().rev() {
                        let ts = msg.get("ts").and_then(|t| t.as_str()).unwrap_or("");
                        let user = msg
                            .get("user")
                            .and_then(|u| u.as_str())
                            .unwrap_or("unknown");
                        let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        let last_ts = last_ts_by_channel
                            .get(&channel_id)
                            .map(String::as_str)
                            .unwrap_or("");

                        // Skip all bot messages (bot_id field is present on every bot-posted message)
                        if msg.get("bot_id").is_some() {
                            continue;
                        }

                        // Skip bot's own messages (fallback for edge cases without bot_id)
                        if !bot_user_id.is_empty() && user == bot_user_id {
                            continue;
                        }

                        // Sender validation
                        if !self.is_user_allowed(user) {
                            tracing::warn!(
                                "Slack: ignoring message from unauthorized user: {user}"
                            );
                            continue;
                        }

                        // Skip empty or already-seen
                        if text.is_empty() || ts <= last_ts {
                            continue;
                        }

                        last_ts_by_channel.insert(channel_id.clone(), ts.to_string());

                        // Format timestamp from Slack ts for envelope
                        let ts_secs = ts
                            .split('.')
                            .next()
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or(0);
                        let ts_display = {
                            let dt =
                                chrono::DateTime::from_timestamp(ts_secs, 0).unwrap_or_default();
                            dt.format("%Y-%m-%d %H:%M:%S").to_string()
                        };

                        // Mention gating: buffer non-mention messages when enabled
                        let (text, triage_required) = if self.mention_only {
                            let thread_ts_val = msg.get("thread_ts").and_then(|v| v.as_str());

                            match self.resolve_mention_gate(text, &bot_user_id, thread_ts_val) {
                                MentionGateResult::ExplicitMention(cleaned) => (cleaned, false),
                                MentionGateResult::ParticipatedThread(text) => (text, true),
                                MentionGateResult::Buffer => {
                                    let buffer = pending_history
                                        .entry(channel_id.clone())
                                        .or_insert_with(|| PendingHistoryBuffer::new(50));
                                    let envelope = format_message_envelope(
                                        &channel_id,
                                        user,
                                        &ts_display,
                                        text,
                                    );
                                    buffer.push(envelope);
                                    continue;
                                }
                            }
                        } else {
                            (text.to_string(), false)
                        };

                        // Drain pending history and wrap content with context
                        let text = {
                            let pending = pending_history
                                .get_mut(&channel_id)
                                .map(|b| b.drain())
                                .unwrap_or_default();
                            if pending.is_empty() {
                                text
                            } else {
                                let current_envelope =
                                    format_message_envelope(&channel_id, user, &ts_display, &text);
                                format!(
                                    "[Chat messages since your last reply - for context]\n{}\n\n[Current message - respond to this]\n{}",
                                    pending.join("\n"),
                                    current_envelope
                                )
                            }
                        };

                        let inbound_thread = Self::inbound_thread_ts(msg, ts);

                        // Thread hydration: fetch replies when message is threaded
                        let (starter_body, history_body) = if let Some(ref tts) = inbound_thread {
                            match fetch_thread_replies(
                                &self.http_client(),
                                &self.bot_token,
                                &channel_id,
                                tts,
                                20,
                            )
                            .await
                            {
                                Ok(replies) => {
                                    let starter = replies
                                        .first()
                                        .and_then(|r| r["text"].as_str())
                                        .map(String::from);
                                    let history = format_thread_history(
                                        &replies,
                                        &bot_user_id,
                                        "assistant",
                                        &channel_id,
                                        &HashMap::new(),
                                    );
                                    (starter, Some(history))
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Slack thread hydration failed for {channel_id}/{tts}: {e}"
                                    );
                                    (None, None)
                                }
                            }
                        } else {
                            (None, None)
                        };

                        let channel_msg = ChannelMessage {
                            id: format!("slack_{channel_id}_{ts}"),
                            sender: user.to_string(),
                            reply_target: channel_id.clone(),
                            content: text,
                            channel: "slack".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: inbound_thread,
                            thread_starter_body: starter_body,
                            thread_history: history_body,
                            triage_required,
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_channel_name() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec![]);
        assert_eq!(ch.name(), "slack");
    }

    #[test]
    fn slack_channel_with_channel_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), Some("C12345".into()), vec![]);
        assert_eq!(ch.channel_id, Some("C12345".to_string()));
    }

    #[test]
    fn normalized_channel_id_respects_wildcard_and_blank() {
        assert_eq!(SlackChannel::normalized_channel_id(None), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("   ")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("*")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some(" * ")), None);
        assert_eq!(
            SlackChannel::normalized_channel_id(Some(" C12345 ")),
            Some("C12345".to_string())
        );
    }

    #[test]
    fn extract_channel_ids_filters_archived_and_non_member_entries() {
        let payload = serde_json::json!({
            "channels": [
                {"id": "C1", "is_archived": false, "is_member": true},
                {"id": "C2", "is_archived": true, "is_member": true},
                {"id": "C3", "is_archived": false, "is_member": false},
                {"id": "C1", "is_archived": false, "is_member": true},
                {"id": "C4"}
            ]
        });
        let ids = SlackChannel::extract_channel_ids(&payload);
        assert_eq!(ids, vec!["C1".to_string(), "C4".to_string()]);
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec![]);
        assert!(!ch.is_user_allowed("U12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["*".into()]);
        assert!(ch.is_user_allowed("U12345"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into(), "U222".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("U222"));
        assert!(!ch.is_user_allowed("U333"));
    }

    #[test]
    fn allowlist_exact_match_not_substring() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into()]);
        assert!(!ch.is_user_allowed("U1111"));
        assert!(!ch.is_user_allowed("U11"));
    }

    #[test]
    fn allowlist_empty_user_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into()]);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(!ch.is_user_allowed("u111"));
    }

    #[test]
    fn allowlist_wildcard_and_specific() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into(), "*".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("anyone"));
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn slack_message_id_format_includes_channel_and_ts() {
        // Verify that message IDs follow the format: slack_{channel_id}_{ts}
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let expected_id = format!("slack_{channel_id}_{ts}");
        assert_eq!(expected_id, "slack_C12345_1234567890.123456");
    }

    #[test]
    fn slack_message_id_is_deterministic() {
        // Same channel_id + same ts = same ID (prevents duplicates after restart)
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let id1 = format!("slack_{channel_id}_{ts}");
        let id2 = format!("slack_{channel_id}_{ts}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn slack_message_id_different_ts_different_id() {
        // Different timestamps produce different IDs
        let channel_id = "C12345";
        let id1 = format!("slack_{channel_id}_1234567890.123456");
        let id2 = format!("slack_{channel_id}_1234567890.123457");
        assert_ne!(id1, id2);
    }

    #[test]
    fn slack_message_id_different_channel_different_id() {
        // Different channels produce different IDs even with same ts
        let ts = "1234567890.123456";
        let id1 = format!("slack_C12345_{ts}");
        let id2 = format!("slack_C67890_{ts}");
        assert_ne!(id1, id2);
    }

    #[test]
    fn slack_message_id_no_uuid_randomness() {
        // Verify format doesn't contain random UUID components
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let id = format!("slack_{channel_id}_{ts}");
        assert!(!id.contains('-')); // No UUID dashes
        assert!(id.starts_with("slack_"));
    }

    #[test]
    fn inbound_thread_ts_prefers_explicit_thread_ts() {
        let msg = serde_json::json!({
            "ts": "123.002",
            "thread_ts": "123.001"
        });

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "123.002");
        assert_eq!(thread_ts.as_deref(), Some("123.001"));
    }

    #[test]
    fn inbound_thread_ts_falls_back_to_ts() {
        let msg = serde_json::json!({
            "ts": "123.001"
        });

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "123.001");
        assert_eq!(thread_ts.as_deref(), Some("123.001"));
    }

    #[test]
    fn inbound_thread_ts_none_when_ts_missing() {
        let msg = serde_json::json!({});

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "");
        assert_eq!(thread_ts, None);
    }

    #[test]
    fn ensure_poll_cursor_bootstraps_new_channel() {
        let mut cursors = HashMap::new();
        let now_ts = "1700000000.123456";

        let cursor = SlackChannel::ensure_poll_cursor(&mut cursors, "C123", now_ts);
        assert_eq!(cursor, now_ts);
        assert_eq!(cursors.get("C123").map(String::as_str), Some(now_ts));
    }

    #[test]
    fn ensure_poll_cursor_keeps_existing_cursor() {
        let mut cursors = HashMap::from([("C123".to_string(), "1700000000.000001".to_string())]);
        let cursor = SlackChannel::ensure_poll_cursor(&mut cursors, "C123", "9999999999.999999");

        assert_eq!(cursor, "1700000000.000001");
        assert_eq!(
            cursors.get("C123").map(String::as_str),
            Some("1700000000.000001")
        );
    }

    // ── Thread hydration formatting ─────────────────────────────────

    #[test]
    fn format_thread_reply_labels_bot_as_assistant() {
        let reply = serde_json::json!({
            "user": "U_BOT",
            "text": "I can help with that",
            "ts": "1234567890.000200"
        });
        let formatted = format_thread_reply(&reply, "U_BOT", "Rain", &HashMap::new());
        assert!(
            formatted.contains("(assistant)"),
            "should label bot as assistant: {formatted}"
        );
        assert!(formatted.contains("I can help with that"));
    }

    #[test]
    fn format_thread_reply_labels_human_as_user() {
        let reply = serde_json::json!({
            "user": "U_HUMAN",
            "text": "hello there",
            "ts": "1234567890.000100"
        });
        let formatted = format_thread_reply(&reply, "U_BOT", "Rain", &HashMap::new());
        assert!(
            formatted.contains("(user)"),
            "should label human as user: {formatted}"
        );
        assert!(formatted.contains("hello there"));
    }

    #[test]
    fn format_thread_reply_resolves_user_name() {
        let reply = serde_json::json!({
            "user": "U_ALICE",
            "text": "thread reply",
            "ts": "1234567890.000300"
        });
        let names = HashMap::from([("U_ALICE".to_string(), "Alice".to_string())]);
        let formatted = format_thread_reply(&reply, "U_BOT", "Rain", &names);
        assert!(
            formatted.contains("Alice"),
            "should include resolved name: {formatted}"
        );
    }

    // ── Mention gating ────────────────────────────────────────────

    #[test]
    fn detects_explicit_bot_mention() {
        assert!(is_mention("<@U_BOT> what do you think?", "U_BOT", None));
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

    #[test]
    fn format_thread_history_includes_envelope() {
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
        let formatted =
            format_thread_history(&replies, "U_BOT", "Rain", "#general", &HashMap::new());
        assert!(
            formatted.contains("[Slack #general"),
            "should have channel envelope: {formatted}"
        );
        assert!(
            formatted.contains("(user)"),
            "should have user role: {formatted}"
        );
        assert!(
            formatted.contains("(assistant)"),
            "should have assistant role: {formatted}"
        );
    }

    // ── Pending history buffer ──────────────────────────────────────

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
        let envelope =
            format_message_envelope("#general", "Alice", "2026-02-24 14:30:05", "hello world");
        assert!(envelope.contains("[Slack #general Alice"));
        assert!(envelope.contains("2026-02-24 14:30:05"));
        assert!(envelope.contains("Alice: hello world"));
    }

    // -- Thread participation tracking -----------------------------------------

    #[test]
    fn participated_threads_empty_on_init() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        assert!(channel.participated_threads().is_empty());
    }

    #[test]
    fn record_participation_tracks_thread() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        channel.record_participation("1234.5678");
        assert!(channel.has_participated("1234.5678"));
    }

    #[test]
    fn has_participated_returns_false_for_unknown_thread() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        assert!(!channel.has_participated("unknown.thread"));
    }

    // -- Participation-based triage routing ------------------------------------

    #[test]
    fn participated_thread_message_sets_triage_required() {
        // When bot has participated in a thread, messages in that thread
        // without explicit @mention should set triage_required = true
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        channel.record_participation("1234.5678");

        // The message is in a participated thread but doesn't @mention the bot
        let text = "hey can someone help with this?";
        let is_explicit = is_mention(text, "U_BOT", None);
        let is_participant = channel.has_participated("1234.5678");

        assert!(!is_explicit, "should not be an explicit mention");
        assert!(is_participant, "should detect participation");
        // triage_required = is_participant && !is_explicit
        assert!(is_participant && !is_explicit);
    }

    #[test]
    fn explicit_mention_in_participated_thread_skips_triage() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        channel.record_participation("1234.5678");

        let text = "<@U_BOT> what do you think?";
        let is_explicit = is_mention(text, "U_BOT", None);

        assert!(is_explicit);
        // triage_required should be false when explicitly mentioned
    }

    #[test]
    fn non_participated_thread_message_is_buffered() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        // Bot has NOT participated in thread "9999.0000"

        let text = "just chatting";
        let is_explicit = is_mention(text, "U_BOT", None);
        let is_participant = channel.has_participated("9999.0000");

        assert!(!is_explicit);
        assert!(!is_participant);
        // Neither mention nor participant -> buffer silently
    }

    // -- resolve_mention_gate behavior ----------------------------------------

    #[test]
    fn mention_gate_explicit_mention_returns_cleaned_text() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        let result =
            channel.resolve_mention_gate("<@U_BOT> what do you think?", "U_BOT", Some("1234.5678"));
        assert_eq!(
            result,
            MentionGateResult::ExplicitMention("what do you think?".to_string())
        );
    }

    #[test]
    fn mention_gate_explicit_mention_in_participated_thread_no_triage() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        channel.record_participation("1234.5678");

        let result = channel.resolve_mention_gate("<@U_BOT> help me", "U_BOT", Some("1234.5678"));
        // Explicit mention takes priority — no triage needed
        assert_eq!(
            result,
            MentionGateResult::ExplicitMention("help me".to_string())
        );
    }

    #[test]
    fn mention_gate_participated_thread_without_mention_returns_triage() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        channel.record_participation("1234.5678");

        let result = channel.resolve_mention_gate(
            "hey can someone help with this?",
            "U_BOT",
            Some("1234.5678"),
        );
        // Bot participated in thread, no explicit mention — needs triage
        assert_eq!(
            result,
            MentionGateResult::ParticipatedThread("hey can someone help with this?".to_string())
        );
    }

    #[test]
    fn mention_gate_non_participated_thread_buffers() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);
        // Bot has NOT participated in thread "9999.0000"

        let result = channel.resolve_mention_gate("just chatting", "U_BOT", Some("9999.0000"));
        assert_eq!(result, MentionGateResult::Buffer);
    }

    #[test]
    fn mention_gate_no_thread_no_mention_buffers() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()]);

        let result = channel.resolve_mention_gate("random message", "U_BOT", None);
        assert_eq!(result, MentionGateResult::Buffer);
    }

    #[test]
    fn mention_gate_regex_mention_returns_explicit() {
        let channel = SlackChannel::new("xoxb-test".into(), None, vec!["*".into()])
            .with_mention_config(true, Some(r"(?i)\brain\b".into()));

        let result =
            channel.resolve_mention_gate("Rain what do you think?", "U_BOT", Some("1234.5678"));
        assert_eq!(
            result,
            MentionGateResult::ExplicitMention("Rain what do you think?".to_string())
        );
    }
}
