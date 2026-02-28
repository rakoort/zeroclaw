# Service Integrations Design

**Date:** 2026-02-28
**Scope:** Replace script-based Slack/Linear tools and unify Slack channel + tools into a single Integration abstraction
**Status:** Draft

## Problem

ZeroClaw interfaces with external services through two separate, inconsistent patterns:

1. **Channels** handle message transport — receiving and sending messages via the Channel trait (`listen()`, `send()`). The Slack channel manages its own HTTP client, token, rate limiting, and Socket Mode WebSocket.

2. **Script-based tools** wrap TypeScript CLI scripts that the LLM invokes. Each tool call spawns `npx tsx <script>` as a subprocess, passes arguments as CLI flags, and parses stdout as the result.

This split causes five concrete problems:

- **Duplicate Slack connections.** The channel and the script tools maintain independent HTTP clients and tokens. Neither tracks the other's rate-limit state. When tools hammer the API, channel replies slow down silently.
- **Subprocess overhead.** Every script tool call pays Node startup and TypeScript compilation cost (~500ms+) for a single HTTP call.
- **Unstructured errors.** Script tools return raw stdout/stderr. The agent cannot distinguish a rate limit from an auth failure from a network error.
- **Bypassed security.** Native tools participate in SecurityPolicy (rate limiting, allowlists, audit). Script tools bypass all of it.
- **No pattern for new services.** Adding a third service forces a choice between the channel pattern, the script pattern, or something new.

## Design

### The Integration Trait

A new `Integration` trait unifies external service connections. Each integration owns an authenticated API client and exposes tools the LLM can invoke. Integrations that also handle message transport implement the existing `Channel` trait on the same struct.

```rust
#[async_trait]
pub trait Integration: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    async fn health_check(&self) -> bool { true }
    fn as_channel(&self) -> Option<Arc<dyn Channel>> { None }
}
```

- `name()` — identifier for config, logging, and observability
- `tools()` — returns all Tool implementations backed by this integration's client
- `health_check()` — verifies live connection and credentials
- `as_channel()` — returns a Channel implementation if this integration handles message transport. Slack overrides this; Linear does not.

### Module Structure

```
src/integrations/
    mod.rs                — Integration trait, collect_integrations()
    slack/
        mod.rs            — SlackIntegration (implements Integration + Channel)
        client.rs         — SlackClient (HTTP, auth, rate limiting, error types)
        tools.rs          — 9 Tool implementations
    linear/
        mod.rs            — LinearIntegration (implements Integration only)
        client.rs         — LinearClient (GraphQL, auth, rate limiting)
        tools.rs          — 14 Tool implementations
```

### Wiring

The integration factory replaces both the channel factory's Slack entry and the tool registry's script tool entries:

```rust
// In startup/wiring code:
let integrations = integrations::collect_integrations(&config);

for integration in &integrations {
    // Collect tools for the LLM
    all_tools.extend(integration.tools());

    // Register as channel if it handles message transport
    if let Some(channel) = integration.as_channel() {
        channels.push(channel);
    }
}
```

The orchestrator still sees `Arc<dyn Channel>`. The tool registry still sees `Arc<dyn Tool>`. Neither system changes.

### Config

Integrations get their own config section, replacing `channels_config.slack`, `tools.slack_script`, and `tools.linear_script`:

```toml
[integrations.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."           # for Socket Mode
channel_id = "C..."              # default channel for replies
allowed_users = ["U..."]
mention_only = true

[integrations.linear]
api_key = "lin_api_..."
```

## Slack Integration

### SlackClient

The shared API client for all Slack operations — channel and tools alike.

```rust
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
    app_token: String,
    rate_limiter: RateLimiter,
}
```

Public surface:

```rust
impl SlackClient {
    pub async fn api_get(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, SlackApiError>;
    pub async fn api_post(&self, method: &str, body: &Value) -> Result<Value, SlackApiError>;
    pub async fn api_post_multipart(&self, method: &str, form: Form) -> Result<Value, SlackApiError>;
}
```

`method` is the Slack Web API method name (e.g., `"chat.postMessage"`, `"conversations.history"`). The client:

1. Builds the URL: `https://slack.com/api/{method}`
2. Adds the `Authorization: Bearer {bot_token}` header
3. Sends the request
4. Parses the response envelope — Slack returns HTTP 200 with `{"ok": false, "error": "..."}` for business errors
5. On HTTP 429: reads the `Retry-After` header, waits, and retries (up to 3 attempts)
6. Emits an `IntegrationApiCall` observer event with method, success, duration, and retry count

