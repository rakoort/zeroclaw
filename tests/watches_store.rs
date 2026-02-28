//! Integration tests for the WatchStore module.

use zeroclaw::watches::{NewWatch, WatchStore};

fn test_store() -> WatchStore {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    WatchStore::init_schema(&conn).unwrap();
    WatchStore { conn }
}

#[test]
fn register_and_retrieve_watch() {
    let store = test_store();
    let id = store
        .register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U_TEST_001".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "Waiting for standup reply".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: Some(240),
            on_expire: Some("Post summary".into()),
            channel_name: "slack".into(),
        })
        .unwrap();
    assert!(!id.is_empty());
    let watches = store.active_watches();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].id, id);
    assert_eq!(watches[0].context, "Waiting for standup reply");
}

#[test]
fn check_message_matches_user_id() {
    let store = test_store();
    store
        .register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U_TEST_001".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "test context".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        })
        .unwrap();
    let result = store.check_message("U_TEST_001", "D456", None, "slack");
    assert!(result.is_some());
    let result = store.check_message("U_TEST_999", "D456", None, "slack");
    assert!(result.is_none());
}

#[test]
fn mark_matched_removes_from_active() {
    let store = test_store();
    let id = store
        .register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U_TEST_001".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "test".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        })
        .unwrap();
    store.mark_matched(&id).unwrap();
    assert!(store.active_watches().is_empty());
}

#[test]
fn cancel_watch() {
    let store = test_store();
    let id = store
        .register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: None,
            match_channel_id: None,
            match_thread_ts: None,
            context: "test".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        })
        .unwrap();
    store.cancel(&id).unwrap();
    assert!(store.active_watches().is_empty());
}
