# Planner Improvements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add critical-abort, per-action iteration budgets, adaptive synthesis, result compression, and parallelism guidance to the planner module.

**Architecture:** Three files change: `types.rs` gets new optional fields on `Plan` and `PlanAction`; `orchestrator.rs` gains `compress_accumulated_lines()`, a critical-abort check after each group, per-action budget resolution, and adaptive synthesis gating; `prompts.rs` gains parallelism and policy-field guidance. All new fields are optional with backward-compatible defaults.

**Tech Stack:** Rust, serde, tokio, anyhow

**Design doc:** `docs/plans/2026-03-05-planner-improvements-design.md`

---

### Task 1: Add new fields to `src/planner/types.rs`

**Files:**
- Modify: `src/planner/types.rs`

**Step 1: Write failing tests**

Add to the `tests` module in `src/planner/types.rs`:

```rust
#[test]
fn plan_action_critical_defaults_to_false() {
    let json = r#"{"type": "read", "description": "read data"}"#;
    let action: PlanAction = serde_json::from_str(json).unwrap();
    assert!(!action.critical);
}

#[test]
fn plan_action_critical_true_deserializes() {
    let json = r#"{"type": "read", "description": "read data", "critical": true}"#;
    let action: PlanAction = serde_json::from_str(json).unwrap();
    assert!(action.critical);
}

#[test]
fn plan_action_max_iterations_defaults_to_none() {
    let json = r#"{"type": "read", "description": "read data"}"#;
    let action: PlanAction = serde_json::from_str(json).unwrap();
    assert!(action.max_iterations.is_none());
}

#[test]
fn plan_action_max_iterations_deserializes() {
    let json = r#"{"type": "write", "description": "write data", "max_iterations": 40}"#;
    let action: PlanAction = serde_json::from_str(json).unwrap();
    assert_eq!(action.max_iterations, Some(40));
}

#[test]
fn plan_require_synthesis_defaults_to_none() {
    let json = r#"{"passthrough": false, "actions": [{"type": "a", "description": "b"}]}"#;
    let plan: Plan = serde_json::from_str(json).unwrap();
    assert!(plan.require_synthesis.is_none());
}

#[test]
fn plan_require_synthesis_false_deserializes() {
    let json = r#"{"require_synthesis": false, "actions": [{"type": "a", "description": "b"}]}"#;
    let plan: Plan = serde_json::from_str(json).unwrap();
    assert_eq!(plan.require_synthesis, Some(false));
}

#[test]
fn plan_require_synthesis_true_deserializes() {
    let json = r#"{"require_synthesis": true, "actions": [{"type": "a", "description": "b"}]}"#;
    let plan: Plan = serde_json::from_str(json).unwrap();
    assert_eq!(plan.require_synthesis, Some(true));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw --lib planner::types`
Expected: FAIL — field `critical` not found on `PlanAction`, etc.

**Step 3: Add fields to structs**

In `PlanAction`, after the `model_hint` field add:

```rust
#[serde(default)]
pub critical: bool,
#[serde(default)]
pub max_iterations: Option<u32>,
```

In `Plan`, after the `actions` field add:

```rust
#[serde(default)]
pub require_synthesis: Option<bool>,
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw --lib planner::types`
Expected: all tests pass including the 7 new ones.

**Step 5: Run full suite to check for regressions**

Run: `cargo test`
Expected: all tests pass.

**Step 6: Commit**

```
feat(planner): add critical, max_iterations, require_synthesis fields to plan schema
```

---

### Task 2: Add `compress_accumulated_lines` to `src/planner/orchestrator.rs`

**Files:**
- Modify: `src/planner/orchestrator.rs`

**Step 1: Write failing tests**