Error types:

```rust
pub enum SlackApiError {
    RateLimited { retry_after: Duration },
    AuthError { error: String },
    ApiError { method: String, error: String },
    Network(reqwest::Error),
}
```

All callers share the rate limiter. When `send()` triggers a 429, tool calls also back off. One budget, one policy.

**Socket Mode** uses `app_token` (not `bot_token`) to call `apps.connections.open`, which returns a WSS URL. The WebSocket connection receives JSON envelopes with an `envelope_id` that must be acknowledged. The client reconnects automatically on disconnect, using exponential backoff.

**File uploads** use Slack's two-step flow: `files.getUploadURLExternal` obtains a presigned URL, the client uploads the file there, then `files.completeUploadExternal` attaches it to a channel or thread. The deprecated `files.upload` endpoint is unused.

### SlackIntegration

```rust
pub struct SlackIntegration {
    client: Arc<SlackClient>,
    config: SlackIntegrationConfig,
    participated_threads: Mutex<HashSet<String>>,
}
```

Implements both `Integration` and `Channel`:

**As Integration:**
- `tools()` returns 9 tools, each holding `Arc<SlackClient>`
- `health_check()` calls `auth.test` to verify the token

**As Channel:**
- `listen()` runs the Socket Mode WebSocket loop, constructs `ChannelMessage` structs, tracks thread participation, enforces allowlists, and runs triage gating
- `send()` calls `self.client.api_post("chat.postMessage", ...)`, chunking long messages
- `health_check()` delegates to `Integration::health_check()`
- `add_reaction()` / `remove_reaction()` call the corresponding Slack API methods

### Slack Tools

Nine tools, each a thin wrapper around `SlackClient`:

| Tool | Slack API method | Parameters |
|------|-----------------|------------|
| `slack_dm` | `chat.postMessage` (to user ID) | `user_id`, `message` |
| `slack_send` | `chat.postMessage` | `channel_id`, `message` |
| `slack_send_thread` | `chat.postMessage` (with `thread_ts`) | `channel_id`, `thread_ts`, `message` |
| `slack_send_file` | `files.getUploadURLExternal` + `files.completeUploadExternal` | `channel_id`, `file_path` |
| `slack_history` | `conversations.history` | `channel_id`, `limit?` |
| `slack_dm_history` | `conversations.history` (on DM channel) | `user_id`, `limit?` |
| `slack_threads` | `conversations.replies` | `channel_id`, `thread_ts`, `limit?` |
| `slack_presence` | `users.getPresence` | `user_id` |
| `slack_react` | `reactions.add` | `channel_id`, `timestamp`, `emoji` |

Each tool:
1. Extracts and validates parameters from the JSON args
2. Calls `self.client.api_get()` or `self.client.api_post()`
3. Returns `ToolResult::success(response)` or propagates the error

No `ritual` / `context` parameters. The existing observer events (`ToolCallStart` records all args) and conversation history capture intent.

### Responsibility Boundaries

| Concern | Location |
|---------|----------|
| HTTP calls, auth headers | SlackClient |
| Rate limit retry | SlackClient |
| Error parsing (`ok: false`) | SlackClient |
| Observer event emission | SlackClient |
| Socket Mode WebSocket | Channel impl on SlackIntegration |
| Envelope acking | Channel impl |
| Thread participation tracking | Channel impl |
| Triage gating | Channel impl |
| Message chunking | Channel impl (send) |
| Allowlist enforcement | Channel impl |
| Individual API operations | Tool implementations |
| Parameter validation | Tool implementations |

## Linear Integration

### LinearClient

Linear uses a single GraphQL endpoint.

```rust
pub struct LinearClient {
    http: reqwest::Client,
    api_key: String,
    rate_limiter: RateLimiter,
}
```

Public surface:

```rust
impl LinearClient {
    pub async fn graphql(&self, query: &str, variables: &Value) -> Result<Value, LinearApiError>;
}
```

The client:

