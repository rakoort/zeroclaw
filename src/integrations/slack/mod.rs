pub mod client;
pub mod tools;

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::SlackIntegrationConfig;
use crate::integrations::Integration;
use crate::tools::traits::Tool;

use self::client::SlackClient;
use self::tools::all_slack_tools;

const MAX_PARTICIPATED_THREADS: usize = 1000;
const SLACK_MESSAGE_CHUNK_LIMIT: usize = 4000;

/// Shared state between `SlackIntegration` (Integration) and its Channel adapter.
struct SlackShared {
    client: Arc<SlackClient>,
    config: SlackIntegrationConfig,
    participated_threads: Mutex<HashSet<String>>,
    mention_regex: Option<regex::Regex>,
    /// Bot user ID, resolved at listen-time via `auth.test`.
    bot_user_id: Mutex<Option<String>>,
}

/// Native Slack integration that provides both tools and a channel.
pub struct SlackIntegration {
    shared: Arc<SlackShared>,
}

impl SlackIntegration {
    pub fn new(config: SlackIntegrationConfig) -> Self {
        let client = Arc::new(SlackClient::new(
            config.bot_token.clone(),
            config.app_token.clone(),
        ));
        Self::new_with_client(config, client)
    }

    pub(crate) fn new_with_client(
        config: SlackIntegrationConfig,
        client: Arc<SlackClient>,
    ) -> Self {
        let mention_regex = config.mention_regex.as_deref().and_then(|r| {
            regex::Regex::new(r)
                .map_err(|e| warn!(pattern = r, "invalid mention_regex: {e}"))
                .ok()
        });

        Self {
            shared: Arc::new(SlackShared {
                client,
                config,
                participated_threads: Mutex::new(HashSet::new()),
                mention_regex,
                bot_user_id: Mutex::new(None),
            }),
        }
    }
}

#[async_trait]
impl Integration for SlackIntegration {
    fn name(&self) -> &str {
        "slack"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        all_slack_tools(Arc::clone(&self.shared.client))
    }

    async fn health_check(&self) -> bool {
        self.shared
            .client
            .api_post("auth.test", &json!({}))
            .await
            .is_ok()
    }

    fn as_channel(&self) -> Option<Arc<dyn Channel>> {
        Some(Arc::new(SlackChannelAdapter {
            shared: Arc::clone(&self.shared),
        }))
    }
}

/// Channel adapter that delegates to the shared Slack state.
struct SlackChannelAdapter {
    shared: Arc<SlackShared>,
}

#[async_trait]
impl Channel for SlackChannelAdapter {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let chunks = split_message(&message.content, SLACK_MESSAGE_CHUNK_LIMIT);