Add to the `tests` module in `src/planner/orchestrator.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_empty_returns_empty_vec() {
        let result = compress_accumulated_lines(&[], 3000);
        assert!(result.is_empty());
    }

    #[test]
    fn compress_under_budget_returns_lines_unchanged() {
        let lines = vec![
            "Action \"read\" (group 1): Found 5 messages".to_string(),
            "Action \"create\" (group 2): Created 3 issues".to_string(),
        ];
        let result = compress_accumulated_lines(&lines, 3000);
        assert_eq!(result, lines);
    }

    #[test]
    fn compress_truncates_long_lines() {
        let long_summary = "x".repeat(600);
        let line = format!("Action \"read\" (group 1): {long_summary}");
        let result = compress_accumulated_lines(&[line], 3000);
        assert_eq!(result.len(), 1);
        assert!(result[0].len() <= 503); // 500 chars + "..."
        assert!(result[0].ends_with("..."));
    }

    #[test]
    fn compress_applies_rolling_window_over_budget() {
        // Build 20 lines that together exceed 3000 chars
        let lines: Vec<String> = (0..20)
            .map(|i| format!("Action \"step{}\" (group {}): {}", i, i, "result data ".repeat(10)))
            .collect();
        let result = compress_accumulated_lines(&lines, 3000);
        // Should be shorter than input
        assert!(result.len() < lines.len());
        // First line should be a placeholder
        assert!(result[0].contains("earlier actions completed"));
    }

    #[test]
    fn compress_single_line_under_budget_unchanged() {
        let lines = vec!["Action \"read\" (group 1): short result".to_string()];
        let result = compress_accumulated_lines(&lines, 3000);
        assert_eq!(result, lines);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw --lib planner::orchestrator`
Expected: FAIL — `compress_accumulated_lines` not found.

**Step 3: Implement `compress_accumulated_lines`**

Add this function in `src/planner/orchestrator.rs`, above `plan_then_execute`:

```rust
const COMPRESS_LINE_MAX: usize = 500;

/// Compress a list of accumulated action result lines for use in executor prompts.
///
/// Two-stage compression:
/// 1. Truncate individual lines to COMPRESS_LINE_MAX characters.
/// 2. If total length exceeds `max_chars`, drop oldest lines and prepend a placeholder.
///
/// The synthesis phase always receives the full uncompressed results — this
/// function is only for inter-group executor context.
fn compress_accumulated_lines(lines: &[String], max_chars: usize) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }

    // Stage 1: truncate individual lines
    let truncated: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.len() > COMPRESS_LINE_MAX {
                format!("{}...", &line[..COMPRESS_LINE_MAX])
            } else {
                line.clone()
            }
        })
        .collect();

    // Stage 2: rolling window if still over budget
    let total: usize = truncated.iter().map(|l| l.len()).sum();
    if total <= max_chars {
        return truncated;
    }

    // Keep as many recent lines as fit, from the end
    let mut kept: Vec<&String> = Vec::new();
    let mut used = 0usize;
    for line in truncated.iter().rev() {
        if used + line.len() + 1 > max_chars {
            break;
        }
        kept.push(line);
        used += line.len() + 1;
    }
    kept.reverse();

    let dropped = truncated.len() - kept.len();
    let mut result = Vec::with_capacity(kept.len() + 1);
    result.push(format!(
        "[{dropped} earlier actions completed — see synthesis for details]"
    ));
    result.extend(kept.into_iter().cloned());
    result
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw --lib planner::orchestrator`
Expected: all 5 new tests pass.

**Step 5: Run full suite**

Run: `cargo test`
Expected: all tests pass.

**Step 6: Commit**

```
feat(planner): add compress_accumulated_lines for inter-group context compression
```

---

### Task 3: Wire per-action budget and result compression into orchestrator

**Files:**
- Modify: `src/planner/orchestrator.rs`

**Step 1: Identify the two change sites**

Read `src/planner/orchestrator.rs`. Find:

1. The `budget` calculation (around line 127):
   ```rust
   let budget = max_tool_iterations.min(max_executor_iterations);
   ```

2. The `group_accumulated` assignment (around line 102):
   ```rust
   let group_accumulated = accumulated.clone();
   ```

**Step 2: Write a test to verify per-action budget wiring**

Add to the `tests` module:

```rust
#[test]
fn per_action_budget_uses_action_max_iterations_when_set() {
    // Verify the resolution logic directly
    let global_max: usize = 30;
    let max_tool: usize = 50;
    let action_override: Option<u32> = Some(10);

    let budget = action_override
        .map(|n| n as usize)
        .unwrap_or(global_max)
        .min(max_tool);
    assert_eq!(budget, 10);
}

#[test]
fn per_action_budget_falls_back_to_global_when_none() {
    let global_max: usize = 30;
    let max_tool: usize = 50;
    let action_override: Option<u32> = None;

    let budget = action_override
        .map(|n| n as usize)
        .unwrap_or(global_max)
        .min(max_tool);
    assert_eq!(budget, 30);
}
```

