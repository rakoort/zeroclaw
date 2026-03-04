# Execution Tracing Design

**Date:** 2026-03-04
**Slug:** `execution-tracing`
**Status:** Draft

## Problem

Rain wastes 40–60% of its tool iterations on manual logging. After every mutation tool call, the LLM formats a JSON entry, calls a tool to append it, and sometimes retries. A standup response hit the 30-iteration cap just trying to log itself.

The root cause: zeroclaw's engine records LLM-level events in RuntimeTrace (requests, responses, parse issues) but never records individual tool call results. The planner/executor has an even larger gap — plan execution is invisible in traces. You cannot distinguish a planned multi-step execution from a flat sequence of unrelated tool calls.

## Solution

Add six RuntimeTrace event types that capture tool execution and plan lifecycle. All events use the existing `runtime_trace::record_event` infrastructure — same JSONL file, same rolling/full mode, same config. No new files, structs, or config knobs.

## Event Hierarchy

```
plan_start                — plan parsed, about to execute
  action_start            — single action beginning
    tool_call_result      — individual tool execution
  action_end              — single action completed
plan_end                  — all actions done

tool_call_result          — also emitted outside plans (normal flow)
```

When the planner is not called, only `tool_call_result` events appear — a flat sequence with full context.

## Event Schemas

### `tool_call_result`

Emitted after every tool execution in `run_tool_call_loop()`.

```json
{
  "event_type": "tool_call_result",
  "channel": "slack",
  "provider": "gemini",
  "model": "gemini-2.0-flash",
  "turn_id": "e5f6g7h8-...",
  "success": true,
  "message": null,
  "payload": {
    "iteration": 3,
    "tool": "slack_send",
    "arguments": "{\"channel_id\":\"C0AG29ZDQUC\",\"message\":\"Good morning\"}",
    "output": "{\"ok\":true,\"ts\":\"1709537112.001234\"}",
    "duration_ms": 245,
    "tool_call_id": "call_abc123"
  }
}
```

Arguments and output are scrubbed via `scrub_credentials()`.

### `plan_start`

Emitted after parsing the plan JSON, before entering the group execution loop.

```json
{
  "event_type": "plan_start",
  "payload": {
    "action_count": 4,
    "group_count": 2,
    "actions": [
      {"action_type": "send_standup", "group": 1, "description": "Send standup to #general"},
      {"action_type": "create_issue", "group": 1, "description": "Create follow-up issue"},
      {"action_type": "dm_user", "group": 2, "description": "DM blocker owner"},
      {"action_type": "log_summary", "group": 2, "description": "Post summary"}
    ],
    "planner_model": "gemini-2.0-flash",
    "executor_model": "gemini-2.0-flash",
    "max_executor_iterations": 30
  }
}
```

### `action_start`

Emitted before each action's `run_tool_call_loop` call.

```json
{
  "event_type": "action_start",
  "payload": {
    "action_index": 0,
    "action_type": "send_standup",
    "group": 1,
    "description": "Send standup to #general",
    "iteration_budget": 30
  }
}
```

### `action_end`

Emitted after each action completes.

```json
{
  "event_type": "action_end",
  "success": true,
  "payload": {
    "action_index": 0,
    "action_type": "send_standup",
    "group": 1,
    "duration_ms": 1820,
    "output_excerpt": "Posted standup to #general"
  }
}
```

### `plan_end`

Emitted after all groups finish, before building `PlanExecutionResult`.

```json
{
  "event_type": "plan_end",
  "payload": {
    "total_actions": 4,
    "succeeded": 3,
    "failed": 1,
    "failed_groups": [2],
    "duration_ms": 8450,
    "passthrough": false
  }
}
```

For passthrough plans (parse error, passthrough flag, empty actions), only `plan_start` + `plan_end` are emitted:

```json
{
  "event_type": "plan_end",
  "payload": {
    "passthrough": true,
    "reason": "parse_error"
  }
}
```

## Code Changes

### File 1: `src/agent/loop_.rs`

One `record_event` call after the existing `fire_after_tool_call` hook, inside the tool result processing block. ~12 lines.

Data available at call site: `call.name`, `call.arguments`, `call.tool_call_id`, `outcome.output`, `outcome.success`, `outcome.error_reason`, `outcome.duration`, `turn_id`, `iteration`, `channel_name`, `provider_name`, `model`.

### File 2: `src/agent/planner.rs`

Four `record_event` calls:

1. **`plan_start`** — after parsing plan JSON, before group loop. Includes action list, group count, models. For passthrough cases, emit `plan_start` + `plan_end` with reason and return early. ~20 lines.
2. **`action_start`** — before each action's `run_tool_call_loop` call. ~15 lines.
3. **`action_end`** — after each action completes. Captures duration via `Instant::now()`. ~15 lines.
4. **`plan_end`** — after group loop, before building `PlanExecutionResult`. Tallies success/failure from accumulated results. ~15 lines.

**Total: ~75 lines of new code across 2 files. No new files, structs, or config.**

## What This Does NOT Change

- RuntimeTraceEvent struct (reuses existing schema)
- RuntimeTrace config (existing `runtime_trace_mode` and `runtime_trace_path` govern everything)
- Hook signatures or observer events
- Tool execution flow
- Planner logic

## Known Limitations

- **CLI-only path**: `Agent::turn()` in `agent.rs` has its own tool loop that bypasses `run_tool_call_loop()`. Tool calls through that path are not traced. This path is CLI-only; Rain runs via channels.
- **Delegate sub-agents**: `DelegateTool::execute_agentic()` calls `run_tool_call_loop()` with `NoopObserver`, but `runtime_trace::record_event` is global — delegate tool calls ARE traced. However, they lack a plan context wrapper.

## Testing

1. **`tool_call_result` emission** — run `run_tool_call_loop` with a mock provider returning one tool call. Verify trace file contains entry with expected fields.
2. **Plan lifecycle events** — call `plan_then_execute` with a mock 2-action plan. Verify trace contains `plan_start`, two `action_start`/`action_end` pairs, and `plan_end` with correct counts.
3. **Passthrough tracing** — call `plan_then_execute` with a passthrough plan. Verify `plan_start` + `plan_end` with `passthrough: true` and reason.
4. **Credential scrubbing** — verify `tool_call_result` arguments and output are scrubbed.

## Impact

| Fix | Iterations saved | Latency reduction |
|-----|-----------------|-------------------|
| Tool call logging | 5–30 per interaction (eliminates manual log_action) | 30–120s |
| Plan execution tracing | 0 (observability, not iteration savings) | Enables targeted optimization |
