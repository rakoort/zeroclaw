# Execution Tracing Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add tool call arguments to the existing RuntimeTrace `tool_call_result` event and add plan lifecycle events (`plan_start`, `action_start`, `action_end`, `plan_end`) so planner execution is visible in traces.

**Architecture:** All events use the existing `runtime_trace::record_event` global function. No new structs, files, or config. Two files changed: `src/agent/loop_.rs` (enrich existing event) and `src/agent/planner.rs` (four new events).

**Tech Stack:** Rust, serde_json, std::time::Instant, existing `crate::observability::runtime_trace`

**Design doc:** `docs/plans/2026-03-04-execution-tracing-design.md`

---

### Task 1: Enrich `tool_call_result` with arguments and tool_call_id

The existing `record_event("tool_call_result", ...)` at `src/agent/loop_.rs:2612-2626` already logs tool name, iteration, duration, and scrubbed output. Add the missing `arguments` and `tool_call_id` fields.

**Files:**
- Modify: `src/agent/loop_.rs:2612-2626`

**Step 1: Add arguments and tool_call_id to existing payload**

Change the `serde_json::json!` block at line 2620-2625 from:

```rust
serde_json::json!({
    "iteration": iteration + 1,
    "tool": call.name.clone(),
    "duration_ms": outcome.duration.as_millis(),
    "output": scrub_credentials(&outcome.output),
}),
```

to:

```rust
serde_json::json!({
    "iteration": iteration + 1,
    "tool": call.name.clone(),
    "arguments": scrub_credentials(&call.arguments.to_string()),
    "output": scrub_credentials(&outcome.output),
    "duration_ms": outcome.duration.as_millis(),
    "tool_call_id": call.tool_call_id,
}),
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: Success (no new imports needed — `scrub_credentials` and `call.arguments` already in scope)

**Step 3: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "feat(tracing): add arguments and tool_call_id to tool_call_result event"
```

---

### Task 2: Add `plan_start` event for successful plans

**Files:**
- Modify: `src/agent/planner.rs:1-4` (add import)
- Modify: `src/agent/planner.rs:257-263` (add event after passthrough check, before group loop)

**Step 1: Add import for runtime_trace**

At the top of `src/agent/planner.rs`, after line 4 (`use tokio_util::sync::CancellationToken;`), add:

```rust
use crate::observability::runtime_trace;
```

**Step 2: Add `plan_start` event after the passthrough check (line 260) and before the group loop (line 263)**

Insert after `return Ok(PlanExecutionResult::Passthrough);` (line 259) and before `let groups = plan.grouped_actions();` (line 263):

```rust
    let groups = plan.grouped_actions();

    // Emit plan_start trace event
    runtime_trace::record_event(
        "plan_start",
        Some(channel_name),
        Some(provider_name),
        Some(executor_model),
        None, // no turn_id at planner level
        None,
        None,
        serde_json::json!({
            "action_count": plan.actions.len(),
            "group_count": groups.len(),
            "actions": plan.actions.iter().map(|a| serde_json::json!({
                "action_type": &a.action_type,
                "group": a.group,
                "description": &a.description,
            })).collect::<Vec<_>>(),
            "planner_model": planner_model,
            "executor_model": executor_model,
            "max_executor_iterations": max_executor_iterations,
        }),
    );
```

Note: This replaces the existing `let groups = plan.grouped_actions();` at line 263 — move it up before the trace event so `groups.len()` is available.

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 4: Commit**

```bash
git add src/agent/planner.rs
git commit -m "feat(tracing): emit plan_start event with action breakdown"
```

---

### Task 3: Add passthrough tracing (`plan_start` + `plan_end` for passthrough cases)

When the planner returns passthrough (parse error, passthrough flag, or empty actions), emit `plan_start` + `plan_end` so we can see WHY it was passthrough.

**Files:**
- Modify: `src/agent/planner.rs:248-260` (passthrough paths)

**Step 1: Add tracing for parse-error passthrough**

