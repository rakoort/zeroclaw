pub mod tools;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use uuid::Uuid;

/// Represents an active watch that monitors for incoming messages matching
/// specific criteria, enabling the agent to react to asynchronous events.
pub struct Watch {
    pub id: String,
    pub event_type: String,
    pub match_user_id: Option<String>,
    pub match_channel_id: Option<String>,
    pub match_thread_ts: Option<String>,
    pub context: String,
    pub reminder_after_minutes: Option<i64>,
    pub reminder_message: Option<String>,
    pub expires_minutes: Option<i64>,
    pub on_expire: Option<String>,
    pub channel_name: String,
    pub created_at: i64,
    pub status: String,
}

/// Input struct for registering a new watch (excludes auto-generated fields).
pub struct NewWatch {
    pub event_type: String,
    pub match_user_id: Option<String>,
    pub match_channel_id: Option<String>,
    pub match_thread_ts: Option<String>,
    pub context: String,
    pub reminder_after_minutes: Option<i64>,
    pub reminder_message: Option<String>,
    pub expires_minutes: Option<i64>,
    pub on_expire: Option<String>,
    pub channel_name: String,
}

/// SQLite-backed store for managing event watches.
pub struct WatchStore {
    pub conn: Connection,
}

impl WatchStore {
    /// Creates the watches table if it does not exist.
    pub fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watches (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                match_user_id TEXT,
                match_channel_id TEXT,
                match_thread_ts TEXT,
                context TEXT NOT NULL,
                reminder_after_minutes INTEGER,
                reminder_message TEXT,
                expires_minutes INTEGER,
                on_expire TEXT,
                channel_name TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'active'
            )",
        )
        .context("Failed to create watches table")?;
        Ok(())
    }

    /// Inserts a new watch and returns its generated ID.
    pub fn register(&self, watch: &NewWatch) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        self.conn
            .execute(
                "INSERT INTO watches (
                    id, event_type, match_user_id, match_channel_id, match_thread_ts,
                    context, reminder_after_minutes, reminder_message, expires_minutes,
                    on_expire, channel_name, created_at, status
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'active')",
                params![
                    id,
                    watch.event_type,
                    watch.match_user_id,
                    watch.match_channel_id,
                    watch.match_thread_ts,
                    watch.context,
                    watch.reminder_after_minutes,
                    watch.reminder_message,
                    watch.expires_minutes,
                    watch.on_expire,
                    watch.channel_name,
                    now,
                ],
            )
            .context("Failed to insert watch")?;
        Ok(id)
    }

    /// Returns all watches with status "active".
    pub fn active_watches(&self) -> Vec<Watch> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, event_type, match_user_id, match_channel_id, match_thread_ts,
                        context, reminder_after_minutes, reminder_message, expires_minutes,
                        on_expire, channel_name, created_at, status
                 FROM watches WHERE status = 'active'",
            )
            .expect("Failed to prepare active_watches query");
        let rows = stmt
            .query_map([], |row| {
                Ok(Watch {
                    id: row.get(0)?,
                    event_type: row.get(1)?,
                    match_user_id: row.get(2)?,
                    match_channel_id: row.get(3)?,
                    match_thread_ts: row.get(4)?,
                    context: row.get(5)?,
                    reminder_after_minutes: row.get(6)?,
                    reminder_message: row.get(7)?,
                    expires_minutes: row.get(8)?,
                    on_expire: row.get(9)?,
                    channel_name: row.get(10)?,
                    created_at: row.get(11)?,
                    status: row.get(12)?,
                })
            })
            .expect("Failed to query active watches");
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Finds the first active watch matching the given message attributes.
    ///
    /// A watch matches when its `channel_name` equals the provided channel,
    /// and each non-NULL filter field (`match_user_id`, `match_channel_id`,
    /// `match_thread_ts`) equals the corresponding argument. NULL filter
    /// fields are treated as wildcards.
    pub fn check_message(
        &self,
        sender_id: &str,
        channel_id: &str,
        thread_ts: Option<&str>,
        channel_name: &str,
    ) -> Option<Watch> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, event_type, match_user_id, match_channel_id, match_thread_ts,
                        context, reminder_after_minutes, reminder_message, expires_minutes,
                        on_expire, channel_name, created_at, status
                 FROM watches
                 WHERE status = 'active' AND channel_name = ?1
                   AND (match_user_id IS NULL OR match_user_id = ?2)
                   AND (match_channel_id IS NULL OR match_channel_id = ?3)
                   AND (match_thread_ts IS NULL OR match_thread_ts = ?4)
                 LIMIT 1",
            )
            .ok()?;
        stmt.query_row(
            params![channel_name, sender_id, channel_id, thread_ts],
            |row| {
                Ok(Watch {
                    id: row.get(0)?,
                    event_type: row.get(1)?,
                    match_user_id: row.get(2)?,
                    match_channel_id: row.get(3)?,
                    match_thread_ts: row.get(4)?,
                    context: row.get(5)?,
                    reminder_after_minutes: row.get(6)?,
                    reminder_message: row.get(7)?,
                    expires_minutes: row.get(8)?,
                    on_expire: row.get(9)?,
                    channel_name: row.get(10)?,
                    created_at: row.get(11)?,
                    status: row.get(12)?,
                })
            },
        )
        .ok()
    }

    /// Updates a watch's status to "matched".
    pub fn mark_matched(&self, id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE watches SET status = 'matched' WHERE id = ?1",
                params![id],
            )
            .context("Failed to mark watch as matched")?;
        Ok(())
    }

    /// Updates a watch's status to "expired".
    pub fn mark_expired(&self, id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE watches SET status = 'expired' WHERE id = ?1",
                params![id],
            )
            .context("Failed to mark watch as expired")?;
        Ok(())
    }

    /// Updates a watch's status to "cancelled".
    pub fn cancel(&self, id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE watches SET status = 'cancelled' WHERE id = ?1",
                params![id],
            )
            .context("Failed to cancel watch")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WatchManager — async timer management layer over WatchStore
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::channels::traits::ChannelMessage;

