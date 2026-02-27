# Rain Agent Runtime Extensions — Design

Date: 2026-02-27
Status: Draft
Slug: `rain-runtime-extensions`

## Problem

Rain is an autonomous PM agent running on zeroclaw. It manages Slack messages and Linear issues through two TypeScript CLI scripts (`slack-cli.ts`, `linear-cli.ts`). Three runtime gaps block reliable operation:

1. **Tool hallucination.** The Gemini provider lacks native function calling. Tools enter the system prompt as text instructions; the model outputs XML tags that `XmlToolDispatcher` parses. The model often generates text claiming it performed an action yet emits no tool call tag — hallucinating the action. The shell security validator also blocks composed `npx tsx ...` commands intermittently.

2. **Untyped tools.** Rain constructs shell commands as strings (`npx tsx slack-cli.ts dm U05SQR41Q0L "message"`). These strings are fragile, unvalidated, and bypass the structured tool interface.

3. **Single-turn execution.** Zeroclaw processes every message as an independent turn. Rain cannot send a DM, wait for the reply, then act on it. Standups, follow-ups, and every ask-then-act interaction break.

## Solution

Three layered changes, each building on the previous:

**A. Gemini native function calling** — the model receives structured JSON schemas via `functionDeclarations` and returns structured `functionCall` objects. No XML parsing. No text-embedded tool calls.

**B. Typed Rust tools** — each Slack and Linear operation becomes a separate `Tool` implementation with a precise JSON schema. The model calls `slack_dm` with structured params; it never constructs shell strings.

**C. Event watch system** — registers passive listeners that match incoming messages by user, channel, or thread. When a watch matches, its stored context injects into the agent turn. Reminders and expiry use per-watch `tokio::spawn` + `sleep_until` tasks — no polling.

## Design

### Part A: Gemini Native Function Calling

#### Current State

The Gemini provider (`src/providers/gemini.rs`) lacks native tool support. `supports_native_tools()` returns `false` (the trait default), and `chat()` returns `tool_calls: Vec::new()`. The `ToolsPayload::Gemini` variant exists in the provider trait but nothing uses it.

#### Changes

**1. Extend request/response types.**

The current `Part` struct has a single `text: String` field. Gemini's function calling requires parts that carry `functionCall` or `functionResponse` data alongside — or instead of — text.

Request-side `Part` (serialization):

```rust
#[derive(Debug, Serialize, Clone)]
struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<FunctionCallPart>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<FunctionResponsePart>,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionCallPart {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionResponsePart {
    name: String,
    response: serde_json::Value,
}
```

Response-side `ResponsePart` (deserialization) — add optional fields:

```rust
#[derive(Debug, Deserialize)]
struct ResponsePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: bool,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FunctionCallResponse>,
}

#[derive(Debug, Deserialize)]
struct FunctionCallResponse {
    name: String,
    args: serde_json::Value,
}
```

Extend `GenerateContentRequest` with optional tool fields:

```rust
#[derive(Debug, Serialize, Clone)]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolDeclaration>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolConfig>,
}

#[derive(Debug, Serialize, Clone)]
struct GeminiToolDeclaration {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Clone)]
struct GeminiToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: FunctionCallingConfigMode,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionCallingConfigMode {
    mode: String,  // "AUTO"
}
```

Add the same fields to `InternalGenerateContentRequest` for the cloudcode-pa endpoint.

**2. Override provider trait methods.**

```rust
fn capabilities(&self) -> ProviderCapabilities {
    ProviderCapabilities { native_tool_calling: true, vision: true }
}

fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
    ToolsPayload::Gemini {
        function_declarations: tools.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        })).collect(),
    }
}
```

**3. Implement `chat_with_tools()`.**

This method builds a `GenerateContentRequest` with the `tools` and `toolConfig` fields populated, sends it via `send_generate_content` (whose signature must change to accept optional tools), and parses `functionCall` parts from the response into `ChatResponse::tool_calls`.

Change the `send_generate_content` signature from:

