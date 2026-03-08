# Integration Observability Design

**Date:** 2026-03-07
**Status:** Approved
**Linear:** SPO-74
**Slug:** `integration-observability`

## Problem

ZeroClaw has comprehensive observability for LLM provider calls (retries,
fallbacks, timing, token counts) but integration API calls are black boxes.
`IntegrationApiCall` is defined in `ObserverEvent` and handled by `LogObserver`,
but emitted zero times. HTTP status codes, per-integration timing, response
sizes, and rate-limit wait durations are invisible at runtime.

## Solution

Wire `IntegrationApiCall` events into all three integration clients via
construction-time observer injection. Extend the event schema with HTTP status,
response size, and rate-limit wait fields.

## Design Decisions

### 1. Extended Event Schema

Add three fields to the existing `IntegrationApiCall` variant:

```rust
IntegrationApiCall {
    integration: String,              // existing
    method: String,                   // existing
    success: bool,                    // existing
    duration_ms: u64,                 // existing
    error: Option<String>,            // existing
    retries: u32,                     // existing
    status_code: Option<u16>,         // NEW
    response_size_bytes: Option<u64>, // NEW
    rate_limit_wait_ms: Option<u64>,  // NEW
}
```

- `status_code`: HTTP response status. `Option` because non-HTTP integrations
  may exist in the future.
- `response_size_bytes`: Body size before any truncation. Surfaces large-payload
  and truncation events.
- `rate_limit_wait_ms`: Total accumulated rate-limit sleep across retries.
  `None` when no rate limiting occurred.

### 2. Observer Threading — Construction-Time Injection

Observer reaches integration clients via constructor DI, not via
`Tool::execute()` signature changes:

```
Agent Loop (owns Arc<dyn Observer>)
  -> collect_integrations(config, observer.clone())
    -> GitHubIntegration::new(config, observer.clone())
      -> GitHubClient { http, observer }
    -> LinearIntegration::new(config, observer.clone())
      -> LinearClient { http, observer }
    -> SlackIntegration::new(config, observer.clone())
      -> SlackClient { http, observer }
```

Rationale: Matches ZeroClaw's existing DI pattern. No tool trait changes, no
blast radius beyond integration constructors. Observer is required (not
optional) to prevent blind spots from recurring.

### 3. Emission Points

Each client emits one `IntegrationApiCall` event per HTTP call, after the retry
loop resolves:

- **GitHubClient::graphql()** — method: `"graphql"`
- **LinearClient::graphql()** — method: `"graphql"`
- **SlackClient::api_post()** — method: the Slack API method string
- **SlackClient::api_get()** — method: the Slack API method string

Timing wraps the entire retry loop (including rate-limit sleeps).
`rate_limit_wait_ms` accumulates across retries. Existing `debug!` rate-limit
logs are preserved (they serve real-time diagnostics).

### 4. LogObserver Update

The existing handler adds the three new fields as structured tracing fields.
Success path logs at `info!`, failure path at `warn!`. Fields default to `0`
when `None` to keep log lines parseable.

### 5. OtelObserver / PrometheusObserver

Remain no-ops. Destructure new fields to suppress compiler warnings. Spans and
counters are a follow-up PR.

## Scope Boundaries

**In scope:**

- Extend `IntegrationApiCall` with 3 new fields
- Inject `Arc<dyn Observer>` into integration constructors and clients
- Emit events in GitHub, Linear, Slack clients
- Update LogObserver handler
- Suppress-destructure in Otel/Prometheus handlers
- Tests for all of the above

**Not in scope:**

- `HttpRequestTool` observability (different concern — user-facing tool)
- OtelObserver spans / PrometheusObserver metrics (follow-up PR)
- `Tool::execute()` signature changes (not needed)
- New config keys or feature flags (observability is always-on)
- Response body logging (security risk — only size captured)

## Testing

- **Per-client unit tests:** MockObserver captures events. Verify success,
  failure, and rate-limited call paths emit correct fields.
- **LogObserver tests:** Verify new fields render in both success/failure arms.
- **Integration wiring test:** `collect_integrations()` receives observer,
  constructs client, client event reaches observer.
- **No changes to tool or agent loop tests** (signatures unchanged).

## Rollback

Revert the single PR. Observer injection is additive (new constructor params).
Event schema change is backward-compatible (new fields are `Option`). No
behavior change to tool execution or agent loop.

## Files Affected

- `src/observability/traits.rs` — event schema
- `src/observability/log.rs` — LogObserver handler
- `src/observability/otel.rs` — destructure update
- `src/observability/prometheus.rs` — destructure update
- `src/integrations/mod.rs` — factory wiring
- `src/integrations/github/client.rs` — observer field + emission
- `src/integrations/github/mod.rs` — constructor change
- `src/integrations/linear/client.rs` — observer field + emission
- `src/integrations/linear/mod.rs` — constructor change
- `src/integrations/slack/client.rs` — observer field + emission
- `src/integrations/slack/mod.rs` — constructor change
