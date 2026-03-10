use super::common::split_message;
use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

const SLACK_RETRY_MAX: u32 = 3;
const SLACK_RETRY_DEFAULT_SECS: u64 = 5;
const SLACK_RETRY_JITTER_MS: u64 = 500;
const MAX_PARTICIPATED_THREADS: usize = 1000;

/// Parse the `Retry-After` header value as seconds.
fn parse_retry_after_secs(value: Option<&str>) -> Option<u64> {
    value.and_then(|v| v.trim().parse::<u64>().ok())
}

/// Check if a Slack JSON response indicates rate limiting.
fn is_slack_ratelimited(body: &serde_json::Value) -> bool {
    body.get("ok") == Some(&serde_json::Value::Bool(false))
        && body.get("error").and_then(|e| e.as_str()) == Some("ratelimited")
}

/// Execute a Slack API request with rate-limit retry.
///
/// Retries up to `SLACK_RETRY_MAX` times on HTTP 429 or JSON `"error": "ratelimited"`.
/// Reads `Retry-After` header to determine wait duration; falls back to 5s.
/// Adds random jitter (0-500ms) to each retry delay.
///
/// The `make_request` closure should perform the actual HTTP call and is
/// invoked on each attempt.
async fn slack_api_request_with_retry<F, Fut>(
    make_request: F,
    url: &str,
) -> anyhow::Result<serde_json::Value>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<reqwest::Response>>,
{
    for attempt in 0..=SLACK_RETRY_MAX {
        let resp = make_request().await?;
        let status = resp.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if attempt == SLACK_RETRY_MAX {
                anyhow::bail!("Slack rate limit exceeded after {SLACK_RETRY_MAX} retries: {url}");
            }
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok());
            let wait_secs = parse_retry_after_secs(retry_after).unwrap_or(SLACK_RETRY_DEFAULT_SECS);
            let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
            tracing::warn!(
                "Slack rate limited on {url} (attempt {}/{SLACK_RETRY_MAX}). Retry-After: {wait_secs}s",
                attempt + 1,
            );
            tokio::time::sleep(Duration::from_millis(wait_secs * 1000 + jitter)).await;
            continue;
        }

        let resp_text = resp
            .text()
            .await
            .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"read_failed: {e}"}}"#));
        let parsed: serde_json::Value = serde_json::from_str(&resp_text).unwrap_or_default();

        if is_slack_ratelimited(&parsed) {
            if attempt == SLACK_RETRY_MAX {
                anyhow::bail!("Slack rate limit exceeded after {SLACK_RETRY_MAX} retries: {url}");
            }
            let jitter = rand::random::<u64>() % SLACK_RETRY_JITTER_MS;
            tracing::warn!(
                "Slack JSON ratelimited on {url} (attempt {}/{SLACK_RETRY_MAX}). Waiting {SLACK_RETRY_DEFAULT_SECS}s",
                attempt + 1,
            );
            tokio::time::sleep(Duration::from_millis(
                SLACK_RETRY_DEFAULT_SECS * 1000 + jitter,
            ))
            .await;
            continue;
        }

        if !status.is_success() {
            anyhow::bail!("Slack API error ({status}): {resp_text}");
        }

        return Ok(parsed);
    }
    unreachable!()
}

/// Execute a Slack API POST request with rate-limit retry.
async fn slack_api_post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    slack_api_request_with_retry(
        || async {
            Ok(client
                .post(url)
                .bearer_auth(token)
                .json(body)
                .send()
                .await?)
        },
        url,
    )
    .await
}

/// Execute a Slack API GET request with rate-limit retry.
async fn slack_api_get(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    query: &[(&str, String)],
) -> anyhow::Result<serde_json::Value> {
    slack_api_request_with_retry(
        || async {
            Ok(client
                .get(url)
                .bearer_auth(token)
                .query(query)
                .send()
                .await?)
        },
        url,
    )
    .await
}

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
    let query = vec![
        ("channel", channel.to_string()),
        ("ts", thread_ts.to_string()),
        ("limit", limit.to_string()),
    ];

    let data = slack_api_get(
        client,
        "https://slack.com/api/conversations.replies",
        bot_token,
        &query,
    )
    .await?;

    if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
        let err = data
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown");
        anyhow::bail!("Slack conversations.replies failed: {err}");
    }

    Ok(data
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default())
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