```rust
async fn send_generate_content(
    &self,
    contents: Vec<Content>,
    system_instruction: Option<Content>,
    model: &str,
    temperature: f64,
) -> anyhow::Result<(String, Option<TokenUsage>)>
```

To:

```rust
async fn send_generate_content(
    &self,
    contents: Vec<Content>,
    system_instruction: Option<Content>,
    model: &str,
    temperature: f64,
    tools: Option<Vec<GeminiToolDeclaration>>,
    tool_config: Option<GeminiToolConfig>,
) -> anyhow::Result<GeminiResponse>
```

`GeminiResponse` carries both text and parsed function calls:

```rust
struct GeminiResponse {
    text: Option<String>,
    function_calls: Vec<ToolCall>,
    usage: Option<TokenUsage>,
    reasoning_content: Option<String>,
}
```

**4. Handle multi-turn tool conversation history.**

When the `NativeToolDispatcher` sends conversation history containing tool calls and tool results, the Gemini message converter must translate them into Gemini's format:

- `ConversationMessage::AssistantToolCalls` maps to model role with `functionCall` parts
- `ConversationMessage::ToolResults` maps to user role with `functionResponse` parts

The Gemini API expects function responses as `role: "user"` with `functionResponse` parts (confirmed against current API docs). The model's function call goes in a `role: "model"` part with `functionCall`.

#### API Format Reference

Confirmed against Gemini API docs (Feb 2026):

Request:
```json
{
  "contents": [...],
  "tools": [{"functionDeclarations": [...]}],
  "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
  "systemInstruction": {...},
  "generationConfig": {...}
}
```

Response — function call in candidate parts:
```json
{
  "candidates": [{
    "content": {
      "role": "model",
      "parts": [{"functionCall": {"name": "fn", "args": {...}}}]
    }
  }]
}
```

Multi-turn — function response sent back:
```json
{
  "role": "user",
  "parts": [{"functionResponse": {"name": "fn", "response": {...}}}]
}
```

The format is identical for both the Gemini Developer API and Vertex AI endpoints.

#### Files Changed

- `src/providers/gemini.rs` — struct extensions, trait overrides, `chat_with_tools()`, response parsing, message conversion

### Part B: Typed Slack & Linear Tools

#### File Structure

```
src/tools/slack/mod.rs          — SlackToolConfig, shared subprocess runner
src/tools/slack/send.rs         — SlackSendTool
src/tools/slack/dm.rs           — SlackDmTool
src/tools/slack/send_thread.rs  — SlackSendThreadTool
src/tools/slack/send_file.rs    — SlackSendFileTool
src/tools/slack/history.rs      — SlackHistoryTool
src/tools/slack/dm_history.rs   — SlackDmHistoryTool
src/tools/slack/threads.rs      — SlackThreadsTool
src/tools/slack/presence.rs     — SlackPresenceTool
src/tools/slack/react.rs        — SlackReactTool

src/tools/linear/mod.rs            — LinearToolConfig, shared subprocess runner
src/tools/linear/issues.rs         — LinearIssuesTool
src/tools/linear/create_issue.rs   — LinearCreateIssueTool
src/tools/linear/update_issue.rs   — LinearUpdateIssueTool
src/tools/linear/archive_issue.rs  — LinearArchiveIssueTool
src/tools/linear/add_comment.rs    — LinearAddCommentTool
src/tools/linear/teams.rs          — LinearTeamsTool
src/tools/linear/users.rs          — LinearUsersTool
src/tools/linear/projects.rs       — LinearProjectsTool
src/tools/linear/cycles.rs         — LinearCyclesTool
src/tools/linear/labels.rs         — LinearLabelsTool
src/tools/linear/states.rs         — LinearStatesTool
src/tools/linear/create_label.rs   — LinearCreateLabelTool
src/tools/linear/create_project.rs — LinearCreateProjectTool
src/tools/linear/create_cycle.rs   — LinearCreateCycleTool
```

#### Shared Config & Runner