1. POSTs to `https://api.linear.app/graphql` with `Content-Type: application/json`
2. Adds the `Authorization: {api_key}` header — **no Bearer prefix** for personal API keys
3. Parses the response — Linear returns `{"data": ..., "errors": [...]}`. The `errors` array may accompany `data` in partial failures.
4. Tracks rate limit headers: `X-RateLimit-Requests-Remaining`, `X-RateLimit-Requests-Reset` (UTC epoch **milliseconds**, not seconds)
5. On 429: computes wait from the reset timestamp, waits, and retries
6. Emits an `IntegrationApiCall` observer event

Error types:

```rust
pub enum LinearApiError {
    RateLimited { reset_at_ms: u64 },
    AuthError { message: String },
    GraphqlErrors { errors: Vec<LinearGraphqlError> },
    Network(reqwest::Error),
}

pub struct LinearGraphqlError {
    pub message: String,
    pub path: Option<Vec<String>>,
}
```

Rate limit budget: 1,500 requests per hour per API key.

### LinearIntegration

```rust
pub struct LinearIntegration {
    client: Arc<LinearClient>,
}
```

Implements `Integration` only — no Channel impl, no `as_channel()`.

- `tools()` returns 14 tools
- `health_check()` runs `query { viewer { id } }` to verify the API key

### Linear Tools

Fourteen tools, each wrapping a GraphQL query or mutation:

| Tool | Operation | Key parameters |
|------|-----------|---------------|
| `linear_issues` | Query `team.issues` | `team_id`, `limit?`, `state?` |
| `linear_create_issue` | Mutation `issueCreate` | `team_id`, `title`, `description?` |
| `linear_update_issue` | Mutation `issueUpdate` | `issue_id`, `title?`, `description?`, `state_id?`, `assignee_id?` |
| `linear_archive_issue` | Mutation `issueArchive` | `issue_id` |
| `linear_add_comment` | Mutation `commentCreate` | `issue_id`, `body` |
| `linear_teams` | Query `teams` | — |
| `linear_users` | Query `team.members` | `team_id` |
| `linear_projects` | Query `team.projects` | `team_id` |
| `linear_cycles` | Query `team.cycles` | `team_id` |
| `linear_labels` | Query `team.labels` | `team_id` |
| `linear_states` | Query `team.states` | `team_id` |
| `linear_create_label` | Mutation `issueLabelCreate` | `team_id`, `name`, `color?` |
| `linear_create_project` | Mutation `projectCreate` | `team_id`, `name`, `description?` |
| `linear_create_cycle` | Mutation `cycleCreate` | `team_id`, `name`, `start_date`, `end_date` |

