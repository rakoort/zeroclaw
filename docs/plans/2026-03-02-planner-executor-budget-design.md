# Fix: Planner Hardcaps Executor Actions at 5 Iterations

**Date:** 2026-03-02
**Risk tier:** Medium (src/agent behavior change, no security/gateway impact)

## Problem

The planner hardcaps each executor action at 5 tool-call iterations
(`max_tool_iterations.min(5)` at `planner.rs:318`). This cap ignores the
user's `max_tool_iterations` config and causes most non-trivial actions to
fail:

```
WARN: Action execution failed: Agent exceeded maximum tool iterations (5) action_type="read_file"
WARN: Action execution failed: Agent exceeded maximum tool iterations (5) action_type="linear_query"
WARN: Action execution failed: Agent exceeded maximum tool iterations (5) action_type="respond"
```

A trivial "are you listening?" message fails when the planner decomposes
it into actions instead of using passthrough.

## Root Cause Analysis

Three problems contribute. Only the first is in scope here:

1. **The `.min(5)` cap is too aggressive.** Most focused actions need 5-15
   iterations. The cap wastes every token spent on failed actions — worse
   for efficiency than a generous budget that succeeds.
2. **The planner over-decomposes simple messages** (prompt/model quality
   — separate concern).
3. **No fallback when an entire plan fails** (resilience — separate
   concern).

## Design

### Change 1: Named constant replaces hardcoded cap

Add a constant to `src/agent/planner.rs`:

```rust
/// Per-action tool-call budget for planner executor actions.
/// Generous enough for focused multi-step actions (read+parse,
/// search+format), tight enough to prevent runaway loops.
const MAX_EXECUTOR_ACTION_ITERATIONS: usize = 15;
```

Line 318 becomes:

```rust
max_tool_iterations.min(MAX_EXECUTOR_ACTION_ITERATIONS),
```

The `.min()` still caps per-action spend when `max_tool_iterations` is
large. When the user sets a lower value (e.g., 10), their config wins.

### Change 2: Richer failure logging

Current log (`planner.rs:336`):

```
Action execution failed: {e}  action_type="read_file" group=1
```

Add `budget` and `description` as structured fields:

```
Action execution failed  action_type="read_file" group=1 budget=15
    error="Agent exceeded maximum tool iterations (15)"
    description="Read SOUL.md from workspace"
```

### Change 3: Plan-level failure summary

After the group loop, if no action produced output (`last_output` is
empty), log a WARN summarizing the failed plan: total action count, failed
groups, and a hint that passthrough may have been appropriate.

## Token Efficiency Rationale

- A failed action wastes every token spent on it. Three actions failing at
  5 iterations burn more tokens than one action succeeding at 12.
- The planner's passthrough path handles simple messages at zero executor
  cost. The budget cap matters only for genuinely complex actions — which
  need the headroom.
- Early termination still applies: actions that produce a final response
  stop immediately. A generous cap does not mean generous spend.

## Testing

- Unit test: confirm the constant is applied (not 5)
- Existing planner parse/passthrough tests pass unchanged

## Non-Goals

- No new config field (YAGNI — promote to config if operators request it)
- No plan-failure fallback to passthrough (separate enhancement)
- No planner prompt improvements (separate concern)

## Rollback

Revert the commit. Behavior returns to `.min(5)`.
