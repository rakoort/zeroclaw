# Fix: Planner Route Hint Passed as Literal Model Name

**Date:** 2026-03-02
**Risk tier:** Medium (src/channels behavior change, no security/gateway impact)

## Problem

The channel orchestrator stores `"hint:planner"` as the planner model string
(`orchestrator.rs:2782-2786`) but passes it to a `ReliableProvider` that has no
hint resolution. Only `RouterProvider::resolve()` can strip the `hint:` prefix
and map it to an actual model. The `active_provider` created via
`get_or_create_provider()` is a bare `ReliableProvider`, so `"hint:planner"`
hits the Vertex AI API as a literal model name, returning 400 Bad Request.

**Observed behavior:**

```
WARN zeroclaw::providers::reliable: Non-retryable error
  provider="gemini" model="hint:planner"
  error=Gemini API error (400 Bad Request):
  "Invalid Endpoint name: .../models/hint:planner"

WARN zeroclaw::channels::orchestrator: Planner failed, falling back to tool call loop
```

The planner is silently bypassed on every channel message. The fallback to
`run_tool_call_loop` works, but the planner/executor split never activates.

## Root Cause

`orchestrator.rs:2785-2786` discards the resolved model from `ModelRouteConfig`
and constructs a hint string:

```rust
planner_model: config
    .model_routes
    .iter()
    .find(|r| r.hint == "planner")
    .map(|_| "hint:planner".to_string()),  // ← discards r.model
```

## Architecture Context

The CLI agent path uses `RouterProvider` (via `create_routed_provider`), which
resolves `hint:` prefixes at call time. The channel orchestrator uses bare
`ReliableProvider` instances (via `create_resilient_provider`) with a separate
per-conversation routing mechanism (`ChannelRouteSelection`). These are
orthogonal concerns — task-type routing (`hint:`) vs per-user routing
(`/models`) — but the channel path was never designed to resolve hints.

The fix resolves the model at configuration time, working with the channel
path's existing architecture rather than introducing hint-based routing.

## Fix

One-line change in `orchestrator.rs`:

```rust
// Before:
.map(|_| "hint:planner".to_string()),

// After:
.map(|r| r.model.clone()),
```

## Latent Concern (Not Addressed)

If planner and executor routes specify different providers, the planner call
goes to the executor's provider (since `active_provider` is always created from
`route.provider`). Currently both use `"gemini"` so this is latent. If
different-provider planner configs materialize, fix at that time with full
context on the use case.

## Validation

- `cargo test` — existing planner + orchestrator tests pass
- Manual: configure a `planner` route, send a channel message, confirm planner
  activates instead of falling back to tool call loop
- Check logs: `model=` in planner call should show the actual model name, not
  `"hint:planner"`

## Rollback

Revert the single-line change. Planner returns to silent fallback behavior
(status quo).