/// Wraps [`WatchStore`] with async timer management. When a watch is registered
/// with `reminder_after_minutes` or `expires_minutes`, per-watch tokio tasks
/// are spawned that sleep until the trigger time, then inject a synthetic
/// [`ChannelMessage`] into the message channel.
pub struct WatchManager {
    store: Arc<Mutex<WatchStore>>,
    pub(crate) timers: Mutex<HashMap<String, CancellationToken>>,
    channel_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
}

impl WatchManager {
    /// Creates a new `WatchManager` wrapping the given store and message sender.
    pub fn new(
        store: Arc<Mutex<WatchStore>>,
        channel_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Self {
        Self {
            store,
            timers: Mutex::new(HashMap::new()),
            channel_tx,
        }
    }

    /// Registers a new watch and spawns timer tasks for reminder/expiry if configured.
    pub async fn register(&self, watch: NewWatch) -> Result<String> {
        let id = {
            let store = self.store.lock().await;
            store.register(&watch)?
        };

        let token = CancellationToken::new();

        // Spawn reminder timer
        if let Some(reminder_mins) = watch.reminder_after_minutes {
            let child_token = token.child_token();
            let tx = self.channel_tx.clone();
            let channel_name = watch.channel_name.clone();
            let reminder_msg = watch
                .reminder_message
                .clone()
                .unwrap_or_else(|| "Reminder triggered".to_string());
            let watch_id = id.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = child_token.cancelled() => {}
                    () = tokio::time::sleep(Duration::from_secs(reminder_mins.cast_unsigned() * 60)) => {
                        let content = format!("[WATCH REMINDER] {reminder_msg}");
                        let msg = ChannelMessage {
                            id: format!("watch-reminder-{watch_id}"),
                            sender: "system".to_string(),
                            reply_target: "system".to_string(),
                            content,
                            channel: channel_name,
                            timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
                            thread_ts: None,
                            thread_starter_body: None,
                            thread_history: None,
                            triage_required: false,
                            ack_reaction_ts: None,
                        };
                        let _ = tx.send(msg).await;
                    }
                }
            });
        }

        // Spawn expiry timer
        if let Some(expire_mins) = watch.expires_minutes {
            let child_token = token.child_token();
            let store = Arc::clone(&self.store);
            let tx = self.channel_tx.clone();
            let channel_name = watch.channel_name.clone();
            let on_expire = watch.on_expire.clone();
            let watch_id = id.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = child_token.cancelled() => {}
                    () = tokio::time::sleep(Duration::from_secs(expire_mins.cast_unsigned() * 60)) => {
                        {
                            let store = store.lock().await;
                            let _ = store.mark_expired(&watch_id);
                        }
                        if let Some(expire_msg) = on_expire {
                            let content = format!("[WATCH EXPIRED] {expire_msg}");
                            let msg = ChannelMessage {
                                id: format!("watch-expired-{watch_id}"),
                                sender: "system".to_string(),
                                reply_target: "system".to_string(),
                                content,
                                channel: channel_name,
                                timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
                                thread_ts: None,
                                thread_starter_body: None,
                                thread_history: None,
                                triage_required: false,
                                ack_reaction_ts: None,
                            };
                            let _ = tx.send(msg).await;
                        }
                    }
                }
            });
        }

        self.timers.lock().await.insert(id.clone(), token);
        Ok(id)
    }

    /// Checks whether an incoming message matches any active watch.
    /// If matched, marks the watch and cancels its timers.
    pub async fn check_message(
        &self,
        sender_id: &str,
        channel_id: &str,
        thread_ts: Option<&str>,
        channel_name: &str,
    ) -> Option<Watch> {
        let watch = {
            let store = self.store.lock().await;
            store.check_message(sender_id, channel_id, thread_ts, channel_name)?
        };

        // Mark as matched and cancel timers
        {
            let store = self.store.lock().await;
            let _ = store.mark_matched(&watch.id);
        }
        self.cancel_timer(&watch.id).await;

        Some(watch)
    }

    /// Cancels an active watch and its associated timers.
    pub async fn cancel(&self, id: &str) -> Result<()> {
        {
            let store = self.store.lock().await;
            store.cancel(id)?;
        }
        self.cancel_timer(id).await;
        Ok(())
    }

    /// Returns all active watches from the store.
    pub async fn active_watches(&self) -> Vec<Watch> {
        let store = self.store.lock().await;
        store.active_watches()
    }

    /// Re-spawns timer tasks for all active watches that have timer fields set.
    /// Call this on daemon restart to resume timers for watches that were
    /// persisted before the previous shutdown.
    pub async fn init(&self) -> Result<()> {
        let watches = {
            let store = self.store.lock().await;
            store.active_watches()
        };

        for watch in watches {
            let remaining_reminder =
                Self::remaining_duration(watch.created_at, watch.reminder_after_minutes);
            let remaining_expiry =
                Self::remaining_duration(watch.created_at, watch.expires_minutes);

            let has_reminder = remaining_reminder.map_or(false, |d| !d.is_zero());
            let has_expiry = remaining_expiry.map_or(false, |d| !d.is_zero());

            if !has_reminder && !has_expiry {
                continue;
            }

            let token = CancellationToken::new();

            if let Some(duration) = remaining_reminder {
                if !duration.is_zero() {
                    let child_token = token.child_token();
                    let tx = self.channel_tx.clone();
                    let channel_name = watch.channel_name.clone();
                    let reminder_msg = watch
                        .reminder_message
                        .clone()
                        .unwrap_or_else(|| "Reminder triggered".to_string());
                    let watch_id = watch.id.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            () = child_token.cancelled() => {}
                            () = tokio::time::sleep(duration) => {
                                let content = format!("[WATCH REMINDER] {reminder_msg}");
                                let msg = ChannelMessage {
                                    id: format!("watch-reminder-{watch_id}"),
                                    sender: "system".to_string(),
                                    reply_target: "system".to_string(),
                                    content,
                                    channel: channel_name,
                                    timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
                                    thread_ts: None,
                                    thread_starter_body: None,
                                    thread_history: None,
                                    triage_required: false,
                                    ack_reaction_ts: None,
                                };
                                let _ = tx.send(msg).await;
                            }
                        }
                    });
                }
            }

            if let Some(duration) = remaining_expiry {
                if !duration.is_zero() {
                    let child_token = token.child_token();
                    let store = Arc::clone(&self.store);
                    let tx = self.channel_tx.clone();
                    let channel_name = watch.channel_name.clone();
                    let on_expire = watch.on_expire.clone();
                    let watch_id = watch.id.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            () = child_token.cancelled() => {}
                            () = tokio::time::sleep(duration) => {
                                {
                                    let store = store.lock().await;
                                    let _ = store.mark_expired(&watch_id);
                                }
                                if let Some(expire_msg) = on_expire {
                                    let content = format!("[WATCH EXPIRED] {expire_msg}");
                                    let msg = ChannelMessage {
                                        id: format!("watch-expired-{watch_id}"),
                                        sender: "system".to_string(),
                                        reply_target: "system".to_string(),
                                        content,
                                        channel: channel_name,
                                        timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
                                        thread_ts: None,
                                        thread_starter_body: None,
                                        thread_history: None,
                                        triage_required: false,
                                        ack_reaction_ts: None,
                                    };
                                    let _ = tx.send(msg).await;
                                }
                            }
                        }
                    });
                }
            }

            self.timers.lock().await.insert(watch.id.clone(), token);
        }

        Ok(())
    }

    /// Cancels the timer for a watch and removes it from the timers map.
    async fn cancel_timer(&self, id: &str) {
        let mut timers = self.timers.lock().await;
        if let Some(token) = timers.remove(id) {
            token.cancel();
        }
    }

    /// Calculates the remaining duration for a timer based on creation time and
    /// the configured minutes. Returns `None` if the field is not set, or
    /// `Duration::ZERO` if the time has already passed.
    fn remaining_duration(created_at: i64, minutes: Option<i64>) -> Option<Duration> {
        let mins = minutes?;
        let target = created_at + (mins * 60);
        let now = chrono::Utc::now().timestamp();
        if target <= now {
            Some(Duration::ZERO)
        } else {
            Some(Duration::from_secs((target - now).cast_unsigned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> WatchStore {
        let conn = Connection::open_in_memory().unwrap();
        WatchStore::init_schema(&conn).unwrap();
        WatchStore { conn }
    }

    fn sample_watch() -> NewWatch {
        NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U_TEST_001".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "Waiting for reply".into(),
            reminder_after_minutes: Some(30),
            reminder_message: Some("Check in".into()),
            expires_minutes: Some(60),
            on_expire: Some("Timed out".into()),
            channel_name: "slack".into(),
        }
    }

    #[test]
    fn mark_expired_removes_from_active() {
        let store = test_store();
        let id = store.register(&sample_watch()).unwrap();
        assert_eq!(store.active_watches().len(), 1);

        store.mark_expired(&id).unwrap();

        assert!(store.active_watches().is_empty());
    }

    fn test_manager_store() -> std::sync::Arc<tokio::sync::Mutex<WatchStore>> {
        let conn = Connection::open_in_memory().unwrap();
        WatchStore::init_schema(&conn).unwrap();
        std::sync::Arc::new(tokio::sync::Mutex::new(WatchStore { conn }))
    }

    #[tokio::test]
    async fn watch_manager_register_and_check() {
        let store = test_manager_store();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let manager = WatchManager::new(store, tx);

        let id = manager
            .register(NewWatch {
                event_type: "dm_reply".into(),
                match_user_id: Some("U_TEST_001".into()),
                match_channel_id: None,
                match_thread_ts: None,
                context: "Waiting for reply".into(),
                reminder_after_minutes: Some(30),
                reminder_message: Some("Check in".into()),
                expires_minutes: Some(60),
                on_expire: Some("Timed out".into()),
                channel_name: "slack".into(),
            })
            .await
            .unwrap();

        assert!(!id.is_empty());

        // check_message should find and match the watch
        let matched = manager
            .check_message("U_TEST_001", "D123", None, "slack")
            .await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().id, id);

        // After match, active_watches should be empty
        let active = manager.active_watches().await;
        assert!(active.is_empty());

        // Timer should have been cancelled on match
        let timers = manager.timers.lock().await;
        assert!(!timers.contains_key(&id));
    }

    #[tokio::test]
    async fn watch_manager_cancel_kills_timers() {
        let store = test_manager_store();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let manager = WatchManager::new(store, tx);

        let id = manager
            .register(NewWatch {
                event_type: "dm_reply".into(),
                match_user_id: None,
                match_channel_id: None,
                match_thread_ts: None,
                context: "test".into(),
                reminder_after_minutes: Some(10),
                reminder_message: Some("Reminder".into()),
                expires_minutes: Some(60),
                on_expire: Some("Expired".into()),
                channel_name: "slack".into(),
            })
            .await
            .unwrap();

        // Timer should exist before cancel
        {
            let timers = manager.timers.lock().await;
            assert!(timers.contains_key(&id));
        }

        manager.cancel(&id).await.unwrap();

        // Timer should be removed after cancel
        let timers = manager.timers.lock().await;
        assert!(!timers.contains_key(&id));

        // Watch should no longer be active
        drop(timers);
        let active = manager.active_watches().await;
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn watch_manager_init_respawns_timers() {
        let store = test_manager_store();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let manager = WatchManager::new(Arc::clone(&store), tx.clone());

        // Register a watch with timers
        let id = manager
            .register(NewWatch {
                event_type: "dm_reply".into(),
                match_user_id: None,
                match_channel_id: None,
                match_thread_ts: None,
                context: "test init".into(),
                reminder_after_minutes: Some(60),
                reminder_message: Some("Reminder".into()),
                expires_minutes: Some(120),
                on_expire: Some("Expired".into()),
                channel_name: "slack".into(),
            })
            .await
            .unwrap();

        // Simulate a restart: create a new manager with the same store
        let manager2 = WatchManager::new(store, tx);

        // Before init, the new manager has no timers
        assert!(manager2.timers.lock().await.is_empty());

        // After init, it should have re-spawned timers for the active watch
        manager2.init().await.unwrap();
        assert!(manager2.timers.lock().await.contains_key(&id));
    }
}