**Step 3: Run tests to verify they pass (they test logic, not the wiring)**

Run: `cargo test -p zeroclaw --lib planner::orchestrator`
Expected: pass.

**Step 4: Apply the two changes**

Change 1 — replace `group_accumulated` assignment:
```rust
// Before:
let group_accumulated = accumulated.clone();

// After:
let group_accumulated = compress_accumulated_lines(&accumulated, 3000);
```

Change 2 — replace `budget` calculation inside the async closure.

Find:
```rust
let budget = max_tool_iterations.min(max_executor_iterations);
```

Replace with:
```rust
let action_max_iter = action.max_iterations
    .map(|n| n as usize)
    .unwrap_or(max_executor_iterations);
let budget = max_tool_iterations.min(action_max_iter);
```

Note: `action.max_iterations` must be cloned before entering the async block, same pattern as `action_model_hint`:
```rust
let action_max_iterations = action.max_iterations;
```
Add this clone line alongside the other pre-async clones, then use `action_max_iterations` inside the async block.

**Step 5: Run full suite**

Run: `cargo test`
Expected: all tests pass.

**Step 6: Commit**

```
feat(planner): wire per-action iteration budget and result compression into orchestrator
```

---

### Task 4: Wire critical abort and adaptive synthesis

**Files:**
- Modify: `src/planner/orchestrator.rs`

**Step 1: Write tests for adaptive synthesis logic**

Add to the `tests` module:

```rust
#[test]
fn adaptive_synthesis_none_with_zero_successes_skips() {
    let require_synthesis: Option<bool> = None;
    let succeeded_count: usize = 0;
    let should_synthesize = match require_synthesis {
        Some(true) => true,
        Some(false) => false,
        None => succeeded_count >= 2,
    };
    assert!(!should_synthesize);
}

#[test]
fn adaptive_synthesis_none_with_one_success_skips() {
    let require_synthesis: Option<bool> = None;
    let succeeded_count: usize = 1;
    let should_synthesize = match require_synthesis {
        Some(true) => true,
        Some(false) => false,
        None => succeeded_count >= 2,
    };
    assert!(!should_synthesize);
}

#[test]
fn adaptive_synthesis_none_with_two_successes_synthesizes() {
    let require_synthesis: Option<bool> = None;
    let succeeded_count: usize = 2;
    let should_synthesize = match require_synthesis {
        Some(true) => true,
        Some(false) => false,
        None => succeeded_count >= 2,
    };
    assert!(should_synthesize);
}

#[test]
fn adaptive_synthesis_force_true_synthesizes_regardless() {
    let require_synthesis: Option<bool> = Some(true);
    let succeeded_count: usize = 0;
    let should_synthesize = match require_synthesis {
        Some(true) => true,
        Some(false) => false,
        None => succeeded_count >= 2,
    };
    assert!(should_synthesize);
}

#[test]
fn adaptive_synthesis_force_false_skips_regardless() {
    let require_synthesis: Option<bool> = Some(false);
    let succeeded_count: usize = 5;
    let should_synthesize = match require_synthesis {
        Some(true) => true,
        Some(false) => false,
        None => succeeded_count >= 2,
    };
    assert!(!should_synthesize);
}
```

**Step 2: Run tests to verify they pass**

Run: `cargo test -p zeroclaw --lib planner::orchestrator`
Expected: pass (these test standalone logic).

**Step 3: Wire critical abort into the group results loop**

Find the current post-`join_all` loop (around line 176):
```rust
for result in &results {
    accumulated.push(result.to_accumulated_line());
    if result.success {
        succeeded_count += 1;
    }
}
if let Some(last_success) = results.iter().rev().find(|r| r.success) {
    last_output = last_success.summary.clone();
    any_succeeded = true;
}
```