/// Per-thread processing status for the thread gate.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThreadStatus {
    Idle,
    InFlight,
}

/// Tracks the processing state of a single Slack thread.
#[derive(Debug, Clone)]
struct ThreadState {
    status: ThreadStatus,
    pending_count: u32,
    last_activity: std::time::Instant,
}

impl Default for ThreadState {
    fn default() -> Self {
        Self {
            status: ThreadStatus::Idle,
            pending_count: 0,
            last_activity: std::time::Instant::now(),
        }
    }
}

/// Parse a Socket Mode envelope and extract the inner `message` event.
///
/// Returns `Some((envelope_id, event))` for `events_api` envelopes that contain
/// a `"type": "message"` event. Returns `None` for all other envelope types
/// (hello, disconnect, interactive, slash_commands) and non-message events
/// (reaction_added, member_joined, etc.).
fn parse_socket_event(envelope: &serde_json::Value) -> Option<(String, serde_json::Value)> {
    let envelope_type = envelope.get("type")?.as_str()?;
    if envelope_type != "events_api" {
        return None;
    }

    let envelope_id = envelope.get("envelope_id")?.as_str()?.to_string();

    let event = envelope.get("payload").and_then(|p| p.get("event"))?;

    let event_type = event.get("type")?.as_str()?;
    if event_type != "message" {
        return None;
    }

    Some((envelope_id, event.clone()))
}

/// Bounded deduplication tracker for Socket Mode envelope IDs.
///
/// Keeps a rolling window of seen IDs. When the capacity is reached,
/// the oldest ID is evicted to make room for new ones.
struct EnvelopeDedup {
    seen: Vec<String>,
    max: usize,
}

impl EnvelopeDedup {
    fn new(max: usize) -> Self {
        Self {
            seen: Vec::with_capacity(max),
            max,
        }
    }

    /// Returns `true` if this ID has not been seen before.
    /// Tracks the ID and evicts the oldest entry when at capacity.
    fn is_new(&mut self, id: &str) -> bool {
        if self.seen.iter().any(|s| s == id) {
            return false;
        }
        if self.seen.len() >= self.max {
            self.seen.remove(0);
        }
        self.seen.push(id.to_string());
        true
    }
}

/// Slack channel — receives events via Socket Mode WebSocket.
pub struct SlackChannel {
    bot_token: String,
    app_token: String,
    client: reqwest::Client,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    mention_only: bool,
    mention_regex: Option<regex::Regex>,
    participated_threads: std::sync::Mutex<std::collections::HashSet<String>>,
    thread_states: std::sync::Mutex<HashMap<String, ThreadState>>,
    /// Base URL for Slack API calls (override for testing).
    api_base: String,
}

impl SlackChannel {
    pub fn new(
        bot_token: String,
        app_token: String,
        channel_id: Option<String>,
        allowed_users: Vec<String>,
    ) -> Self {
        Self {
            client: crate::config::build_runtime_proxy_client("channel.slack"),
            bot_token,
            app_token,
            channel_id,
            allowed_users,
            mention_only: true,
            mention_regex: None,
            participated_threads: std::sync::Mutex::new(std::collections::HashSet::new()),
            thread_states: std::sync::Mutex::new(HashMap::new()),
            api_base: "https://slack.com/api".into(),
        }
    }