        for chunk in &chunks {
            let mut body = json!({
                "channel": message.recipient,
                "text": chunk,
            });

            if let Some(ts) = &message.thread_ts {
                body["thread_ts"] = json!(ts);
            }

            self.shared
                .client
                .api_post("chat.postMessage", &body)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        // Record thread participation after successful send.
        if let Some(ts) = &message.thread_ts {
            let mut threads = self.shared.participated_threads.lock();
            if threads.len() >= MAX_PARTICIPATED_THREADS {
                // Evict an arbitrary entry.
                if let Some(old) = threads.iter().next().cloned() {
                    threads.remove(&old);
                }
            }
            threads.insert(ts.clone());
        }

        // Remove ack reaction if present.
        if let Some(ack_ts) = &message.ack_reaction_ts {
            if let Err(e) = self
                .shared
                .client
                .api_post(
                    "reactions.remove",
                    &json!({
                        "channel": message.recipient,
                        "name": "eyes",
                        "timestamp": ack_ts
                    }),
                )
                .await
            {
                warn!("failed to remove ack reaction: {e}");
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Resolve bot user ID via auth.test.
        if let Ok(resp) = self.shared.client.api_post("auth.test", &json!({})).await {
            if let Some(uid) = resp.get("user_id").and_then(|v| v.as_str()) {
                *self.shared.bot_user_id.lock() = Some(uid.to_string());
                info!(bot_user_id = uid, "slack bot identity resolved");
            }
        }

        // Socket Mode connection loop with reconnect backoff.
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            match self.run_socket_mode(&tx).await {
                Ok(()) => {
                    info!("slack socket mode disconnected, reconnecting");
                    backoff = Duration::from_secs(1);
                }
                Err(e) => {
                    warn!(error = %e, backoff_secs = backoff.as_secs(), "slack socket mode error");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.shared
            .client
            .api_post("auth.test", &json!({}))
            .await
            .is_ok()
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.shared
            .client
            .api_post(
                "reactions.add",
                &json!({
                    "channel": channel_id,
                    "timestamp": message_id,
                    "name": emoji,
                }),
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.shared
            .client
            .api_post(
                "reactions.remove",
                &json!({
                    "channel": channel_id,
                    "timestamp": message_id,
                    "name": emoji,
                }),
            )
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

impl SlackChannelAdapter {
    /// Run one Socket Mode WebSocket session; returns on disconnect.
    async fn run_socket_mode(
        &self,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        // Get a WebSocket URL via apps.connections.open (uses app_token).
        let ws_url = self.open_socket_mode_connection().await?;

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let (mut write, mut read) = futures_util::StreamExt::split(ws_stream);

        use futures_util::SinkExt;
        use futures_util::StreamExt;

        let mut dedup: Vec<String> = Vec::with_capacity(100);

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    warn!("websocket read error: {e}");
                    break;
                }
            };

            let text = match msg {
                WsMessage::Text(t) => t,
                WsMessage::Close(_) => break,
                WsMessage::Ping(d) => {
                    let _ = write.send(WsMessage::Pong(d)).await;
                    continue;
                }
                _ => continue,
            };

            let envelope: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Ack every envelope with an envelope_id.
            if let Some(eid) = envelope.get("envelope_id").and_then(|v| v.as_str()) {
                let ack = json!({"envelope_id": eid});
                let _ = write.send(WsMessage::Text(ack.to_string().into())).await;
            }

            let env_type = envelope.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if env_type == "disconnect" {
                info!("slack requested disconnect");
                break;
            }
            if env_type != "events_api" {
                continue;
            }

            let event = match envelope.get("payload").and_then(|p| p.get("event")) {
                Some(e) => e,
                None => continue,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if event_type != "message" {
                continue;
            }

            // Skip bot messages and message_changed subtypes.
            if event.get("subtype").is_some() {
                continue;
            }

            let text_content = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text_content.is_empty() {
                continue;
            }

            let user = event
                .get("user")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let channel = event
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ts = event
                .get("ts")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let thread_ts = event
                .get("thread_ts")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Dedup.
            let dedup_key = format!("{channel}:{ts}");
            if dedup.contains(&dedup_key) {
                continue;
            }
            if dedup.len() >= 100 {
                dedup.remove(0);
            }
            dedup.push(dedup_key);

            // Channel scoping.
            if let Some(ref scope_channel) = self.shared.config.channel_id {
                if !scope_channel.is_empty() && channel != *scope_channel {
                    continue;
                }
            }

            // Bot self-filter.
            {
                let bot_uid = self.shared.bot_user_id.lock().clone();
                if let Some(ref uid) = bot_uid {
                    if user == *uid {
                        continue;
                    }
                }
            }

            // Allowlist.
            if !is_user_allowed(&user, &self.shared.config.allowed_users) {
                debug!(user = user, "slack user not in allowlist, skipping");
                continue;
            }

            // Mention gating.
            let effective_thread_ts = thread_ts.clone().unwrap_or_else(|| ts.clone());
            let (triage_required, should_process) = self.evaluate_mention_gate(
                text_content,
                &effective_thread_ts,
            );

            if !should_process {
                continue;
            }

            // Add ack reaction.
            let ack_reaction_ts = if self
                .shared
                .client
                .api_post(
                    "reactions.add",
                    &json!({
                        "channel": &channel,
                        "timestamp": &ts,
                        "name": "eyes"
                    }),
                )
                .await
                .is_ok()
            {
                Some(ts.clone())
            } else {
                None
            };

            // Hydrate thread context if in a thread.
            let (thread_starter_body, thread_history) = if thread_ts.is_some() {
                self.fetch_thread_context(&channel, &effective_thread_ts)
                    .await
            } else {
                (None, None)
            };

            let msg = ChannelMessage {
                id: format!("slack-{channel}-{ts}"),
                sender: user,
                reply_target: channel.clone(),
                content: text_content.to_string(),
                channel: format!("slack:{channel}"),
                timestamp: parse_slack_ts(&ts),
                thread_ts: Some(effective_thread_ts),
                thread_starter_body,
                thread_history,
                triage_required,
                ack_reaction_ts,
            };

            if tx.send(msg).await.is_err() {
                info!("channel receiver dropped, stopping slack listener");
                return Ok(());
            }
        }

        Ok(())
    }

    async fn open_socket_mode_connection(&self) -> anyhow::Result<String> {
        // apps.connections.open uses the app-level token, not bot token.
        // We need a dedicated HTTP call with the app_token.
        let http = reqwest::Client::new();
        let resp = http
            .post(format!(
                "{}/api/apps.connections.open",
                // SlackClient's base_url isn't directly accessible, but for Socket Mode
                // we always use the real Slack API.
                "https://slack.com"
            ))
            .bearer_auth(&self.shared.config.app_token)
            .send()
            .await?;

        let json: serde_json::Value = resp.json().await?;
        let ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let err = json
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("apps.connections.open failed: {err}");
        }

        json.get("url")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("missing url in apps.connections.open response"))
    }

    fn evaluate_mention_gate(
        &self,
        text: &str,
        thread_ts: &str,
    ) -> (bool, bool) {
        if !self.shared.config.mention_only {
            return (false, true);
        }

        // Check explicit mention.
        let bot_uid = self.shared.bot_user_id.lock().clone();
        let explicit_mention = if let Some(ref uid) = bot_uid {
            text.contains(&format!("<@{uid}>"))
        } else {
            false
        };

        // Check custom mention regex.
        let regex_mention = self
            .shared
            .mention_regex
            .as_ref()
            .map(|r| r.is_match(text))
            .unwrap_or(false);

        if explicit_mention || regex_mention {
            return (false, true);
        }

        // Check thread participation.
        let participated = self
            .shared
            .participated_threads
            .lock()
            .contains(thread_ts);

        if participated {
            // Triage required — the bot has participated but wasn't explicitly mentioned.
            return (true, true);
        }

        // Not mentioned, not in participated thread — skip.
        (false, false)
    }

    async fn fetch_thread_context(
        &self,
        channel: &str,
        thread_ts: &str,
    ) -> (Option<String>, Option<String>) {
        let resp = self
            .shared
            .client
            .api_get(
                "conversations.replies",
                &[("channel", channel), ("ts", thread_ts), ("limit", "50")],
            )
            .await;

        let messages = match resp {
            Ok(v) => v
                .get("messages")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default(),
            Err(e) => {
                warn!("failed to fetch thread context: {e}");
                return (None, None);
            }
        };

        if messages.is_empty() {
            return (None, None);
        }

        let bot_uid = self.shared.bot_user_id.lock().clone();
        let starter_body = messages
            .first()
            .and_then(|m| m.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from);

        let history_lines: Vec<String> = messages
            .iter()
            .skip(1)
            .filter_map(|m| {
                let user = m.get("user").and_then(|u| u.as_str()).unwrap_or("unknown");
                let text = m.get("text").and_then(|t| t.as_str()).unwrap_or("");
                let role = if bot_uid.as_deref() == Some(user) {
                    "assistant"
                } else {
                    "user"
                };
                if text.is_empty() {
                    None
                } else {
                    Some(format!("[{role}] {user}: {text}"))
                }
            })
            .collect();

        let history = if history_lines.is_empty() {
            None
        } else {
            Some(history_lines.join("\n"))
        };

        (starter_body, history)
    }
}

fn is_user_allowed(user_id: &str, allowed_users: &[String]) -> bool {
    if allowed_users.is_empty() {
        return false;
    }
    allowed_users
        .iter()
        .any(|u| u == "*" || u == user_id)
}

fn parse_slack_ts(ts: &str) -> u64 {
    ts.split('.')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Split a message into chunks at natural boundaries.
fn split_message(content: &str, max_len: usize) -> Vec<String> {
    if content.len() <= max_len {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Clamp to a valid UTF-8 char boundary so slicing never panics.
        let mut safe_max = max_len;
        while safe_max > 0 && !remaining.is_char_boundary(safe_max) {
            safe_max -= 1;
        }
        let slice = &remaining[..safe_max];
        // Try to split at paragraph, then newline, then space.
        let split_at = slice
            .rfind("\n\n")
            .or_else(|| slice.rfind('\n'))
            .or_else(|| slice.rfind(' '))
            .unwrap_or(safe_max);

        let (chunk, rest) = remaining.split_at(split_at);
        let chunk = chunk.trim_end();
        if !chunk.is_empty() {
            chunks.push(chunk.to_string());
        }
        remaining = rest.trim_start();
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::client::SlackClient;
    use super::tools::all_slack_tools;
    use super::SlackIntegration;
    use crate::config::SlackIntegrationConfig;
    use crate::integrations::Integration;
    use std::sync::Arc;

    #[test]
    fn all_slack_tools_returns_9_tools() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into()));
        let tools = all_slack_tools(client);
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn all_slack_tools_have_valid_json_schemas() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into()));
        let tools = all_slack_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"], "object",
                "Tool {} schema must be object",
                tool.name()
            );
            assert!(
                schema.get("properties").is_some(),
                "Tool {} must have properties",
                tool.name()
            );
        }
    }

    fn test_config() -> SlackIntegrationConfig {
        SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        }
    }

    #[test]
    fn split_message_multibyte_does_not_panic() {
        // Emoji is 4 bytes; place the split boundary mid-character.
        let emoji_msg = "a".repeat(3998) + "\u{1F600}\u{1F600}"; // 3998 + 4 + 4 = 4006 bytes
        let chunks = super::split_message(&emoji_msg, super::SLACK_MESSAGE_CHUNK_LIMIT);
        let reassembled: String = chunks.join("");
        // All original content must survive (whitespace-trimmed at boundaries is ok).
        assert_eq!(reassembled.chars().count(), emoji_msg.chars().count());
    }

    #[test]
    fn split_message_short_returns_single_chunk() {
        let chunks = super::split_message("hello world", 4000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn split_message_splits_at_paragraph() {
        let msg = "a".repeat(2000) + "\n\n" + &"b".repeat(2000);
        let chunks = super::split_message(&msg, 4000);
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn slack_integration_name() {
        let integration = SlackIntegration::new(test_config());
        assert_eq!(integration.name(), "slack");
    }

    #[test]
    fn slack_integration_returns_9_tools() {
        let integration = SlackIntegration::new(test_config());
        assert_eq!(integration.tools().len(), 9);
    }

    #[test]
    fn slack_integration_as_channel_returns_some() {
        let integration = Arc::new(SlackIntegration::new(test_config()));
        assert!(integration.as_channel().is_some());
    }

    #[tokio::test]
    async fn slack_integration_health_check_fails_without_server() {
        let config = SlackIntegrationConfig {
            bot_token: "xoxb-test".into(),
            app_token: "xapp-test".into(),
            channel_id: None,
            allowed_users: vec![],
            mention_only: true,
            mention_regex: None,
            triage_model: None,
        };
        // With default base_url (https://slack.com) and a fake token, health_check
        // should fail (network error or auth error).
        let integration = SlackIntegration::new(config);
        let healthy = integration.health_check().await;
        assert!(!healthy);
    }
}