Replace lines 249-254:

```rust
    let plan = match parse_plan_from_response(&response_text) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!("Plan parse failed ({e}), falling back to passthrough");
            return Ok(PlanExecutionResult::Passthrough);
        }
    };
```

with:

```rust
    let plan = match parse_plan_from_response(&response_text) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!("Plan parse failed ({e}), falling back to passthrough");
            runtime_trace::record_event(
                "plan_end",
                Some(channel_name),
                Some(provider_name),
                Some(planner_model),
                None,
                None,
                Some(&e.to_string()),
                serde_json::json!({
                    "passthrough": true,
                    "reason": "parse_error",
                }),
            );
            return Ok(PlanExecutionResult::Passthrough);
        }
    };
```

**Step 2: Add tracing for explicit passthrough / empty actions**

Replace lines 257-260:

```rust
    if plan.is_passthrough() {
        return Ok(PlanExecutionResult::Passthrough);
    }
```

with:

```rust
    if plan.is_passthrough() {
        let reason = if plan.passthrough {
            "passthrough_flag"
        } else {
            "empty_actions"
        };
        runtime_trace::record_event(
            "plan_end",
            Some(channel_name),
            Some(provider_name),
            Some(planner_model),
            None,
            None,
            None,
            serde_json::json!({
                "passthrough": true,
                "reason": reason,
            }),
        );
        return Ok(PlanExecutionResult::Passthrough);
    }
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 4: Commit**

```bash
git add src/agent/planner.rs
git commit -m "feat(tracing): emit plan_end with reason for passthrough cases"
```

---

### Task 4: Add `action_start` and `action_end` events

**Files:**
- Modify: `src/agent/planner.rs:310-357` (inside the async block per action)

**Step 1: Add `std::time::Instant` import**

At the top of `src/agent/planner.rs`, add:

```rust
use std::time::Instant;
```

**Step 2: Add action_start before run_tool_call_loop and action_end after**

Inside the async block (after line 308 `async move {`), add `action_start` before the `run_tool_call_loop` call:

```rust
                async move {
                    let action_started = Instant::now();

                    runtime_trace::record_event(
                        "action_start",
                        Some(channel_name),
                        Some(provider_name),
                        Some(executor_model),
                        None,
                        None,
                        None,
                        serde_json::json!({
                            "action_index": action_index,
                            "action_type": &action_type,
                            "group": action_group,
                            "description": &action_desc,
                            "iteration_budget": budget,
                        }),
                    );

                    let result = crate::agent::loop_::run_tool_call_loop(
```

Note: `action_index` needs to be captured. In the `.map()` closure (line 278), change `.map(|action| {` to `.enumerate().map(|(action_index, action)| {` and clone `action_index` into the async block alongside `action_type`, `action_group`, `action_desc`.

After the match block that produces `ActionResult` (after line 356), add `action_end`:

```rust
                    let action_result = match result {
                        Ok(output) => ActionResult {
                            action_type: action_type.clone(),
                            group: action_group,
                            success: true,
                            summary: output.clone(),
                            raw_output: output,
                        },
                        Err(e) => {
                            tracing::warn!(
                                action_type = action_type.as_str(),
                                group = action_group,
                                budget = budget,
                                description = action_desc.as_str(),
                                "Action execution failed: {e}"
                            );
                            ActionResult {
                                action_type: action_type.clone(),
                                group: action_group,
                                success: false,
                                summary: e.to_string(),
                                raw_output: String::new(),
                            }
                        }
                    };

                    runtime_trace::record_event(
                        "action_end",
                        Some(channel_name),
                        Some(provider_name),
                        Some(executor_model),
                        None,
                        Some(action_result.success),
                        if action_result.success { None } else { Some(&action_result.summary) },
                        serde_json::json!({
                            "action_index": action_index,
                            "action_type": &action_result.action_type,
                            "group": action_result.group,
                            "duration_ms": action_started.elapsed().as_millis(),
                            "output_excerpt": if action_result.success {
                                action_result.summary.chars().take(200).collect::<String>()
                            } else {
                                String::new()
                            },
                        }),
                    );

                    action_result
```

**Step 3: Fix borrow issues**

The async block captures references to `channel_name`, `provider_name`, `executor_model`. These are already `&str` params passed to `plan_then_execute` and are used inside the async block by `run_tool_call_loop`. Since `runtime_trace::record_event` takes `Option<&str>`, no new ownership issues arise — the references have the same lifetime as the existing ones.

**Step 4: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 5: Commit**

```bash
git add src/agent/planner.rs
git commit -m "feat(tracing): emit action_start and action_end events per plan action"
```

---

### Task 5: Add `plan_end` event

**Files:**
- Modify: `src/agent/planner.rs:375-388` (after group loop, before return)

**Step 1: Add plan_end event and plan-level timing**

Add `let plan_started = Instant::now();` right before the `plan_start` event (from Task 2).

Then insert before `Ok(PlanExecutionResult::Executed { ... })` at line 385:

```rust
    let total_actions: usize = groups.iter().map(|g| g.len()).sum();
    let succeeded = accumulated.iter().filter(|line| !line.contains("FAILED")).count();
    let failed = total_actions - succeeded;

    runtime_trace::record_event(
        "plan_end",
        Some(channel_name),
        Some(provider_name),
        Some(executor_model),
        None,
        Some(any_succeeded),
        None,
        serde_json::json!({
            "total_actions": total_actions,
            "succeeded": succeeded,
            "failed": failed,
            "failed_groups": failed_group_ids.iter().collect::<Vec<_>>(),
            "duration_ms": plan_started.elapsed().as_millis(),
            "passthrough": false,
        }),
    );
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 3: Commit**

```bash
git add src/agent/planner.rs
git commit -m "feat(tracing): emit plan_end event with success/failure summary"
```

---

### Task 6: Add tests for tool_call_result enrichment

**Files:**
- Modify: `src/agent/loop_.rs` (existing test module)

**Step 1: Write test verifying tool_call_result includes arguments**

Find the existing test infrastructure in `loop_.rs` tests. Add a test that:
- Sets up RuntimeTrace with a temp directory in rolling mode
- Creates a mock provider returning one tool call
- Runs `run_tool_call_loop`
- Reads the trace file and verifies the `tool_call_result` entry contains `arguments` and `tool_call_id` fields in the payload

```rust
#[tokio::test]
async fn tool_call_result_trace_includes_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("trace.jsonl");
    let cfg = crate::config::ObservabilityConfig {
        backend: "none".into(),
        otel_endpoint: None,
        otel_service_name: None,
        runtime_trace_mode: "full".into(),
        runtime_trace_path: trace_path.to_string_lossy().into(),
        runtime_trace_max_entries: 100,
    };
    crate::observability::runtime_trace::init_from_config(&cfg, tmp.path());

    // Use existing mock provider/tool infrastructure to run a tool call loop
    // that triggers at least one tool execution, then verify the trace file.
    let events = crate::observability::runtime_trace::load_events(
        &trace_path, 100, Some("tool_call_result"), None,
    ).unwrap();

    for event in &events {
        assert!(event.payload.get("arguments").is_some(),
            "tool_call_result should include arguments");
        assert!(event.payload.get("tool_call_id").is_some(),
            "tool_call_result should include tool_call_id");
    }
}
```

Note: This test skeleton needs to use the existing `MockProvider` and mock tools from the test module. Adapt to match the existing test patterns in `loop_.rs`.

**Step 2: Run the test**

Run: `cargo test tool_call_result_trace_includes_arguments`
Expected: PASS

**Step 3: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "test: verify tool_call_result trace includes arguments and tool_call_id"
```

---

### Task 7: Add tests for plan lifecycle events

**Files:**
- Modify: `src/agent/planner.rs` (existing test module)

**Step 1: Write test for plan lifecycle trace events**

Add a test that extends the existing `plan_then_execute_with_actions_returns_executed` test pattern. Initialize RuntimeTrace, run `plan_then_execute` with a 2-action plan, then verify the trace file contains `plan_start`, `action_start`, `action_end`, and `plan_end` events.

```rust
#[tokio::test]
async fn plan_then_execute_emits_trace_events() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("trace.jsonl");
    let cfg = crate::config::ObservabilityConfig {
        backend: "none".into(),
        otel_endpoint: None,
        otel_service_name: None,
        runtime_trace_mode: "full".into(),
        runtime_trace_path: trace_path.to_string_lossy().into(),
        runtime_trace_max_entries: 100,
    };
    crate::observability::runtime_trace::init_from_config(&cfg, tmp.path());

    let provider = MockPlannerProvider {
        responses: Mutex::new(vec![
            crate::providers::ChatResponse {
                text: Some(r#"{"actions": [{"group": 1, "type": "lookup", "description": "Look up data"}]}"#.into()),
                tool_calls: vec![], usage: None, reasoning_content: None, provider_parts: None,
            },
            crate::providers::ChatResponse {
                text: Some("Found the data.".into()),
                tool_calls: vec![], usage: None, reasoning_content: None, provider_parts: None,
            },
        ]),
    };
    let observer = crate::observability::NoopObserver;
    let _ = super::plan_then_execute(
        &provider, "hint:planner", "hint:complex", "System.", "Find data", "",
        &[], &[], &observer, "router", 0.7, 5, 15, "test", None, None, &[],
    ).await.expect("should succeed");

    let all_events = crate::observability::runtime_trace::load_events(
        &trace_path, 100, None, None,
    ).unwrap();

    let event_types: Vec<&str> = all_events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(event_types.contains(&"plan_start"), "should emit plan_start");
    assert!(event_types.contains(&"action_start"), "should emit action_start");
    assert!(event_types.contains(&"action_end"), "should emit action_end");
    assert!(event_types.contains(&"plan_end"), "should emit plan_end");
}
```

**Step 2: Write test for passthrough trace events**

```rust
#[tokio::test]
async fn plan_then_execute_passthrough_emits_plan_end_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let trace_path = tmp.path().join("trace.jsonl");
    let cfg = crate::config::ObservabilityConfig {
        backend: "none".into(),
        otel_endpoint: None,
        otel_service_name: None,
        runtime_trace_mode: "full".into(),
        runtime_trace_path: trace_path.to_string_lossy().into(),
        runtime_trace_max_entries: 100,
    };
    crate::observability::runtime_trace::init_from_config(&cfg, tmp.path());

    let provider = MockPlannerProvider {
        responses: Mutex::new(vec![crate::providers::ChatResponse {
            text: Some(r#"{"passthrough": true}"#.into()),
            tool_calls: vec![], usage: None, reasoning_content: None, provider_parts: None,
        }]),
    };
    let observer = crate::observability::NoopObserver;
    let _ = super::plan_then_execute(
        &provider, "hint:planner", "hint:complex", "System.", "Hello", "",
        &[], &[], &observer, "router", 0.7, 5, 15, "test", None, None, &[],
    ).await.expect("should not error");

    let events = crate::observability::runtime_trace::load_events(
        &trace_path, 100, Some("plan_end"), None,
    ).unwrap();

    assert!(!events.is_empty(), "should emit plan_end for passthrough");
    let payload = &events[0].payload;
    assert_eq!(payload["passthrough"], true);
    assert_eq!(payload["reason"], "passthrough_flag");
}
```

**Step 3: Run all planner tests**

Run: `cargo test --lib planner::tests`
Expected: All PASS

**Step 4: Commit**

```bash
git add src/agent/planner.rs
git commit -m "test: verify plan lifecycle trace events"
```

---

### Task 8: Run full validation

**Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues

**Step 2: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

**Step 3: Full test suite**

Run: `cargo test`
Expected: All pass

**Step 4: Fix any issues and recommit if needed**
