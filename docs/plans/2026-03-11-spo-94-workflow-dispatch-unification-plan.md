# Workflow and Dispatch Unification Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Consolidate ZeroClaw's intake, execution, state, trigger, and waiting systems into a unified dispatch model that supports async workflows with event-driven pause/resume.

**Architecture:** All triggers (channel messages, cron jobs, webhooks, workflow continuations) produce flat `WorkItem` structs fed through one dispatch path. Workflows persist state to SQLite when yielding, and resume when events match pending conditions or deadlines expire. Triage gate is removed; the intent model (@mention = act, otherwise = listen) replaces it.

**Tech Stack:** Rust, SQLite (via rusqlite), tokio mpsc channels, axum (gateway), serde for serialization

**Design doc:** `docs/plans/2026-03-11-spo-94-workflow-dispatch-unification-design.md`

---

## Phase 1: Remove Triage Gate

Remove the triage gate, `silent_act`, and `triage_required`. This is pure deletion — the intent model (@mention = act, no @mention = accumulate context silently) replaces the LLM-based triage decision.

### Task 1.1: Remove TriageAction and THREAD_TRIAGE_PROMPT

**Files:**
- Modify: `src/channels/types.rs:51-85` (remove `THREAD_TRIAGE_PROMPT`, `TriageAction`)
- Modify: `src/channels/types.rs:17` (remove `ConversationHistoryMap` if only used by triage — verify first)

**Step 1: Delete `THREAD_TRIAGE_PROMPT` constant**

Remove lines 51-75 in `src/channels/types.rs`.

**Step 2: Delete `TriageAction` enum**

Remove lines 77-85 in `src/channels/types.rs`.

**Step 3: Compile to find all references**

Run: `cargo check 2>&1 | head -60`
Expected: Compilation errors pointing to triage references in orchestrator.rs

**Step 4: Commit**

```bash
git add src/channels/types.rs
git commit -m "refactor(triage): remove TriageAction enum and THREAD_TRIAGE_PROMPT constant

Part of SPO-94: triage gate removal. Intent model replaces LLM-based triage."
```

### Task 1.2: Remove triage_required from ChannelMessage

**Files:**
- Modify: `src/channels/traits.rs:17-19` (remove `triage_required` field)

**Step 1: Remove `triage_required` field from `ChannelMessage`**

Remove the `triage_required: bool` field from the struct at `src/channels/traits.rs`.

**Step 2: Compile to find all references**

Run: `cargo check 2>&1 | head -60`
Expected: Errors wherever `triage_required` is set or read.

**Step 3: Fix each reference**