Each service module owns a config struct and subprocess runner. All tools in a service share one `Arc<Config>`:

```rust
// src/tools/slack/mod.rs
pub struct SlackToolConfig {
    pub script_path: PathBuf,
    pub workspace_dir: PathBuf,
}

impl SlackToolConfig {
    pub fn new(script: &str, workspace_dir: &Path) -> Self {
        Self {
            script_path: workspace_dir.join(script),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Run the CLI script with the given args. Returns stdout on success.
    pub async fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("npx")
            .args(["tsx", &self.script_path.to_string_lossy()])
            .args(args)
            .current_dir(&self.workspace_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("CLI script failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
```

`LinearToolConfig` is identical in shape.

#### Individual Tool Pattern

Each tool is a thin wrapper: extract params, call the runner, return the result.

```rust
// src/tools/slack/dm.rs
pub struct SlackDmTool {
    config: Arc<SlackToolConfig>,
}

impl SlackDmTool {
    pub fn new(config: Arc<SlackToolConfig>) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for SlackDmTool {
    fn name(&self) -> &str { "slack_dm" }

    fn description(&self) -> &str {
        "Send a direct message to a Slack user"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "user_id": {"type": "string", "description": "Slack user ID (e.g. U05SQR41Q0L)"},
                "message": {"type": "string", "description": "Message text to send"},
                "ritual": {"type": "string", "description": "Ritual context for this action"},
                "context": {"type": "string", "description": "Reason for sending this message"}
            },
            "required": ["user_id", "message", "ritual", "context"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let user_id = args["user_id"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing user_id"))?;
        let message = args["message"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing message"))?;
        let ritual = args["ritual"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing ritual"))?;
        let context = args["context"].as_str()
            .ok_or_else(|| anyhow::anyhow!("missing context"))?;

        let output = self.config
            .run(&["dm", user_id, message, "--ritual", ritual, "--context", context])
            .await?;

        Ok(ToolResult { success: true, output, error: None })
    }
}
```

Read-only tools (history, issues, presence) omit `ritual` and `context`.

#### Write vs. Read Tool Contract

Write operations (send, dm, send-thread, send-file, react, create-issue, update-issue, archive-issue, add-comment, create-label, create-project, create-cycle) require `ritual` and `context` as mandatory parameters.

Read operations (history, dm-history, threads, presence, issues, teams, users, projects, cycles, labels, states) require neither.

#### Registration

Tools register conditionally in `src/tools/mod.rs` when config sections exist:

```rust
// In all_tools_with_runtime():
if let Some(ref slack_script) = config.tools_slack_script {
    let cfg = Arc::new(SlackToolConfig::new(slack_script, &config.workspace_dir));
    tool_arcs.push(Arc::new(slack::SlackDmTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackSendTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackSendThreadTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackSendFileTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackHistoryTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackDmHistoryTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackThreadsTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackPresenceTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::SlackReactTool::new(Arc::clone(&cfg))));
}

if let Some(ref linear_script) = config.tools_linear_script {
    let cfg = Arc::new(LinearToolConfig::new(linear_script, &config.workspace_dir));
    tool_arcs.push(Arc::new(linear::LinearIssuesTool::new(Arc::clone(&cfg))));
    // ... all linear tools
}
```

#### Config Schema Addition

New fields in `Config` / `schema.rs`:

```rust
pub tools_slack_script: Option<String>,
pub tools_linear_script: Option<String>,
```

Loaded from `zeroclaw.toml`:

```toml
[tools.slack]
script = "skills/slack/scripts/slack-cli.ts"

[tools.linear]
script = "skills/linear/scripts/linear-cli.ts"
```

#### Files Changed

- `src/tools/slack/` (new) — 10 files, one per Slack operation
- `src/tools/linear/` (new) — 14 files, one per Linear operation
- `src/tools/mod.rs` — conditional registration
- `src/config/schema.rs` — new config fields

### Part C: Event Watch System

#### Architecture

The watch system has three components:

