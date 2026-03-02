# Planner Executor Budget Fix — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the hardcoded `.min(5)` executor budget cap with a named constant of 15 and add diagnostic logging for action failures.

**Architecture:** Single-file change in `src/agent/planner.rs`. Add a constant, update one call site, enrich two log points. No new config fields, no API changes.

**Tech Stack:** Rust, tracing (structured logging)

---

### Task 1: Add the named constant and replace the hardcoded cap

**Files:**
- Modify: `src/agent/planner.rs:8-9` (add constant after imports)
- Modify: `src/agent/planner.rs:318` (replace `.min(5)`)

**Step 1: Add constant after the imports block (after line 9)**

```rust
/// Per-action tool-call budget for planner executor actions.
/// Generous enough for focused multi-step actions (read+parse, search+format),
/// tight enough to prevent runaway loops.
const MAX_EXECUTOR_ACTION_ITERATIONS: usize = 15;
```

**Step 2: Replace the hardcoded cap at line 318**

Change:
```rust
max_tool_iterations.min(5),
```

To:
```rust
max_tool_iterations.min(MAX_EXECUTOR_ACTION_ITERATIONS),
```

**Step 3: Run `cargo check` to verify compilation**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles with no errors

**Step 4: Run existing planner tests**

Run: `cargo test -p zeroclaw planner -- --nocapture 2>&1 | tail -20`
Expected: all existing tests pass

**Step 5: Commit**

```
fix(planner): raise executor action budget from 5 to 15 iterations

The hardcoded .min(5) cap caused every non-trivial executor action to
fail with "exceeded maximum tool iterations (5)", wasting all tokens
spent on the planner call and failed actions. Replace with a named
constant (MAX_EXECUTOR_ACTION_ITERATIONS = 15) that still caps
per-action spend while letting focused actions complete.
```

---

### Task 2: Enrich action failure logging with budget and description

**Files:**
- Modify: `src/agent/planner.rs:298-301` (clone description for async block)
- Modify: `src/agent/planner.rs:336-340` (enrich warn log)

**Step 1: Clone the action description alongside the existing metadata clones**

After the existing clones at lines 300-301:
```rust
let action_type = action.action_type.clone();
let action_group = action.group;
```

Add:
```rust
let action_desc = action.description.clone();
```

Also capture the budget value before the async block. After `let ct = cancellation_token.clone();` add:
```rust
let budget = max_tool_iterations.min(MAX_EXECUTOR_ACTION_ITERATIONS);
```

**Step 2: Update the warn log at line 336**

Change:
```rust
tracing::warn!(
    action_type = action_type.as_str(),
    group = action_group,
    "Action execution failed: {e}"
);
```

To:
```rust
tracing::warn!(
    action_type = action_type.as_str(),
    group = action_group,
    budget = budget,
    description = action_desc.as_str(),
    "Action execution failed: {e}"
);
```

**Step 3: Run `cargo check`**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles with no errors

**Step 4: Run planner tests**

Run: `cargo test -p zeroclaw planner -- --nocapture 2>&1 | tail -20`
Expected: all tests pass

**Step 5: Commit**

```
fix(planner): add budget and description to action failure logs

Include the iteration budget and action description as structured
fields in the warn log so operators can diagnose why actions fail
without guessing which action hit the cap.
```

---

### Task 3: Add plan-level failure summary log

**Files:**
- Modify: `src/agent/planner.rs:362-367` (add warn before return when no action succeeded)

**Step 1: Add plan-failure summary after the group loop**

After the closing `}` of the `for group in &groups` loop (line 362), before the `Ok(PlanExecutionResult::Executed { ... })` return, add:

```rust
if last_output.is_empty() {
    let total_actions = groups.iter().map(|g| g.len()).sum::<usize>();
    let failed_groups: Vec<u32> = groups
        .iter()
        .flat_map(|g| g.iter().map(|a| a.group))
        .collect::<std::collections::BTreeSet<u32>>()
        .into_iter()
        .collect();
    tracing::warn!(
        total_actions = total_actions,
        failed_groups = ?failed_groups,
        "All plan actions failed; consider whether this request should passthrough"
    );
}
```

**Step 2: Run `cargo check`**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles with no errors

**Step 3: Run planner tests**

Run: `cargo test -p zeroclaw planner -- --nocapture 2>&1 | tail -20`
Expected: all tests pass

**Step 4: Commit**

```
fix(planner): log summary when all plan actions fail

When no action in a plan produces output, log the total action count
and which groups failed. This surfaces plan-level failures that would
otherwise go unnoticed as individual action warnings.
```

---

### Task 4: Add a unit test for the executor budget constant

**Files:**
- Modify: `src/agent/planner.rs` (add test in existing `#[cfg(test)] mod tests`)

**Step 1: Write the test**

Add to the existing test module:

```rust
#[test]
fn executor_action_budget_is_not_hardcoded_to_five() {
    // Guard: the constant must be greater than the old hardcoded cap of 5.
    assert!(
        super::MAX_EXECUTOR_ACTION_ITERATIONS > 5,
        "MAX_EXECUTOR_ACTION_ITERATIONS should be greater than 5, got {}",
        super::MAX_EXECUTOR_ACTION_ITERATIONS
    );
    // Verify the constant is at a reasonable level (not unbounded).
    assert!(
        super::MAX_EXECUTOR_ACTION_ITERATIONS <= 50,
        "MAX_EXECUTOR_ACTION_ITERATIONS should be at most 50, got {}",
        super::MAX_EXECUTOR_ACTION_ITERATIONS
    );
}
```

**Step 2: Run the new test**

Run: `cargo test -p zeroclaw planner::tests::executor_action_budget -- --nocapture 2>&1 | tail -10`
Expected: PASS

**Step 3: Run full test suite to verify no regressions**

Run: `cargo test -p zeroclaw planner -- --nocapture 2>&1 | tail -20`
Expected: all tests pass

**Step 4: Commit**

```
test(planner): add guard test for executor action budget constant

Ensures the per-action budget stays above the old hardcoded cap of 5
and below a reasonable upper bound, preventing silent regressions.
```