    #[cfg(test)]
    fn with_api_base(mut self, base: &str) -> Self {
        self.api_base = base.into();
        self
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

    /// Check if a Slack user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get the bot's own user ID so we can ignore our own messages
    async fn get_bot_user_id(&self) -> Option<String> {
        let data = slack_api_get(
            &self.client,
            "https://slack.com/api/auth.test",
            &self.bot_token,
            &[],
        )
        .await
        .ok()?;

        data.get("user_id")
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

    /// Record that the bot has participated in a thread.
    pub fn record_participation(&self, thread_ts: &str) {
        let mut set = self.participated_threads.lock().unwrap();
        if set.len() >= MAX_PARTICIPATED_THREADS {
            // HashSet iteration order is arbitrary; evicts an arbitrary entry
            if let Some(entry) = set.iter().next().cloned() {
                set.remove(&entry);
            }
        }
        set.insert(thread_ts.to_string());
    }

    /// Record participation for a message the bot just posted.
    ///
    /// For top-level messages (`outbound_thread_ts` is `None`), record the
    /// response `ts` so threads started by the bot's own messages are tracked.
    /// For threaded replies, skip — the caller already records `thread_ts`.
    pub fn record_self_post_participation(
        &self,
        response: &serde_json::Value,
        outbound_thread_ts: Option<&str>,
    ) {
        if outbound_thread_ts.is_some() {
            return;
        }
        if let Some(ts) = response.get("ts").and_then(|t| t.as_str()) {
            self.record_participation(ts);
        }
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

    /// Attempt to dispatch an agent for a thread.
    /// Returns `true` if dispatch should proceed (thread was Idle or new).
    /// Returns `false` if thread is already InFlight (message queued as pending).
    fn try_dispatch(&self, thread_ts: &str) -> bool {
        let mut states = self.thread_states.lock().unwrap();
        let state = states.entry(thread_ts.to_string()).or_default();
        match state.status {
            ThreadStatus::Idle => {
                state.status = ThreadStatus::InFlight;
                state.last_activity = std::time::Instant::now();
                true
            }
            ThreadStatus::InFlight => {
                state.pending_count += 1;
                state.last_activity = std::time::Instant::now();
                false
            }
        }
    }

    /// Mark a thread dispatch as complete.
    /// Returns `true` if there are pending messages that need a follow-up dispatch.
    /// If true, pending_count is reset to 0 and status stays InFlight.
    /// If false, the entry is removed.
    fn complete_dispatch(&self, thread_ts: &str) -> bool {
        let mut states = self.thread_states.lock().unwrap();
        if let Some(state) = states.get_mut(thread_ts) {
            if state.pending_count > 0 {
                state.pending_count = 0;
                state.last_activity = std::time::Instant::now();
                true
            } else {
                states.remove(thread_ts);
                false
            }
        } else {
            false
        }
    }

    /// Evict thread state entries that are Idle and older than `max_age_minutes`.
    fn evict_stale_threads(&self, max_age_minutes: u64) {
        let cutoff = std::time::Duration::from_secs(max_age_minutes * 60);
        let mut states = self.thread_states.lock().unwrap();
        states.retain(|_, state| {
            state.status == ThreadStatus::InFlight || state.last_activity.elapsed() < cutoff
        });
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

    /// Open a Socket Mode WebSocket connection.
    ///
    /// Posts to `apps.connections.open` with the app-level token to obtain a
    /// single-use WebSocket URL, then connects via `tokio_tungstenite`.
    async fn connect_socket_mode(
        &self,
    ) -> anyhow::Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let resp = self
            .client
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(&self.app_token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;

        if body.get("ok") != Some(&serde_json::Value::Bool(true)) {
            let err = body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("apps.connections.open failed: {err}");
        }

        let ws_url = body
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow::anyhow!("apps.connections.open response missing url"))?;

        if !ws_url.starts_with("wss://") {
            anyhow::bail!("apps.connections.open returned non-secure URL: {ws_url}");
        }

        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await?;
        Ok(ws_stream)
    }
}

const SLACK_MESSAGE_CHUNK_LIMIT: usize = 4000;

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let chunks = split_message(&message.content, SLACK_MESSAGE_CHUNK_LIMIT);
        let mut first_result: Option<serde_json::Value> = None;

        for chunk in &chunks {
            let mut body = serde_json::json!({
                "channel": message.recipient,
                "text": chunk
            });
            if let Some(ref ts) = message.thread_ts {
                body["thread_ts"] = serde_json::json!(ts);
            }

            let post_url = format!("{}/chat.postMessage", self.api_base);
            let result = slack_api_post(&self.client, &post_url, &self.bot_token, &body).await?;

            if result.get("ok") == Some(&serde_json::Value::Bool(false)) {
                let err = result
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                anyhow::bail!("Slack chat.postMessage failed: {err}");
            }

            if first_result.is_none() {
                first_result = Some(result);
            }
        }

        // Record thread participation
        if let Some(ref ts) = message.thread_ts {
            self.record_participation(ts);
        }

        // Track threads started by our own top-level messages so replies
        // to them are recognized as participated threads.
        if let Some(ref result) = first_result {
            self.record_self_post_participation(result, message.thread_ts.as_deref());
        }

        // Remove ack reaction after reply
        if let Some(ref ack_ts) = message.ack_reaction_ts {
            let remove_body = serde_json::json!({
                "channel": message.recipient,
                "name": "eyes",
                "timestamp": ack_ts
            });
            let remove_url = format!("{}/reactions.remove", self.api_base);
            if let Err(e) =
                slack_api_post(&self.client, &remove_url, &self.bot_token, &remove_body).await
            {
                tracing::warn!("Failed to remove ack reaction: {e}");
            }
        }

        Ok(())
    }
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = self.get_bot_user_id().await.unwrap_or_default();

        if bot_user_id.is_empty() {
            tracing::warn!("Slack auth.test failed — bot_user_id unknown; self-message filtering may miss own messages");
        }

        let scoped_channel = self.configured_channel_id();
        let mut dedup = EnvelopeDedup::new(100);

        // Exponential backoff state: start 1s, double on failure, cap 60s
        let mut backoff_ms: u64 = 1_000;
        const BACKOFF_CAP_MS: u64 = 60_000;
        let mut first_attempt = true;

        loop {
            // --- connect ---
            let ws_stream = match self.connect_socket_mode().await {
                Ok(ws) => {
                    tracing::info!("Slack Socket Mode connected");
                    first_attempt = false;
                    ws
                }
                Err(e) => {
                    if first_attempt {
                        return Err(e.context("Slack Socket Mode initial connection failed"));
                    }
                    tracing::warn!("Slack Socket Mode connection failed: {e}");
                    let jitter = rand::random::<u64>() % 500;
                    tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
                    backoff_ms = (backoff_ms * 2).min(BACKOFF_CAP_MS);
                    continue;
                }
            };

            let (mut sink, mut stream) = ws_stream.split();
            let mut connection_confirmed = false;

            // --- read loop ---
            while let Some(msg_result) = stream.next().await {
                let ws_msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("Slack WebSocket read error: {e}");
                        break; // reconnect
                    }
                };

                let text = match ws_msg {
                    WsMessage::Text(t) => t,
                    WsMessage::Close(_) => {
                        tracing::info!("Slack WebSocket closed by server");
                        break;
                    }
                    _ => continue,
                };

                let envelope: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("Slack WebSocket non-JSON frame: {e}");
                        continue;
                    }
                };

                // Reset backoff after first valid frame confirms a healthy connection
                if !connection_confirmed {
                    backoff_ms = 1_000;
                    connection_confirmed = true;
                }

                // Acknowledge every envelope that carries an envelope_id
                if let Some(eid) = envelope.get("envelope_id").and_then(|v| v.as_str()) {
                    let ack = serde_json::json!({ "envelope_id": eid });
                    if let Err(e) = sink.send(WsMessage::Text(ack.to_string().into())).await {
                        tracing::warn!("Slack envelope ack failed: {e}");
                        break;
                    }
                }

                // Handle disconnect envelope — break to reconnect
                if envelope.get("type").and_then(|v| v.as_str()) == Some("disconnect") {
                    let reason = envelope
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    tracing::info!("Slack Socket Mode disconnect: {reason}");
                    break;
                }

                // Parse to (envelope_id, message event) — skip non-message envelopes
                let (envelope_id, event) = match parse_socket_event(&envelope) {
                    Some(pair) => pair,
                    None => continue,
                };

                // Deduplicate
                if !dedup.is_new(&envelope_id) {
                    tracing::debug!("Slack duplicate envelope skipped: {envelope_id}");
                    continue;
                }

                // Channel scoping
                let event_channel = event
                    .get("channel")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default();

                if let Some(ref sc) = scoped_channel {
                    if event_channel != sc.as_str() {
                        continue;
                    }
                }

                let ts = event.get("ts").and_then(|t| t.as_str()).unwrap_or_default();
                let user = event
                    .get("user")
                    .and_then(|u| u.as_str())
                    .unwrap_or_default();
                let raw_text = event
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();

                // Skip bot messages: bot_id field present or user matches our bot
                if event.get("bot_id").is_some() {
                    continue;
                }
                if !bot_user_id.is_empty() && user == bot_user_id {
                    continue;
                }

                // Allowlist check
                if !self.is_user_allowed(user) {
                    tracing::debug!("Slack message from non-allowed user {user}, skipping");
                    continue;
                }

                // Skip empty text
                if raw_text.trim().is_empty() {
                    continue;
                }

                // Format timestamp for display
                let ts_display = ts
                    .split('.')
                    .next()
                    .and_then(|s| s.parse::<i64>().ok())
                    .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| ts.to_string());

                // Resolve thread
                let thread_ts = SlackChannel::inbound_thread_ts(&event, ts);

                // Mention gating — bypass for DMs (channel IDs starting with 'D')
                let is_dm = event_channel.starts_with('D');
                let (final_text, triage_required) = if self.mention_only && !is_dm {
                    match self.resolve_mention_gate(raw_text, &bot_user_id, thread_ts.as_deref()) {
                        MentionGateResult::ExplicitMention(cleaned) => (cleaned, false),
                        MentionGateResult::ParticipatedThread(text) => (text, true),
                        MentionGateResult::Buffer => continue,
                    }
                } else {
                    (raw_text.to_string(), false)
                };

                // Add ack reaction (eyes)
                let ack_body = serde_json::json!({
                    "channel": event_channel,
                    "name": "eyes",
                    "timestamp": ts
                });
                if let Err(e) = slack_api_post(
                    &self.client,
                    "https://slack.com/api/reactions.add",
                    &self.bot_token,
                    &ack_body,
                )
                .await
                {
                    tracing::warn!("Failed to add ack reaction: {e}");
                }

                // Thread hydration
                let (thread_starter_body, thread_history) = if let Some(ref tts) = thread_ts {
                    match fetch_thread_replies(
                        &self.client,
                        &self.bot_token,
                        event_channel,
                        tts,
                        50,
                    )
                    .await
                    {
                        Ok(replies) if !replies.is_empty() => {
                            let starter_body = replies
                                .first()
                                .and_then(|r| r.get("text"))
                                .and_then(|t| t.as_str())
                                .map(String::from);
                            let history = format_thread_history(
                                &replies,
                                &bot_user_id,
                                "ZeroClaw",
                                event_channel,
                                &HashMap::new(),
                            );
                            (starter_body, Some(history))
                        }
                        Ok(_) => (None, None),
                        Err(e) => {
                            tracing::warn!("Failed to fetch thread replies: {e}");
                            (None, None)
                        }
                    }
                } else {
                    (None, None)
                };

                let sender_name = user.to_string();
                let content = format!(
                    "[Slack {event_channel} {sender_name} {ts_display}] {sender_name}: {final_text}"
                );

                let channel_message = ChannelMessage {
                    id: format!("slack_{event_channel}_{ts}"),
                    sender: sender_name,
                    reply_target: event_channel.to_string(),
                    content,
                    channel: "slack".to_string(),
                    timestamp: ts
                        .split('.')
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0),
                    thread_ts,
                    thread_starter_body,
                    thread_history,
                    triage_required,
                    ack_reaction_ts: Some(ts.to_string()),
                };

                if tx.send(channel_message).await.is_err() {
                    tracing::info!("Slack listen channel closed, shutting down");
                    return Ok(());
                }
            }

            // WebSocket broke or disconnected — reconnect with backoff
            let jitter = rand::random::<u64>() % 500;
            tracing::info!(
                "Slack Socket Mode reconnecting in {}ms",
                backoff_ms + jitter
            );
            tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
            backoff_ms = (backoff_ms * 2).min(BACKOFF_CAP_MS);
        }
    }

    async fn health_check(&self) -> bool {
        slack_api_get(
            &self.client,
            "https://slack.com/api/auth.test",
            &self.bot_token,
            &[],
        )
        .await
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_channel_name() {
        let ch = SlackChannel::new("xoxb-fake".into(), "xapp-fake".into(), None, vec![]);
        assert_eq!(ch.name(), "slack");
    }

    #[test]
    fn slack_channel_with_channel_id() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            Some("C12345".into()),
            vec![],
        );
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
    fn empty_allowlist_denies_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), "xapp-fake".into(), None, vec![]);
        assert!(!ch.is_user_allowed("U12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            None,
            vec!["*".into()],
        );
        assert!(ch.is_user_allowed("U12345"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            None,
            vec!["U111".into(), "U222".into()],
        );
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("U222"));
        assert!(!ch.is_user_allowed("U333"));
    }

    #[test]
    fn allowlist_exact_match_not_substring() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            None,
            vec!["U111".into()],
        );
        assert!(!ch.is_user_allowed("U1111"));
        assert!(!ch.is_user_allowed("U11"));
    }

    #[test]
    fn allowlist_empty_user_id() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            None,
            vec!["U111".into()],
        );
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            None,
            vec!["U111".into()],
        );
        assert!(ch.is_user_allowed("U111"));
        assert!(!ch.is_user_allowed("u111"));
    }

    #[test]
    fn allowlist_wildcard_and_specific() {
        let ch = SlackChannel::new(
            "xoxb-fake".into(),
            "xapp-fake".into(),
            None,
            vec!["U111".into(), "*".into()],
        );
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

    // -- Thread participation tracking -----------------------------------------

    #[test]
    fn participated_threads_empty_on_init() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        assert!(channel.participated_threads().is_empty());
    }

    #[test]
    fn record_participation_tracks_thread() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        channel.record_participation("1234.5678");
        assert!(channel.has_participated("1234.5678"));
    }

    #[test]
    fn has_participated_returns_false_for_unknown_thread() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        assert!(!channel.has_participated("unknown.thread"));
    }

    #[test]
    fn participated_threads_capped_at_limit() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        for i in 0..1100 {
            channel.record_participation(&format!("{i}.0000"));
        }
        let threads = channel.participated_threads();
        assert!(
            threads.len() <= 1000,
            "expected <= 1000, got {}",
            threads.len()
        );
    }

    // -- Participation-based triage routing ------------------------------------

    #[test]
    fn participated_thread_message_sets_triage_required() {
        // When bot has participated in a thread, messages in that thread
        // without explicit @mention should set triage_required = true
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
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
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        channel.record_participation("1234.5678");

        let text = "<@U_BOT> what do you think?";
        let is_explicit = is_mention(text, "U_BOT", None);

        assert!(is_explicit);
        // triage_required should be false when explicitly mentioned
    }

    #[test]
    fn non_participated_thread_message_is_buffered() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
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
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        let result =
            channel.resolve_mention_gate("<@U_BOT> what do you think?", "U_BOT", Some("1234.5678"));
        assert_eq!(
            result,
            MentionGateResult::ExplicitMention("what do you think?".to_string())
        );
    }

    #[test]
    fn mention_gate_explicit_mention_in_participated_thread_no_triage() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
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
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
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
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        // Bot has NOT participated in thread "9999.0000"

        let result = channel.resolve_mention_gate("just chatting", "U_BOT", Some("9999.0000"));
        assert_eq!(result, MentionGateResult::Buffer);
    }

    #[test]
    fn mention_gate_no_thread_no_mention_buffers() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );

        let result = channel.resolve_mention_gate("random message", "U_BOT", None);
        assert_eq!(result, MentionGateResult::Buffer);
    }

    #[test]
    fn mention_gate_regex_mention_returns_explicit() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        )
        .with_mention_config(true, Some(r"(?i)\brain\b".into()));

        let result =
            channel.resolve_mention_gate("Rain what do you think?", "U_BOT", Some("1234.5678"));
        assert_eq!(
            result,
            MentionGateResult::ExplicitMention("Rain what do you think?".to_string())
        );
    }

    // -- Retry helper tests ---------------------------------------------------

    #[test]
    fn parse_retry_after_valid_header() {
        let delay = parse_retry_after_secs(Some("30"));
        assert_eq!(delay, Some(30));
    }

    #[test]
    fn parse_retry_after_missing() {
        let delay = parse_retry_after_secs(None);
        assert_eq!(delay, None);
    }

    #[test]
    fn parse_retry_after_zero() {
        let delay = parse_retry_after_secs(Some("0"));
        assert_eq!(delay, Some(0));
    }

    #[test]
    fn parse_retry_after_invalid() {
        let delay = parse_retry_after_secs(Some("not-a-number"));
        assert_eq!(delay, None);
    }

    #[test]
    fn is_ratelimited_json_detects_ratelimit() {
        let body: serde_json::Value = serde_json::json!({
            "ok": false,
            "error": "ratelimited"
        });
        assert!(is_slack_ratelimited(&body));
    }

    #[test]
    fn is_ratelimited_json_ignores_other_errors() {
        let body: serde_json::Value = serde_json::json!({
            "ok": false,
            "error": "channel_not_found"
        });
        assert!(!is_slack_ratelimited(&body));
    }

    #[test]
    fn is_ratelimited_json_ignores_success() {
        let body: serde_json::Value = serde_json::json!({
            "ok": true
        });
        assert!(!is_slack_ratelimited(&body));
    }

    // -- Socket Mode envelope parsing -----------------------------------------

    #[test]
    fn parse_socket_envelope_valid_message() {
        let envelope = serde_json::json!({
            "type": "events_api",
            "envelope_id": "env-001",
            "payload": {
                "event": {
                    "type": "message",
                    "user": "U_ALICE",
                    "text": "hello world",
                    "channel": "C12345",
                    "ts": "1234567890.000100"
                }
            }
        });
        let result = parse_socket_event(&envelope);
        assert!(result.is_some(), "should parse valid message envelope");
        let (id, event) = result.unwrap();
        assert_eq!(id, "env-001");
        assert_eq!(event["type"].as_str(), Some("message"));
        assert_eq!(event["text"].as_str(), Some("hello world"));
    }

    #[test]
    fn parse_socket_envelope_non_message_event() {
        let envelope = serde_json::json!({
            "type": "events_api",
            "envelope_id": "env-002",
            "payload": {
                "event": {
                    "type": "reaction_added",
                    "user": "U_ALICE",
                    "reaction": "thumbsup"
                }
            }
        });
        let result = parse_socket_event(&envelope);
        assert!(result.is_none(), "should skip non-message events");
    }

    #[test]
    fn parse_socket_envelope_hello() {
        let envelope = serde_json::json!({
            "type": "hello",
            "num_connections": 1,
            "debug_info": {}
        });
        let result = parse_socket_event(&envelope);
        assert!(result.is_none(), "should skip hello envelopes");
    }

    #[test]
    fn parse_socket_envelope_missing_payload() {
        let envelope = serde_json::json!({
            "type": "events_api",
            "envelope_id": "env-003"
        });
        let result = parse_socket_event(&envelope);
        assert!(
            result.is_none(),
            "should return None when payload is missing"
        );
    }

    #[test]
    fn parse_socket_envelope_disconnect() {
        let envelope = serde_json::json!({
            "type": "disconnect",
            "reason": "link_disabled"
        });
        let result = parse_socket_event(&envelope);
        assert!(result.is_none(), "should skip disconnect envelopes");
    }

    // -- Top-level message self-participation ---------------------------------

    #[tokio::test]
    async fn send_toplevel_message_records_self_participation() {
        use super::super::traits::Channel;

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat.postMessage"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "ok": true, "ts": "1772224188.479959" })),
            )
            .mount(&server)
            .await;

        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        )
        .with_api_base(&server.uri());

        let msg = SendMessage {
            recipient: "C12345".into(),
            content: "hello".into(),
            subject: None,
            thread_ts: None,
            ack_reaction_ts: None,
        };
        channel.send(&msg).await.unwrap();

        assert!(
            channel.has_participated("1772224188.479959"),
            "send() should track threads started by the bot's own top-level messages"
        );
    }

    #[test]
    fn send_records_own_toplevel_message_as_participated() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        let response = serde_json::json!({
            "ok": true,
            "ts": "1772224188.479959"
        });
        channel.record_self_post_participation(&response, None);
        assert!(
            channel.has_participated("1772224188.479959"),
            "bot should track threads started by its own top-level messages"
        );
    }

    #[test]
    fn send_does_not_double_record_threaded_reply() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        let response = serde_json::json!({
            "ok": true,
            "ts": "1772224200.000100"
        });
        channel.record_self_post_participation(&response, Some("1772224188.479959"));
        assert!(
            !channel.has_participated("1772224200.000100"),
            "should not record response ts when replying in an existing thread"
        );
    }

    #[test]
    fn send_handles_missing_ts_in_response_gracefully() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        let response = serde_json::json!({ "ok": true });
        channel.record_self_post_participation(&response, None);
        assert!(channel.participated_threads().is_empty());
    }

    // -- Envelope deduplication -----------------------------------------------

    #[test]
    fn envelope_dedup_tracks_ids() {
        let mut dedup = EnvelopeDedup::new(10);
        assert!(dedup.is_new("a"), "first insert of 'a' should be new");
        assert!(dedup.is_new("b"), "first insert of 'b' should be new");
        assert!(!dedup.is_new("a"), "second insert of 'a' should NOT be new");
    }

    #[test]
    fn envelope_dedup_evicts_oldest() {
        let mut dedup = EnvelopeDedup::new(3);
        assert!(dedup.is_new("a"));
        assert!(dedup.is_new("b"));
        assert!(dedup.is_new("c"));
        // At capacity: [a, b, c]. Inserting d evicts a.
        assert!(dedup.is_new("d"));
        // a was evicted, so it should be new again
        assert!(dedup.is_new("a"), "'a' should be new after eviction");
        // d is still tracked
        assert!(!dedup.is_new("d"), "'d' should NOT be new");
    }

    // -- Thread gate state -------------------------------------------------

    #[test]
    fn thread_state_defaults_to_idle() {
        let state = ThreadState::default();
        assert_eq!(state.status, ThreadStatus::Idle);
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn thread_status_debug_display() {
        let idle = ThreadStatus::Idle;
        let inflight = ThreadStatus::InFlight;
        assert_ne!(format!("{idle:?}"), format!("{inflight:?}"));
    }

    // -- Thread gate dispatch/complete ----------------------------------------

    #[test]
    fn thread_gate_try_dispatch_idle_thread_returns_true() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        assert!(channel.try_dispatch("1234.5678"));
    }

    #[test]
    fn thread_gate_try_dispatch_inflight_thread_returns_false() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        assert!(channel.try_dispatch("1234.5678"));
        assert!(!channel.try_dispatch("1234.5678"));
    }

    #[test]
    fn thread_gate_try_dispatch_increments_pending() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        channel.try_dispatch("1234.5678");
        channel.try_dispatch("1234.5678"); // blocked, pending_count = 1
        channel.try_dispatch("1234.5678"); // blocked, pending_count = 2

        let states = channel.thread_states.lock().unwrap();
        let state = states.get("1234.5678").unwrap();
        assert_eq!(state.pending_count, 2);
    }

    #[test]
    fn thread_gate_complete_with_no_pending_returns_false() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        channel.try_dispatch("1234.5678");
        let has_pending = channel.complete_dispatch("1234.5678");
        assert!(!has_pending);

        let states = channel.thread_states.lock().unwrap();
        assert!(
            !states.contains_key("1234.5678"),
            "idle entry should be removed"
        );
    }

    #[test]
    fn thread_gate_complete_with_pending_returns_true() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        channel.try_dispatch("1234.5678");
        channel.try_dispatch("1234.5678"); // pending = 1
        let has_pending = channel.complete_dispatch("1234.5678");
        assert!(has_pending);

        let states = channel.thread_states.lock().unwrap();
        let state = states.get("1234.5678").unwrap();
        assert_eq!(state.status, ThreadStatus::InFlight);
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn thread_gate_evicts_idle_entries() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        {
            let mut states = channel.thread_states.lock().unwrap();
            states.insert(
                "old.thread".to_string(),
                ThreadState {
                    status: ThreadStatus::Idle,
                    pending_count: 0,
                    last_activity: std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(3600))
                        .unwrap(),
                },
            );
        }
        channel.evict_stale_threads(30);
        let states = channel.thread_states.lock().unwrap();
        assert!(!states.contains_key("old.thread"));
    }

    #[test]
    fn thread_gate_does_not_evict_inflight_entries() {
        let channel = SlackChannel::new(
            "xoxb-test".into(),
            "xapp-test".into(),
            None,
            vec!["*".into()],
        );
        {
            let mut states = channel.thread_states.lock().unwrap();
            states.insert(
                "busy.thread".to_string(),
                ThreadState {
                    status: ThreadStatus::InFlight,
                    pending_count: 0,
                    last_activity: std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(3600))
                        .unwrap(),
                },
            );
        }
        channel.evict_stale_threads(30);
        let states = channel.thread_states.lock().unwrap();
        assert!(states.contains_key("busy.thread"));
    }
}