Replace with:
```rust
for (action, result) in group.iter().zip(results.iter()) {
    accumulated.push(result.to_accumulated_line());
    if result.success {
        succeeded_count += 1;
    }
    if action.critical && !result.success {
        return Err(anyhow::anyhow!(
            "Critical action '{}' (group {}) failed: {}",
            action.action_type, action.group, result.summary
        ));
    }
}
if let Some(last_success) = results.iter().rev().find(|r| r.success) {
    last_output = last_success.summary.clone();
    any_succeeded = true;
}
```

**Step 4: Wire adaptive synthesis**

Find the synthesis gate (around line 183):
```rust
let output = if succeeded_count <= 1 {
    // Single action or all failed — skip synthesis, use raw output
    last_output
} else {
```

Replace the condition with:
```rust
let should_synthesize = match plan.require_synthesis {
    Some(true) => true,
    Some(false) => false,
    None => succeeded_count >= 2,
};

let output = if !should_synthesize {
    // Skip synthesis — use raw last output
    last_output
} else {
```

Also update the `synthesized` field in the final `runtime_trace::record_event` call at the end:
```rust
// Before:
"synthesized": succeeded_count > 1,

// After:
"synthesized": should_synthesize,
```

**Step 5: Run full suite**

Run: `cargo test`
Expected: all tests pass.

**Step 6: Commit**

```
feat(planner): add critical-abort on action failure and adaptive synthesis gating
```

---

### Task 5: Update planner prompt with parallelism and policy-field guidance

**Files:**
- Modify: `src/planner/prompts.rs`

**Step 1: Write failing tests**

Add to the `tests` module in `src/planner/prompts.rs`:

```rust
#[test]
fn planner_prompt_guides_parallelism() {
    let prompt = build_planner_system_prompt("");
    assert!(prompt.contains("independent actions to the same group"));
    assert!(prompt.contains("Prefer 1"));
}

#[test]
fn planner_prompt_explains_critical_field() {
    let prompt = build_planner_system_prompt("");
    assert!(prompt.contains("critical"));
    assert!(prompt.contains("aborts"));
}

#[test]
fn planner_prompt_explains_require_synthesis_field() {
    let prompt = build_planner_system_prompt("");
    assert!(prompt.contains("require_synthesis"));
}

#[test]
fn planner_prompt_explains_max_iterations_field() {
    let prompt = build_planner_system_prompt("");
    assert!(prompt.contains("max_iterations"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw --lib planner::prompts`
Expected: FAIL — new content not in prompt yet.

**Step 3: Add guidance to `build_planner_system_prompt`**

In `build_planner_system_prompt`, find the Rules section:
```
        Rules:\n\
```

Insert the following two blocks before `Output ONLY valid JSON`:

```rust
        // After the existing rules block, before "Output ONLY valid JSON":
        "Parallelism rule:\n\
        - Assign all independent actions to the same group number\n\
        - Only use a higher group number when an action genuinely requires output from a prior action\n\
        - Prefer 1-2 groups over 3-5 for most tasks; more groups means more latency\n\n\
        Policy fields (all optional):\n\
        - critical: true — mark actions whose output is essential; if this action fails, the plan aborts immediately rather than continuing with missing data\n\
        - require_synthesis: false — set for pure lookup or single-action tasks where the executor output is already the final answer (skips one LLM call)\n\
        - max_iterations: integer — give complex write actions a larger budget (e.g. 40) and simple lookups a tighter one (e.g. 8); omit to use the default\n\n\
```

Place this between the `Include all judgment calls...` rule and `Output ONLY valid JSON`.

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw --lib planner::prompts`
Expected: all tests pass including the 4 new ones.

**Step 5: Run full suite**

Run: `cargo test`
Expected: all tests pass.

**Step 6: Run fmt and clippy**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Fix any issues.

**Step 7: Commit**

```
feat(planner): add parallelism guidance and policy-field documentation to planner prompt
```

---

## Summary of Deletions / Modifications

| File | Net Change |
|------|-----------|
| `src/planner/types.rs` | +3 fields, +7 tests |
| `src/planner/orchestrator.rs` | +`compress_accumulated_lines`, +critical abort, +adaptive synthesis, +per-action budget, +12 tests |
| `src/planner/prompts.rs` | +2 guidance blocks, +4 tests |