1. **WatchStore** — SQLite table and query methods for watch CRUD
2. **WatchManager** — in-memory map of active timers; handles registration, cancellation, and timer spawning
3. **Watch tools** — `watch`, `watch_list`, `watch_cancel` tools the model calls

Watches persist as rows in SQLite. Active timers (reminders, expiry) run as `tokio::spawn` tasks with `sleep_until` — fully event-driven, zero polling.

#### SQLite Schema

Added to `init_schema()` in `src/memory/sqlite.rs`:

```sql
CREATE TABLE IF NOT EXISTS watches (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    match_user_id TEXT,
    match_channel_id TEXT,
    match_thread_ts TEXT,
    context TEXT NOT NULL,
    reminder_after_minutes INTEGER,
    reminder_message TEXT,
    reminder_sent INTEGER NOT NULL DEFAULT 0,
    expires_minutes INTEGER,
    on_expire TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    channel_name TEXT NOT NULL DEFAULT 'slack',
    created_at TEXT NOT NULL,
    matched_at TEXT,
    expires_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_watches_status ON watches(status);
CREATE INDEX IF NOT EXISTS idx_watches_active ON watches(status, channel_name);
```

#### WatchStore

```rust
// src/watches/mod.rs
pub struct WatchStore {
    conn: rusqlite::Connection,
}

impl WatchStore {
    /// Check if an incoming message matches any active watch.
    pub fn check_message(
        &self,
        user_id: &str,
        channel_id: &str,
        thread_ts: Option<&str>,
        channel_name: &str,
    ) -> Option<Watch> { ... }

    /// Insert a new watch. Returns the watch ID.
    pub fn register(&self, watch: NewWatch) -> anyhow::Result<String> { ... }

    /// Set status to 'cancelled'.
    pub fn cancel(&self, watch_id: &str) -> anyhow::Result<()> { ... }

    /// Set status to 'matched' and record matched_at.
    pub fn mark_matched(&self, watch_id: &str) -> anyhow::Result<()> { ... }

    /// Set reminder_sent = 1.
    pub fn mark_reminder_sent(&self, watch_id: &str) -> anyhow::Result<()> { ... }

    /// Set status to 'expired'.
    pub fn mark_expired(&self, watch_id: &str) -> anyhow::Result<()> { ... }

    /// Load all active watches (for daemon restart recovery).
    pub fn active_watches(&self) -> Vec<Watch> { ... }
}
```

#### WatchManager

```rust
pub struct WatchManager {
    store: Arc<WatchStore>,
    timers: Mutex<HashMap<String, CancellationToken>>,
    channel_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
}

impl WatchManager {
    /// Initialize from SQLite — re-spawn timers for unexpired active watches.
    pub async fn init(
        store: Arc<WatchStore>,
        channel_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Self { ... }

    /// Register a new watch and spawn timer tasks.
    pub async fn register(&self, watch: NewWatch) -> anyhow::Result<String> {
        let id = self.store.register(&watch)?;
        let cancel = CancellationToken::new();

        // Spawn reminder timer
        if let Some(reminder_mins) = watch.reminder_after_minutes {
            let cancel = cancel.clone();
            let store = Arc::clone(&self.store);
            let tx = self.channel_tx.clone();
            let watch_id = id.clone();
            let msg = watch.reminder_message.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(reminder_mins as u64 * 60)) => {
                        store.mark_reminder_sent(&watch_id).ok();
                        send_reminder(&tx, &watch_id, msg.as_deref()).await;
                    }
                    _ = cancel.cancelled() => {}
                }
            });
        }

        // Spawn expiry timer
        if let Some(expire_mins) = watch.expires_minutes {
            let cancel = cancel.clone();
            let store = Arc::clone(&self.store);
            let tx = self.channel_tx.clone();
            let watch_id = id.clone();
            let on_expire = watch.on_expire.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(expire_mins as u64 * 60)) => {
                        store.mark_expired(&watch_id).ok();
                        if let Some(prompt) = on_expire {
                            fire_expiry(&tx, &watch_id, &prompt).await;
                        }
                    }
                    _ = cancel.cancelled() => {}
                }
            });
        }

        self.timers.lock().await.insert(id.clone(), cancel);
        Ok(id)
    }

    /// Check incoming message against active watches.
    pub async fn check_message(
        &self,
        user_id: &str,
        channel_id: &str,
        thread_ts: Option<&str>,
        channel_name: &str,
    ) -> Option<Watch> {
        let watch = self.store.check_message(user_id, channel_id, thread_ts, channel_name)?;
        self.store.mark_matched(&watch.id).ok();
        if let Some(cancel) = self.timers.lock().await.remove(&watch.id) {
            cancel.cancel();
        }
        Some(watch)
    }

    /// Cancel a watch by ID.
    pub async fn cancel(&self, watch_id: &str) -> anyhow::Result<()> {
        self.store.cancel(watch_id)?;
        if let Some(cancel) = self.timers.lock().await.remove(watch_id) {
            cancel.cancel();
        }
        Ok(())
    }
}
```