List queries use cursor-based pagination. Default limit: 50 (Linear's default). Each tool accepts an optional `limit` parameter.

## Observability

### New Observer Event

```rust
ObserverEvent::IntegrationApiCall {
    integration: String,    // "slack", "linear"
    method: String,         // "chat.postMessage" or "issueCreate"
    success: bool,
    duration_ms: u64,
    error: Option<String>,
    retries: u32,
}
```

`SlackClient` and `LinearClient` emit this event on every API call. Combined with the existing `ToolCallStart` / `ToolCall` events, this provides two layers of visibility: the LLM's tool invocation and the underlying API call.

### Prometheus Metrics

The Prometheus backend handles `IntegrationApiCall`:

```
zeroclaw_integration_api_calls_total{integration, method, success}
zeroclaw_integration_api_duration_seconds{integration, method}
zeroclaw_integration_api_retries_total{integration, method}
```

### Existing Observability Gaps (Fixed in Same Pass)

1. **Router: emit FallbackTriggered.** The event type exists in `traits.rs`, but the router never emits it. Wire it where the router currently calls `tracing::warn!` on fallback.

2. **Prometheus: handle classification and planner events.** Currently ignored. Add:
   ```
   zeroclaw_classification_total{tier}
   zeroclaw_planner_requests_total{model}
   ```

3. **Watch system: add tracing.** `WatchManager` has zero logging. Add `tracing::info!` for watch registration, match, expiry, and cancellation.

## Testing

### Layer 1: Client Unit Tests (wiremock-rs)

Mock Slack and Linear API responses with `wiremock::MockServer`. Each client test starts a mock server on a random port and injects its URL into the client.

**SlackClient (~10 tests):**
- Rate limit: mock 429 with `Retry-After: 2`, verify wait and retry, verify retry count
- Auth error: mock `{"ok": false, "error": "invalid_auth"}`, verify `SlackApiError::AuthError`
- API error: mock `{"ok": false, "error": "channel_not_found"}`, verify `SlackApiError::ApiError`
- Success envelope: mock `{"ok": true, "messages": [...]}`, verify parsed result
- Rate limiter sharing: two concurrent calls, first gets 429, verify second also backs off
- Token header: verify every request includes `Authorization: Bearer {token}`
- Observer event emission: verify `IntegrationApiCall` emitted with correct fields
- Network error: drop the mock server, verify `SlackApiError::Network`

**LinearClient (~5 tests):**
- GraphQL error: mock `{"errors": [{"message": "..."}]}`, verify `LinearApiError::GraphqlErrors`
- Auth header: verify `Authorization: {key}` (no Bearer prefix)
- Rate limit: mock 429 with `X-RateLimit-Requests-Reset` header, verify wait calculation (milliseconds)
- Success: mock `{"data": {"viewer": {"id": "..."}}}`, verify parsed result
- Partial error: mock response with both `data` and `errors`, verify both accessible

### Layer 2: Tool Unit Tests

Test each tool with a mock client returning predefined responses.

**Per tool (~2-3 tests each, ~50 total):**
- Missing required parameter produces error
- Happy path produces correct `ToolResult`
- Client error produces failed `ToolResult`, never a panic

**Schema validity test (1 test covering all tools):**
- Every tool's `parameters_schema()` returns valid JSON Schema

### Layer 3: Integration-Level Tests

**Channel behavior (~10 tests):**
- Socket Mode envelope yields correct `ChannelMessage` fields
- Envelope ack sends correct payload back
- Bot message filtered out
- Disallowed user message dropped
- Thread participation updates tracking state
- Allowlist enforcement edge cases

**End-to-end dispatch flow (~3 tests):**
- Mock SlackIntegration receives message; dispatch loop processes it; mock provider returns tool call; tool executes against mock SlackClient; provider returns reply; `send()` sends the reply
- Same flow, but provider returns plain text (no tool call)
- Tool execution error produces error reply

### Test Infrastructure

```rust
/// Creates a SlackClient pointing at a wiremock MockServer.
fn mock_slack_client(server: &MockServer, bot_token: &str) -> Arc<SlackClient> {
    Arc::new(SlackClient::new_with_base_url(
        bot_token.to_string(),
        String::new(),  // no app_token for tool tests
        server.uri(),
    ))
}
```

Both `SlackClient` and `LinearClient` accept an optional `base_url` override. In production this defaults to `https://slack.com/api` / `https://api.linear.app`. In tests it points to the mock server.

### Test Count Estimate

| Layer | Tests | Coverage |
|-------|-------|----------|
| SlackClient | ~10 | HTTP, auth, rate limits, errors, observer events |
| LinearClient | ~5 | GraphQL, auth, rate limits, partial errors |
| Slack tools (9) | ~20 | Params, happy path, error propagation |
| Linear tools (14) | ~30 | Params, happy path, error propagation |
| Schema validity | ~1 | All tool schemas valid |
| Channel behavior | ~10 | Socket Mode, filtering, threading |
| End-to-end flow | ~3 | Full dispatch chain |
| Observability | ~5 | Correct event emission |
| **Total** | **~84** | |

## What This Replaces

| Current | Replaced by |
|---------|------------|
| `src/channels/slack.rs` | `src/integrations/slack/mod.rs` (Channel impl) |
| `src/tools/slack/` (9 script tools) | `src/integrations/slack/tools.rs` (9 native tools) |
| `src/tools/linear/` (14 script tools) | `src/integrations/linear/tools.rs` (14 native tools) |
| `config.channels_config.slack` | `config.integrations.slack` |
| `config.tools.slack_script` | removed |
| `config.tools.linear_script` | removed |

The Channel trait, orchestrator, dispatch loop, and tool registry remain unchanged. Other channels (Discord, Telegram, etc.) are unaffected.

## What This Does Not Address

- Classifier scoring validation (separate concern)
- Planner execution improvements (separate concern)
- Config schema simplification (separate concern)
- The 23 unused channel implementations (left as-is)

## Migration

1. Build `src/integrations/` alongside existing code
2. Wire integrations into startup, gated behind `config.integrations.*`
3. Verify parity: same tools, same channel behavior, same results
4. Remove `src/channels/slack.rs`, `src/tools/slack/`, `src/tools/linear/`
5. Remove `channels_config.slack`, `tools.slack_script`, `tools.linear_script` from config schema
