# Planner Improvements Design

**Date:** 2026-03-05
**Slug:** `planner-improvements`
**Status:** Design

---

## Problem

Three issues observed in production after the planner module refactor:

1. **Reliability** — when an action fails (provider error, iteration cap), the orchestrator logs a warning and continues with missing data. There is no way for the planner to mark an action as load-bearing, so critical steps fail silently.

2. **Token efficiency** — the planner always runs a synthesis phase regardless of task complexity. For single-action or pure lookup tasks, synthesis adds a full LLM call with no user-visible benefit. Accumulated results also grow unbounded across groups, bloating executor prompts.

3. **Latency** — independent lookup actions are often assigned to sequential groups by the planner LLM. They could run in parallel in the same group, halving wall-clock time for lookup-heavy plans.

---

## Scope

Two changes:

- **A — Enriched Plan Schema:** New optional fields on `Plan` and `PlanAction` give the planner LLM semantic control over execution policy.
- **B — Result Compression + Prompt Parallelism Guidance:** Compress accumulated results between groups to prevent context bloat; update the planner prompt to encourage maximum parallelism.

Out of scope: explicit action dependency graphs (`after: Vec<String>`), rescue planning on failure, per-provider retry policy changes.

---

## Design

### A — Plan Schema Enrichment

#### New fields

```rust
pub struct PlanAction {
    // ... existing fields unchanged ...

    /// If true, the orchestrator aborts the entire plan when this action fails.
    /// Default: false (fail silently, continue).
    #[serde(default)]
    pub critical: bool,

    /// Per-action override for the tool iteration budget.
    /// Default: None (use global max_executor_iterations).
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

pub struct Plan {
    // ... existing fields unchanged ...

    /// Controls whether the synthesis phase runs.
    /// None  = auto: synthesize if ≥2 actions succeeded.
    /// true  = always synthesize.
    /// false = skip synthesis, return last executor output directly.
    #[serde(default)]
    pub require_synthesis: Option<bool>,
}
```

All fields are optional with backward-compatible defaults. Existing plans without these fields behave identically to today.

#### Orchestrator changes

**Critical abort.** After each action result is collected:

```rust
if action.critical && !result.success {
    return Err(anyhow::anyhow!(
        "Critical action '{}' (group {}) failed: {}",
        action.action_type, action.group, result.summary
    ));
}
```

The caller (`Agent`, cron scheduler, channel orchestrator) already handles `Err` from `plan_then_execute` by falling back to the flat tool loop — no caller changes needed.

**Per-action iteration budget.** When dispatching each executor:

```rust
let iterations = action.max_iterations
    .unwrap_or(max_executor_iterations);
```

Lets the planner assign tight budgets to simple lookups (e.g., `max_iterations: 8`) and larger budgets to complex write actions (e.g., `max_iterations: 40`).

**Adaptive synthesis.** Replace the current `succeeded_count <= 1` heuristic:

```rust
let should_synthesize = match plan.require_synthesis {
    Some(true)  => true,
    Some(false) => false,
    None        => succeeded_count >= 2,
};
```

---

### B — Result Compression + Prompt Guidance

#### Result compression

Add `compress_accumulated(results: &[ActionResult], max_chars: usize) -> String` in `orchestrator.rs`.

Two stages, applied before building executor prompts for each group:

1. **Per-result truncation (always).** Each `ActionResult.summary` is capped at 500 characters in the accumulated context string. Full output is retained on the struct for synthesis.

2. **Rolling window (when over budget).** If the full accumulated string exceeds `max_chars` (default: 3000 characters), drop the oldest results and prepend a placeholder:

   ```
   [N earlier actions completed — see synthesis for details]
   <most recent results...>
   ```

   The synthesis phase always receives the full uncompressed `Vec<ActionResult>` — only the inter-group executor context is compressed.

#### Planner prompt additions

Two additions to `build_planner_system_prompt()`:

**Parallelism guidance:**
> Assign all independent actions to the same group number. Only use a higher group number when an action genuinely requires output from a prior action. Prefer 1–2 groups over 3–5 for most tasks.

**Policy field guidance:**
> - `critical: true` — mark actions whose output is essential. If a critical action fails, the plan aborts rather than continuing with missing data.
> - `require_synthesis: false` — set for pure lookup or single-action tasks where the executor output is already the final answer.
> - `max_iterations` — give complex write actions a larger budget (e.g., 40) and simple lookups a tighter one (e.g., 8).

---

## Files Changed

| File | Change |
|------|--------|
| `src/planner/types.rs` | Add `critical`, `max_iterations` to `PlanAction`; add `require_synthesis` to `Plan` |
| `src/planner/orchestrator.rs` | Critical abort path, per-action iteration budget, adaptive synthesis, `compress_accumulated()` |
| `src/planner/prompts.rs` | Parallelism instruction, policy field guidance |

---

## Risk and Rollback

**Risk: Low.** All new fields are optional with defaults that preserve existing behavior. The critical abort path is gated on `critical: true`, which the planner LLM will not set unless explicitly trained to do so via the prompt. Result compression only activates above a character threshold.

**Rollback:** Revert the three files. No config or schema migrations required.

---

## Testing Strategy

- Unit tests for `compress_accumulated` covering: under budget (no-op), over budget (rolling window), single result.
- Unit tests for adaptive synthesis logic: `None` with 0/1/2 successes, `Some(true)`, `Some(false)`.
- Unit test for critical abort: mock failing critical action → expect `Err`.
- Unit tests for per-action budget resolution: `None` falls back to global, `Some(n)` overrides.
- Updated planner prompt tests to cover new guidance sections.