#### Message Pipeline Injection

In `run_message_dispatch_loop()` (`src/channels/orchestrator.rs`), after receiving from `rx` and before dispatching:

```rust
while let Some(mut msg) = rx.recv().await {
    // Watch lookup — event-driven, fires only when a message arrives
    if let Some(watch) = watch_manager.check_message(
        &msg.sender,
        &msg.reply_target,
        msg.thread_ts.as_deref(),
        &msg.channel,
    ).await {
        msg.content = format!(
            "[Watch context — id: {}]\n{}\n\n---\nIncoming message:\n{}",
            watch.id, watch.context, msg.content
        );
    }

    // ... existing dispatch logic (semaphore, worker spawn, etc.)
}
```

#### Expiry & Reminder Delivery

When a timer fires, it injects a synthetic `ChannelMessage` into the same `tx` channel that real messages use:

```rust
async fn fire_expiry(
    tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    watch_id: &str,
    prompt: &str,
) {
    let msg = ChannelMessage {
        id: format!("watch_expire_{watch_id}"),
        sender: "system".into(),
        reply_target: String::new(),
        content: format!("[Watch expired — id: {watch_id}]\n{prompt}"),
        channel: "watch".into(),
        timestamp: now_epoch(),
        thread_ts: None,
        thread_starter_body: None,
        thread_history: None,
        triage_required: false,
        ack_reaction_ts: None,
    };
    let _ = tx.send(msg).await;
}
```

Expiry and reminder events flow through the same dispatch loop as real messages. No separate processing path exists.

#### Watch Tools

Three tools, registered unconditionally (watches work across channels):

**`watch`** — Register a new watch:
```json
{
  "type": "object",
  "properties": {
    "event_type": {"type": "string", "enum": ["dm_reply", "channel_message", "thread_reply"]},
    "match_user_id": {"type": "string", "description": "Slack user ID to match (omit for any user)"},
    "match_channel_id": {"type": "string", "description": "Channel ID to match (omit for any channel)"},
    "match_thread_ts": {"type": "string", "description": "Thread timestamp to match (omit for any thread)"},
    "context": {"type": "string", "description": "Context injected into agent turn when watch matches"},
    "reminder_after_minutes": {"type": "integer", "description": "Send reminder after N minutes if no match"},
    "reminder_message": {"type": "string", "description": "Reminder message text"},
    "expires_minutes": {"type": "integer", "description": "Watch expires after N minutes"},
    "on_expire": {"type": "string", "description": "Prompt injected as new agent turn on expiry"},
    "channel_name": {"type": "string", "default": "slack", "description": "Channel to watch"}
  },
  "required": ["event_type", "context"]
}
```

**`watch_list`** — List active watches (no params).

**`watch_cancel`** — Cancel a watch by ID:
```json
{
  "type": "object",
  "properties": {
    "watch_id": {"type": "string", "description": "ID of the watch to cancel"}
  },
  "required": ["watch_id"]
}
```

#### Files Changed