For each error:
- In `src/channels/slack.rs`: where `MentionGateResult::ParticipatedThread` sets `triage_required: true`, change to `triage_required` field removal (the message still gets produced, it just doesn't go through triage).
- In `src/channels/orchestrator.rs`: where `msg.triage_required` is checked, remove the triage block entirely.
- In any other channel implementations: remove the field from struct construction.

**Step 4: Compile clean**

Run: `cargo check`
Expected: Clean compilation.

**Step 5: Commit**

```bash
git add src/channels/traits.rs src/channels/slack.rs src/channels/orchestrator.rs
git commit -m "refactor(triage): remove triage_required field from ChannelMessage

All intake now uses intent model: @mention = act, no @mention = accumulate."
```

### Task 1.3: Remove triage LLM call from orchestrator

**Files:**
- Modify: `src/channels/orchestrator.rs:1179-1245` (remove triage block)
- Modify: `src/channels/orchestrator.rs:99-151` (remove `parse_triage_action()`)

**Step 1: Remove triage evaluation block**

In `process_channel_message()` at `src/channels/orchestrator.rs:1179-1245`, remove the entire `if msg.triage_required { ... }` block including the LLM call, timeout, parsing, and action matching.

**Step 2: Remove `parse_triage_action()` function**

Delete the function at lines 99-151.

**Step 3: Remove triage_model from runtime context if unused**

Check if `ctx.triage_model` is used anywhere else. If not, remove it from `ChannelRuntimeContext`.

**Step 4: Run tests**

Run: `cargo test --lib channels`
Expected: All tests pass (triage-specific tests should be removed or updated).

**Step 5: Run full check**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: All pass.

**Step 6: Commit**

```bash
git add src/channels/orchestrator.rs
git commit -m "refactor(triage): remove triage LLM call and parse_triage_action

Triage gate fully removed. SPO-94 intent model is now the only intake decision."
```

### Task 1.4: Update Slack mention gate to accumulate silently

**Files:**
- Modify: `src/channels/slack.rs:658-684` (`resolve_mention_gate()`)
- Modify: `src/channels/slack.rs:233-239` (`MentionGateResult`)

**Step 1: Simplify MentionGateResult**

`ParticipatedThread` no longer needs to set `triage_required`. Verify what happens to messages that hit `ParticipatedThread` — they should now be silently accumulated (not dispatched), unless they contain an @mention.

Review the listen loop in `slack.rs` where `MentionGateResult` is matched. For `ParticipatedThread`: instead of producing a `ChannelMessage` with `triage_required: true`, skip sending the message through the dispatch channel entirely. The thread history hydration via `conversations.replies` already captures these messages when Rain is later @mentioned.

**Step 2: Update the match in listen()**

Where the listen loop handles `MentionGateResult::ParticipatedThread`:
- Old behavior: produce ChannelMessage with `triage_required: true`
- New behavior: log at trace level, do not send to dispatch channel

**Step 3: Update or remove `ParticipatedThread` variant**

If `ParticipatedThread` now always results in "do nothing," consider whether the variant is still needed. If `resolve_mention_gate()` can return `Buffer` for this case instead, remove `ParticipatedThread` entirely.

**Step 4: Run tests**

Run: `cargo test --lib channels::slack`
Expected: Pass (update any tests that assert on `ParticipatedThread` behavior).

**Step 5: Commit**

```bash
git add src/channels/slack.rs
git commit -m "refactor(slack): simplify mention gate — participated threads accumulate silently

ParticipatedThread messages no longer dispatch. Thread context is hydrated
via conversations.replies when Rain is @mentioned."
```

---

## Phase 2: Core Types

Define the foundational types that the rest of the system builds on.

### Task 2.1: Define Event types

**Files:**
- Create: `src/events/types.rs`
- Create: `src/events/mod.rs`
- Modify: `src/lib.rs` (add `pub mod events`)

**Step 1: Write tests for Event serialization**

Create `src/events/types.rs` with test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_round_trips_through_serde() {
        let event = Event {
            source: IntegrationSource::Slack,
            event_type: "message".to_string(),
            fields: {
                let mut m = HashMap::new();
                m.insert("sender".to_string(), json!("U05TBBNT94G"));
                m.insert("thread_ts".to_string(), json!("1709312400.123456"));
                m
            },
            timestamp: chrono::Utc::now(),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.source, IntegrationSource::Slack);
        assert_eq!(deserialized.event_type, "message");
    }

    #[test]
    fn event_matcher_matches_exact_fields() {
        let event = Event {
            source: IntegrationSource::GitHub,
            event_type: "pr_merged".to_string(),
            fields: {
                let mut m = HashMap::new();
                m.insert("branch".to_string(), json!("feature/spo-94"));
                m
            },
            timestamp: chrono::Utc::now(),
        };
        let matcher = EventMatcher {
            source: Some(IntegrationSource::GitHub),
            event_type: Some("pr_merged".to_string()),
            field_filters: vec![
                FieldFilter::Exact("branch".to_string(), json!("feature/spo-94")),
            ],
        };
        assert!(matcher.matches(&event));
    }

    #[test]
    fn event_matcher_rejects_wrong_source() {
        let event = Event {
            source: IntegrationSource::Slack,
            event_type: "message".to_string(),
            fields: HashMap::new(),
            timestamp: chrono::Utc::now(),
        };
        let matcher = EventMatcher {
            source: Some(IntegrationSource::GitHub),
            event_type: None,
            field_filters: vec![],
        };
        assert!(!matcher.matches(&event));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib events`
Expected: FAIL — module doesn't exist yet.

**Step 3: Implement Event types**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationSource {
    Slack,
    GitHub,
    Linear,
    Timer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub source: IntegrationSource,
    pub event_type: String,
    pub fields: HashMap<String, Value>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldFilter {
    Exact(String, Value),
    Contains(String, String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMatcher {
    pub source: Option<IntegrationSource>,
    pub event_type: Option<String>,
    pub field_filters: Vec<FieldFilter>,
}

impl EventMatcher {
    pub fn matches(&self, event: &Event) -> bool {
        if let Some(ref src) = self.source {
            if src != &event.source {
                return false;
            }
        }
        if let Some(ref et) = self.event_type {
            if et != &event.event_type {
                return false;
            }
        }
        for filter in &self.field_filters {
            match filter {
                FieldFilter::Exact(key, value) => {
                    if event.fields.get(key) != Some(value) {
                        return false;
                    }
                }
                FieldFilter::Contains(key, substring) => {
                    let matches = event.fields.get(key)
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains(substring))
                        .unwrap_or(false);
                    if !matches {
                        return false;
                    }
                }
            }
        }
        true
    }
}
```

**Step 4: Create `src/events/mod.rs`**

```rust
pub mod types;
pub use types::*;
```

**Step 5: Add module to `src/lib.rs`**

Add `pub mod events;` to the module list.

**Step 6: Run tests**

Run: `cargo test --lib events`
Expected: All pass.

**Step 7: Commit**

```bash
git add src/events/ src/lib.rs
git commit -m "feat(events): add Event, EventMatcher, and IntegrationSource types

Foundation types for SPO-94 event system. Events carry source, type, and
arbitrary fields. EventMatcher supports exact and contains field filters."
```

### Task 2.2: Define WaitCondition and CompletionTrigger

**Files:**
- Modify: `src/events/types.rs` (add WaitCondition, CompletionTrigger, TimeoutBehavior)

**Step 1: Write tests**

```rust
#[test]
fn wait_condition_with_mention_trigger() {
    let condition = WaitCondition {
        workflow_id: "wf-001".to_string(),
        event_matcher: EventMatcher {
            source: Some(IntegrationSource::Slack),
            event_type: Some("mention".to_string()),
            field_filters: vec![
                FieldFilter::Exact("thread_ts".to_string(), json!("123.456")),
            ],
        },
        completion: CompletionTrigger::Mention,
        deadline: Some(Utc::now() + chrono::Duration::hours(3)),
        timeout_behavior: TimeoutBehavior::CollectAndContinue,
    };
    let serialized = serde_json::to_string(&condition).unwrap();
    let deserialized: WaitCondition = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.workflow_id, "wf-001");
    assert!(matches!(deserialized.completion, CompletionTrigger::Mention));
}

#[test]
fn wait_condition_with_event_match_trigger() {
    let condition = WaitCondition {
        workflow_id: "wf-002".to_string(),
        event_matcher: EventMatcher {
            source: Some(IntegrationSource::GitHub),
            event_type: Some("pr_merged".to_string()),
            field_filters: vec![
                FieldFilter::Contains("branch".to_string(), "spo-94".to_string()),
            ],
        },
        completion: CompletionTrigger::EventMatch,
        deadline: Some(Utc::now() + chrono::Duration::hours(24)),
        timeout_behavior: TimeoutBehavior::SkipWithDefault("PR not merged in time".to_string()),
    };
    assert!(matches!(condition.completion, CompletionTrigger::EventMatch));
}
```

**Step 2: Run to verify fail**

Run: `cargo test --lib events`
Expected: FAIL — types not defined.

**Step 3: Implement types**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompletionTrigger {
    EventMatch,
    Mention,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimeoutBehavior {
    CollectAndContinue,
    SkipWithDefault(String),
    Retry { max: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitCondition {
    pub workflow_id: String,
    pub event_matcher: EventMatcher,
    pub completion: CompletionTrigger,
    pub deadline: Option<DateTime<Utc>>,
    pub timeout_behavior: TimeoutBehavior,
}
```

**Step 4: Run tests**

Run: `cargo test --lib events`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/events/types.rs
git commit -m "feat(events): add WaitCondition, CompletionTrigger, TimeoutBehavior

Workflows register WaitConditions when yielding. CompletionTrigger supports
EventMatch (for webhooks) and Mention (for Slack DM flows)."
```

### Task 2.3: Define WorkItem and ReplyTarget

**Files:**
- Create: `src/dispatch/types.rs`
- Create: `src/dispatch/mod.rs`
- Modify: `src/lib.rs` (add `pub mod dispatch`)

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_from_channel_needs_evaluation() {
        let item = WorkItem {
            prompt: "What's the sprint status?".to_string(),
            context: vec![ContextBlock::ThreadHistory("prior messages".to_string())],
            reply_to: ReplyTarget::Channel {
                channel_name: "slack".to_string(),
                channel_id: "C123".to_string(),
                thread_ts: Some("123.456".to_string()),
            },
            constraints: ExecutionConstraints::default(),
            needs_evaluation: true,
        };
        assert!(item.needs_evaluation);
    }

    #[test]
    fn work_item_from_cron_skips_evaluation() {
        let item = WorkItem {
            prompt: "Run standup ritual".to_string(),
            context: vec![ContextBlock::ContextFile("standup.md content".to_string())],
            reply_to: ReplyTarget::Delivery {
                channel_name: "slack".to_string(),
                recipient: "C123".to_string(),
                thread_ts: None,
            },
            constraints: ExecutionConstraints::default(),
            needs_evaluation: false,
        };
        assert!(!item.needs_evaluation);
    }

    #[test]
    fn work_item_from_workflow_continuation() {
        let item = WorkItem {
            prompt: "Synthesize standup replies".to_string(),
            context: vec![
                ContextBlock::PriorStepResult("User A: working on auth".to_string()),
                ContextBlock::PriorStepResult("User B: no reply (timeout)".to_string()),
            ],
            reply_to: ReplyTarget::Workflow {
                workflow_id: "wf-001".to_string(),
            },
            constraints: ExecutionConstraints::default(),
            needs_evaluation: false,
        };
        assert!(matches!(item.reply_to, ReplyTarget::Workflow { .. }));
    }
}
```

**Step 2: Run to verify fail**

Run: `cargo test --lib dispatch`
Expected: FAIL.

**Step 3: Implement types**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContextBlock {
    ThreadHistory(String),
    ContextFile(String),
    PriorStepResult(String),
    MemoryContext(String),
    EventPayload(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplyTarget {
    Channel {
        channel_name: String,
        channel_id: String,
        thread_ts: Option<String>,
    },
    Delivery {
        channel_name: String,
        recipient: String,
        thread_ts: Option<String>,
    },
    Workflow {
        workflow_id: String,
    },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConstraints {
    pub tools: Option<Vec<String>>,
    pub model_hint: Option<String>,
    pub max_iterations: Option<u32>,
}

impl Default for ExecutionConstraints {
    fn default() -> Self {
        Self {
            tools: None,
            model_hint: None,
            max_iterations: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub prompt: String,
    pub context: Vec<ContextBlock>,
    pub reply_to: ReplyTarget,
    pub constraints: ExecutionConstraints,
    pub needs_evaluation: bool,
}
```

**Step 4: Create `src/dispatch/mod.rs`**

```rust
pub mod types;
pub use types::*;
```

**Step 5: Add module to `src/lib.rs`**

Add `pub mod dispatch;` to the module list.

**Step 6: Run tests**

Run: `cargo test --lib dispatch`
Expected: All pass.

**Step 7: Commit**

```bash
git add src/dispatch/ src/lib.rs
git commit -m "feat(dispatch): add WorkItem, ReplyTarget, ContextBlock, ExecutionConstraints

Unified dispatch types for SPO-94. Every trigger (channel, cron, webhook,
workflow) produces a WorkItem. ReplyTarget encodes where output goes."
```

### Task 2.4: Define WorkflowState for persistence

**Files:**
- Modify: `src/events/types.rs` (add WorkflowState)

**Step 1: Write tests**

```rust
#[test]
fn workflow_state_round_trips() {
    let state = WorkflowState {
        workflow_id: "wf-001".to_string(),
        completed_steps: vec!["Send standup DMs".to_string()],
        accumulated_context: vec!["DM sent to U05X, thread_ts=123.456".to_string()],
        remaining_steps_json: json!([{"prompt": "Synthesize replies"}]),
        wait_condition: WaitCondition {
            workflow_id: "wf-001".to_string(),
            event_matcher: EventMatcher {
                source: Some(IntegrationSource::Slack),
                event_type: Some("mention".to_string()),
                field_filters: vec![],
            },
            completion: CompletionTrigger::Mention,
            deadline: Some(Utc::now() + chrono::Duration::hours(3)),
            timeout_behavior: TimeoutBehavior::CollectAndContinue,
        },
        reply_to_json: json!({"Channel": {"channel_name": "slack", "channel_id": "C123", "thread_ts": null}}),
        created_at: Utc::now(),
        schema_version: 1,
    };
    let serialized = serde_json::to_string(&state).unwrap();
    let deserialized: WorkflowState = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.workflow_id, "wf-001");
    assert_eq!(deserialized.schema_version, 1);
}
```

**Step 2: Run to verify fail**

Run: `cargo test --lib events`
Expected: FAIL.

**Step 3: Implement**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub workflow_id: String,
    pub completed_steps: Vec<String>,
    pub accumulated_context: Vec<String>,
    pub remaining_steps_json: Value,
    pub wait_condition: WaitCondition,
    pub reply_to_json: Value,
    pub created_at: DateTime<Utc>,
    pub schema_version: u32,
}
```

**Step 4: Run tests**

Run: `cargo test --lib events`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/events/types.rs
git commit -m "feat(events): add WorkflowState for yield/resume persistence

Serializable workflow state captures completed steps, remaining steps,
and the wait condition. Schema version enables forward-compatible migration."
```

---

## Phase 3: Workflow State Persistence

Create the SQLite table and CRUD operations for persisted workflow state.

### Task 3.1: Create workflow_states SQLite table

**Files:**
- Create: `src/events/store.rs`
- Modify: `src/events/mod.rs`

**Step 1: Write tests for store operations**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        (dir, path)
    }

    #[test]
    fn init_creates_table() {
        let (_dir, path) = test_db();
        init_workflow_store(&path).unwrap();
        // Verify table exists by inserting and reading
    }

    #[test]
    fn save_and_load_workflow_state() {
        let (_dir, path) = test_db();
        init_workflow_store(&path).unwrap();

        let state = WorkflowState { /* ... test state ... */ };
        save_workflow_state(&path, &state).unwrap();

        let loaded = load_workflow_state(&path, "wf-001").unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().workflow_id, "wf-001");
    }

    #[test]
    fn list_pending_conditions() {
        let (_dir, path) = test_db();
        init_workflow_store(&path).unwrap();

        // Save two workflow states with different conditions
        // Verify list_pending_conditions returns both
    }

    #[test]
    fn delete_workflow_state() {
        let (_dir, path) = test_db();
        init_workflow_store(&path).unwrap();

        let state = WorkflowState { /* ... */ };
        save_workflow_state(&path, &state).unwrap();
        delete_workflow_state(&path, "wf-001").unwrap();

        let loaded = load_workflow_state(&path, "wf-001").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn expired_workflows_found_by_deadline() {
        let (_dir, path) = test_db();
        init_workflow_store(&path).unwrap();

        // Save state with deadline in the past
        // Verify list_expired_workflows returns it
    }
}
```

**Step 2: Implement store**

Follow the pattern in `src/cron/store.rs:658-690`. Use the same `ensure_db` / connection helper pattern. Table schema:

```sql
CREATE TABLE IF NOT EXISTS workflow_states (
    workflow_id TEXT PRIMARY KEY,
    state_json TEXT NOT NULL,
    wait_condition_json TEXT NOT NULL,
    deadline TEXT,
    created_at TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1
)
```

Functions:
- `init_workflow_store(db_path) -> Result<()>`
- `save_workflow_state(db_path, state) -> Result<()>` (upsert)
- `load_workflow_state(db_path, workflow_id) -> Result<Option<WorkflowState>>`
- `delete_workflow_state(db_path, workflow_id) -> Result<()>`
- `list_pending_conditions(db_path) -> Result<Vec<WaitCondition>>`
- `list_expired_workflows(db_path, now) -> Result<Vec<WorkflowState>>`

**Step 3: Run tests**

Run: `cargo test --lib events::store`
Expected: All pass.

**Step 4: Commit**

```bash
git add src/events/store.rs src/events/mod.rs
git commit -m "feat(events): add workflow state SQLite persistence

CRUD operations for workflow_states table. Supports save, load, delete,
list pending conditions, and list expired workflows by deadline."
```

---

## Phase 4: Event Stream and Matching

Wire events into a shared stream and add condition matching to the dispatch path.

### Task 4.1: Create event bus (mpsc channel)

**Files:**
- Create: `src/events/bus.rs`
- Modify: `src/events/mod.rs`

**Step 1: Write tests**

```rust
#[tokio::test]
async fn event_bus_broadcasts_to_subscribers() {
    let bus = EventBus::new(100);
    let mut rx = bus.subscribe();
    let event = Event { /* ... */ };
    bus.publish(event.clone()).await;
    let received = rx.recv().await.unwrap();
    assert_eq!(received.event_type, event.event_type);
}
```

**Step 2: Implement**

Use `tokio::sync::broadcast` for multiple consumers. The bus holds a `broadcast::Sender<Event>`. `subscribe()` returns a `broadcast::Receiver<Event>`.

```rust
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub async fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
```

**Step 3: Run tests**

Run: `cargo test --lib events::bus`
Expected: All pass.

**Step 4: Commit**

```bash
git add src/events/bus.rs src/events/mod.rs
git commit -m "feat(events): add EventBus using tokio broadcast channel

Publish/subscribe event bus. Integrations publish Events; dispatch loop
and scheduler subscribe to match against pending workflow conditions."
```

### Task 4.2: Add condition matching to dispatch path

**Files:**
- Create: `src/events/matcher.rs`
- Modify: `src/events/mod.rs`

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_event_to_pending_condition() {
        let conditions = vec![
            WaitCondition {
                workflow_id: "wf-001".to_string(),
                event_matcher: EventMatcher {
                    source: Some(IntegrationSource::Slack),
                    event_type: Some("mention".to_string()),
                    field_filters: vec![
                        FieldFilter::Exact("thread_ts".to_string(), json!("123.456")),
                    ],
                },
                completion: CompletionTrigger::Mention,
                deadline: None,
                timeout_behavior: TimeoutBehavior::CollectAndContinue,
            },
        ];

        let event = Event {
            source: IntegrationSource::Slack,
            event_type: "mention".to_string(),
            fields: {
                let mut m = HashMap::new();
                m.insert("thread_ts".to_string(), json!("123.456"));
                m
            },
            timestamp: Utc::now(),
        };

        let matched = find_matching_conditions(&conditions, &event);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].workflow_id, "wf-001");
    }

    #[test]
    fn no_match_returns_empty() {
        let conditions = vec![/* ... condition for GitHub ... */];
        let event = Event {
            source: IntegrationSource::Slack,
            /* ... */
        };
        let matched = find_matching_conditions(&conditions, &event);
        assert!(matched.is_empty());
    }

    #[test]
    fn multiple_conditions_can_match_same_event() {
        // Two workflows waiting on same thread — both should match
    }
}
```

**Step 2: Implement**

```rust
pub fn find_matching_conditions<'a>(
    conditions: &'a [WaitCondition],
    event: &Event,
) -> Vec<&'a WaitCondition> {
    conditions
        .iter()
        .filter(|c| c.event_matcher.matches(event))
        .collect()
}
```

**Step 3: Run tests**

Run: `cargo test --lib events::matcher`
Expected: All pass.

**Step 4: Commit**

```bash
git add src/events/matcher.rs src/events/mod.rs
git commit -m "feat(events): add condition matcher for pending workflows

find_matching_conditions scans pending WaitConditions against incoming
Events. Returns all matches — multiple workflows can wait on the same event."
```

---

## Phase 5: Widen Slack Event Filter

Accept `reaction_added` and `app_mention` events from the Slack Socket Mode WebSocket.

### Task 5.1: Widen parse_socket_event to accept new event types

**Files:**
- Modify: `src/channels/slack.rs:273-289` (`parse_socket_event()`)

**Step 1: Write tests for new event types**

Add tests alongside existing Slack tests:

```rust
#[test]
fn parse_socket_event_accepts_reaction_added() {
    let payload = json!({
        "type": "events_api",
        "envelope_id": "env-react-001",
        "payload": {
            "event": {
                "type": "reaction_added",
                "user": "U_USER",
                "reaction": "white_check_mark",
                "item": {
                    "type": "message",
                    "channel": "C_CHAN",
                    "ts": "123.456"
                }
            }
        }
    });
    let result = parse_socket_event(&payload.to_string());
    assert!(result.is_some());
    // Verify event type and fields are extracted
}

#[test]
fn parse_socket_event_accepts_app_mention() {
    let payload = json!({
        "type": "events_api",
        "envelope_id": "env-mention-001",
        "payload": {
            "event": {
                "type": "app_mention",
                "user": "U_USER",
                "text": "<@BOT> hello",
                "channel": "C_CHAN",
                "ts": "789.012"
            }
        }
    });
    let result = parse_socket_event(&payload.to_string());
    assert!(result.is_some());
}
```

**Step 2: Run to verify fail**

Run: `cargo test --lib channels::slack -- parse_socket_event`
Expected: FAIL — current code filters these out.

**Step 3: Update parse_socket_event**

Widen the event type filter from `event.type == "message"` to also accept `"reaction_added"` and `"app_mention"`. Return a structured result that indicates the event type so the caller can handle each appropriately.

**Step 4: Update listen() to handle new event types**

In the listen loop, after `parse_socket_event`:
- `message` → existing handling (produce ChannelMessage or accumulate)
- `reaction_added` → publish Event to EventBus (for workflow condition matching)
- `app_mention` → treat as explicit mention (produce ChannelMessage, same as current @mention path)

**Step 5: Run tests**

Run: `cargo test --lib channels::slack`
Expected: All pass.

**Step 6: Commit**

```bash
git add src/channels/slack.rs
git commit -m "feat(slack): accept reaction_added and app_mention events

Widen Socket Mode event filter. reaction_added events publish to EventBus
for workflow condition matching. app_mention events dispatch as explicit mentions."
```

---

## Phase 6: Workflow Yield and Resume

Wire the yield/resume mechanism into plan execution and the dispatch loop.

### Task 6.1: Add yield capability to plan execution

**Files:**
- Modify: `src/planner/types.rs:39-66` (add `wait` action type handling)
- Modify: `src/planner/orchestrator.rs:632-752` (detect wait actions, persist state, return)

**Step 1: Define wait action semantics**

A plan action with `action_type: "wait"` signals the executor to yield. The action's `params` field carries the WaitCondition configuration:

```toml
[[plan.actions]]
action_type = "wait"
description = "Wait for cofounder reply"
group = 2

[plan.actions.params]
source = "slack"
event_type = "mention"
thread_ts = "{{dm_thread_ts}}"
deadline_hours = 3
timeout_behavior = "collect_and_continue"
```

**Step 2: Write test for wait action detection**

```rust
#[test]
fn executor_yields_on_wait_action() {
    // Build a plan with group 1 (delegate) and group 2 (wait)
    // Execute — group 1 runs, group 2 triggers yield
    // Verify PlanExecutionResult::Yielded is returned
}
```

**Step 3: Add `Yielded` variant to PlanExecutionResult**

```rust
pub enum PlanExecutionResult {
    Passthrough,
    Executed { output, action_results, analysis },
    Yielded {
        workflow_id: String,
        completed_steps: Vec<String>,
        accumulated_context: Vec<String>,
        wait_condition: WaitCondition,
    },
}
```

**Step 4: Implement yield in execute_plan**

In the group-by-group loop, when an action has `action_type == "wait"`:
1. Parse `WaitCondition` from action params
2. Generate workflow_id (UUID)
3. Return `PlanExecutionResult::Yielded` with state

**Step 5: Run tests**

Run: `cargo test --lib planner`
Expected: All pass.

**Step 6: Commit**

```bash
git add src/planner/types.rs src/planner/orchestrator.rs
git commit -m "feat(planner): add wait action type with yield capability

Plan actions with action_type 'wait' cause the executor to yield.
Returns PlanExecutionResult::Yielded with workflow state for persistence."
```

### Task 6.2: Persist workflow state on yield

**Files:**
- Modify: `src/channels/orchestrator.rs` (handle Yielded result from planner)
- Modify: `src/cron/scheduler.rs` (handle Yielded result from planner)

**Step 1: In channel orchestrator**

Where `plan_then_execute()` result is handled, add a match arm for `Yielded`:
1. Call `save_workflow_state()` to persist
2. Register the WaitCondition in the pending conditions store
3. Do not send a reply (execution is suspended)
4. Optionally send a status message ("Waiting for reply...")

**Step 2: In cron scheduler**

Same pattern — when `execute_plan()` returns `Yielded`, persist state instead of recording a completed run.

**Step 3: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 4: Commit**

```bash
git add src/channels/orchestrator.rs src/cron/scheduler.rs
git commit -m "feat(workflow): persist state and register conditions on yield

When plan execution yields, workflow state is saved to SQLite and the
WaitCondition is registered for event matching in the dispatch loop."
```

### Task 6.3: Resume workflow on event match

**Files:**
- Modify: `src/channels/orchestrator.rs` (add condition check before normal dispatch)

**Step 1: Add event matching at top of dispatch**

In `run_message_dispatch_loop()` or `process_channel_message()`, before the current evaluation logic:

1. Convert incoming `ChannelMessage` to `Event`
2. Load pending conditions from store
3. Call `find_matching_conditions()`
4. For each match:
   a. Load `WorkflowState` from store
   b. Build continuation `WorkItem` with remaining steps + event payload as context
   c. Delete the workflow state (it's being resumed)
   d. Dispatch the continuation WorkItem
5. If matched, skip normal processing for this message
6. If no match, continue with normal dispatch

**Step 2: Write integration test**

```rust
#[tokio::test]
async fn workflow_resumes_on_matching_event() {
    // 1. Save a WorkflowState with a WaitCondition matching thread_ts "123"
    // 2. Simulate an incoming message in thread "123" with @mention
    // 3. Verify workflow state is loaded and deleted
    // 4. Verify continuation WorkItem is dispatched
}
```

**Step 3: Implement**

**Step 4: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/channels/orchestrator.rs
git commit -m "feat(workflow): resume yielded workflows on matching events

Dispatch loop checks incoming messages against pending WaitConditions
before normal processing. Matching events resume the workflow with
remaining steps and event payload as context."
```

### Task 6.4: Resume workflow on deadline expiry

**Files:**
- Modify: `src/cron/scheduler.rs` (add deadline check to scheduler tick)

**Step 1: Add deadline check to scheduler polling loop**

On each tick, after processing due cron jobs:
1. Call `list_expired_workflows(db_path, now)`
2. For each expired workflow:
   a. Load full state
   b. Build continuation WorkItem with timeout payload
   c. Apply timeout_behavior (CollectAndContinue, SkipWithDefault, or Retry)
   d. If Retry and under max: send reminder, reset deadline
   e. Otherwise: delete state, dispatch continuation
3. Dispatch through normal agent execution path

**Step 2: Write test**

```rust
#[tokio::test]
async fn expired_workflow_resumes_with_timeout() {
    // 1. Save WorkflowState with deadline in the past
    // 2. Run scheduler tick
    // 3. Verify workflow is loaded and dispatched with timeout context
    // 4. Verify workflow state is deleted
}
```

**Step 3: Implement**

**Step 4: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/cron/scheduler.rs
git commit -m "feat(workflow): resume expired workflows on scheduler tick

Scheduler checks workflow deadlines alongside cron job schedules.
Expired workflows resume with timeout behavior (collect, skip, or retry)."
```

---

## Phase 7: Gateway Webhook Endpoints

Add GitHub and Linear webhook receivers that emit unified Events.

### Task 7.1: GitHub webhook endpoint

**Files:**
- Create: `src/gateway/github.rs`
- Modify: `src/gateway/mod.rs` (add route)

**Step 1: Write tests**

Test HMAC-SHA256 signature verification and event parsing. Follow the pattern in existing webhook handlers (WhatsApp at `src/gateway/mod.rs:1126-1218`).

**Step 2: Implement handler**

```rust
pub async fn handle_github_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // 1. Verify X-Hub-Signature-256 header (HMAC-SHA256)
    // 2. Parse X-GitHub-Event header for event type
    // 3. Parse JSON body
    // 4. Build Event { source: GitHub, event_type, fields }
    // 5. Publish to EventBus
    // 6. Return 200 OK
}
```

**Step 3: Register route**

Add `.route("/github", post(github::handle_github_webhook))` to gateway router.

**Step 4: Run tests**

Run: `cargo test --lib gateway`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/gateway/github.rs src/gateway/mod.rs
git commit -m "feat(gateway): add GitHub webhook endpoint

POST /github receives GitHub webhook events, verifies HMAC-SHA256
signature, and publishes unified Events to the event bus."
```

### Task 7.2: Linear webhook endpoint

**Files:**
- Create: `src/gateway/linear.rs`
- Modify: `src/gateway/mod.rs` (add route)

**Step 1: Write tests**

Same pattern as GitHub but with Linear's signature format.

**Step 2: Implement handler**

Same structure as GitHub handler but parsing Linear's payload format (action, type, data fields).

**Step 3: Register route**

Add `.route("/linear", post(linear::handle_linear_webhook))` to gateway router.

**Step 4: Run tests**

Run: `cargo test --lib gateway`
Expected: All pass.

**Step 5: Commit**

```bash
git add src/gateway/linear.rs src/gateway/mod.rs
git commit -m "feat(gateway): add Linear webhook endpoint

POST /linear receives Linear webhook events, verifies signature,
and publishes unified Events to the event bus."
```

---

## Phase 8: State Cleanup

Remove redundant state and simplify.

### Task 8.1: Remove last_output from CronJob

**Files:**
- Modify: `src/cron/types.rs:123` (remove `last_output` field)
- Modify: `src/cron/store.rs` (remove `last_output` from UPDATE queries)
- Modify: any CLI display code that reads `last_output`

**Step 1: Remove field and fix compilation**

Remove `last_output` from `CronJob` struct. Fix all references — CLI display should query `cron_runs` for the most recent run's output instead.

**Step 2: Update SQLite schema**

Add migration: `ALTER TABLE cron_jobs DROP COLUMN last_output` (SQLite 3.35+) or recreate table.

**Step 3: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 4: Commit**

```bash
git add src/cron/types.rs src/cron/store.rs
git commit -m "refactor(cron): remove last_output from CronJob

Redundant with cron_runs table. CLI now queries most recent run."
```

---

## Phase 9: Full Validation

### Task 9.1: Run full CI checks

**Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: Clean.

**Step 2: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: Clean.

**Step 3: Full test suite**

Run: `cargo test`
Expected: All pass.

**Step 4: Review dead code**

Run: `cargo clippy --all-targets -- -W dead_code`
Verify no orphaned triage code, unused ParticipatedThread references, or stale imports remain.

---

## Dependency Graph

```
Phase 1 (triage removal) ──┐
                            ├── Phase 2 (core types) ── Phase 3 (workflow store) ── Phase 6 (yield/resume)
                            │                                                            │
                            │                       Phase 4 (event bus + matcher) ───────┘
                            │                            │
                            │                       Phase 5 (Slack event widening)
                            │
                            └── Phase 7 (gateway webhooks) ── depends on Phase 4

Phase 8 (state cleanup) ── independent, can run anytime after Phase 1
Phase 9 (validation) ── final gate
```

Phases 1 and 2 can run in parallel. Phase 5 and 7 can run in parallel after Phase 4. Phase 8 is independent.