- `src/watches/mod.rs` (new) — WatchStore, WatchManager, Watch structs
- `src/watches/tools.rs` (new) — WatchTool, WatchListTool, WatchCancelTool
- `src/memory/sqlite.rs` — watches table in init_schema()
- `src/channels/orchestrator.rs` — watch check in dispatch loop, WatchManager init
- `src/tools/mod.rs` — register watch tools

### Integration: End-to-End Data Flow

#### Startup

1. Provider init: `GeminiProvider` returns `supports_native_tools() = true`
2. Dispatcher selection: runtime chooses `NativeToolDispatcher`
3. Tool registration: Slack and Linear tools load from config; watch tools load unconditionally
4. `WatchManager::init(sqlite_conn, channel_tx)` loads active watches and re-spawns their timers

#### Agent Turn with Native Tools

```
Model receives tool specs as Gemini functionDeclarations
    ↓
Model returns functionCall: {name: "slack_dm", args: {...}}
    ↓
NativeToolDispatcher extracts tool_calls from ChatResponse
    ↓
SlackDmTool.execute() spawns `npx tsx slack-cli.ts dm ...`, returns JSON
    ↓
NativeToolDispatcher formats result as functionResponse
    ↓
Next turn: model sees result, decides next action
```

#### Standup Flow with Watches

```
08:00 — Cron fires agent turn with standup prompt
    ↓
Model calls slack_dm (DM to Ra) — tool executes
Model calls slack_dm (DM to Indrek) — tool executes
Model calls watch (register for Ra's reply) — WatchManager.register()
    spawns reminder timer (60min) + expiry timer (240min)
Model calls watch (register for Indrek's reply) — same
    ↓
Turn ends. Session closes.
    ↓
08:12 — Ra replies via Slack DM
    Socket Mode delivers message
    Dispatch loop: watch_manager.check_message() matches
    Prepends watch context to msg.content
    cancel.cancel() kills Ra's timers
    Agent turn: model sees update + watch context
    Turn ends
    ↓
09:00 — Indrek's reminder timer fires (60min after registration)
    Synthetic ChannelMessage injected via tx
    Dispatch loop processes it like any message
    Agent sends reminder DM
    ↓
12:00 — Indrek's expiry timer fires (240min)
    Synthetic ChannelMessage carries on_expire prompt
    Agent reads DM history, posts summary to channel
```

#### Module Dependencies

```
src/watches/           ← rusqlite, tokio, tokio_util::sync::CancellationToken
src/watches/tools.rs   ← watches/mod.rs, tools/traits.rs
src/tools/slack/       ← tools/traits.rs, tokio::process
src/tools/linear/      ← tools/traits.rs, tokio::process
src/providers/gemini.rs ← providers/traits.rs (existing)
src/channels/orchestrator.rs ← watches/mod.rs (new dependency)
```

No circular dependencies. No cross-subsystem coupling.

## Non-Goals

- Keep the shell tool — Rain still needs it for general commands
- Keep the TypeScript CLI scripts unchanged — they remain the execution layer
- Keep Slack and Linear API calls out of Rust — the CLI scripts handle all HTTP
- Keep `ChannelMessage` struct unchanged — watch context prepends to the content string

## Testing Strategy

- **Gemini native tools**: Unit tests for struct serialization/deserialization, functionCall parsing, and multi-turn message conversion. Integration test against a mock HTTP server returning functionCall responses.
- **Typed tools**: Unit tests per tool for parameter validation and subprocess argument construction. Integration tests against a mock CLI script that echoes args as JSON.
- **Watch system**: Unit tests for WatchStore CRUD and matching logic. Integration tests for WatchManager timer behavior using `tokio::time::pause()` for deterministic time control.

## Rollback

Each part reverts independently:
- Part A: revert gemini.rs — runtime falls back to XmlToolDispatcher
- Part B: remove tool registrations — model loses typed tools; the shell tool still works
- Part C: remove watch check from orchestrator — watches stop matching; timers still fire but produce no effect
