# Gemini Provider Simplification

**Date:** 2026-03-03
**Status:** Approved
**Scope:** `src/providers/gemini.rs`, `src/providers/traits.rs`, `src/agent/planner.rs`, `src/agent/loop_.rs`, `src/config/schema.rs`

## Problem

Recent feature additions to the Gemini provider introduced complexity that now causes production failures and diverges from current Gemini API conventions:

1. **`required_tool_names` causes 400 errors.** The planner threads tool names through `ChatRequest` into Gemini's `allowedFunctionNames`. Mismatches between these names and the actual `functionDeclarations` produce non-retryable 400 responses. All executor actions fail; the user gets empty replies.

2. **Thinking config ignores Gemini 3 models.** The provider always sends `thinkingBudget` (a Gemini 2.5 parameter). Gemini 3 models expect `thinkingLevel`. The docs warn that `thinkingBudget` on Gemini 3 "may lead to unexpected performance."

3. **Tool results use the wrong role.** The provider sends `functionResponse` parts with `role: "user"` and merges consecutive results to maintain strict user/model alternation. The Gemini API supports `role: "tool"` natively, which may eliminate the merge workaround.

4. **No way to opt into VALIDATED mode.** Gemini offers a `VALIDATED` function-calling mode (preview) that guarantees schema adherence without forcing a call. The provider has no way to use it.

## Design

Five changes, ordered by risk (lowest first).

### 1. Remove `required_tool_names`

**Delete the field** from `ChatRequest`, `run_tool_call_loop`, and all call sites.

**Simplify `build_tool_config_for_request`** to take a single `force_tool_call: bool`:

```rust
fn build_tool_config_for_request(
    has_tools: bool,
    force_tool_call: bool,
    tool_call_mode: Option<&str>,
) -> Option<GeminiToolConfig> {
    if !has_tools {
        return None;
    }
    let mode = if force_tool_call {
        "ANY"
    } else {
        match tool_call_mode {
            Some("validated") => "VALIDATED",
            _ => "AUTO",
        }
    };
    Some(GeminiToolConfig {
        function_calling_config: FunctionCallingConfigMode { mode: mode.into() },
    })
}
```

**Drop `allowed_function_names`** from `FunctionCallingConfigMode`. The field is no longer needed: `ANY` without names forces a call from any declared function.

**Planner keeps its tool filtering.** `filter_tool_names` and `combined_excluded` still control which tools the executor sees via `tool_specs`. The change removes the redundant constraint at the Gemini API level.

**Signal for force_tool_call:** The planner executor path passes `force_tool_call: true` to the provider. The normal agent loop passes `false`. This replaces the `required_tool_names` threading with a single boolean.

Mechanism: add `force_tool_call: bool` to `ChatRequest` (replacing `required_tool_names`). The planner sets it to `true` when `wanted_tools` is non-empty. The agent loop sets it to `false`. Other providers ignore it.

### 2. Model-aware thinking config

**Add a helper** to detect the model family:

```rust
fn is_gemini3_model(model: &str) -> bool {
    let normalized = model.strip_prefix("models/").unwrap_or(model);
    normalized.starts_with("gemini-3")
}
```

**Branch `thinking_config_for_hint`** on model family:

| Route hint | Gemini 2.5 (`thinkingBudget`) | Gemini 3 (`thinkingLevel`) |
|---|---|---|
| triage / heartbeat / simple | 0 (disable) | "minimal" |
| planner / medium | 1024 | "low" |
| complex | 4096 | "medium" |
| reasoning | -1 (dynamic) | "high" |

Only one field is `Some` per request. The other stays `None` and is skipped by `skip_serializing_if`.

### 3. Switch tool result role to `"tool"`

**Change** `role: Some("user".into())` to `role: Some("tool".into())` for `functionResponse` content entries.

**Keep the consecutive-merge logic initially.** If testing confirms Gemini handles multiple `role: "tool"` entries without alternation issues, remove the merge logic in a follow-up commit within this PR.

**Revert path:** If any auth path (API key, OAuth, Vertex) rejects `role: "tool"`, revert this single commit. The rest of the PR is unaffected.

### 4. Improve `generationConfig` retry observability

**Keep the existing retry logic** (`should_retry_oauth_without_generation_config`). The cloudcode-pa endpoint may still reject `generationConfig` for some OAuth token scopes. Removing this safety net risks breaking CLI OAuth users.

**Upgrade logging** from `tracing::warn!` to include a structured counter field:

```rust
tracing::warn!(
    retry_reason = "generation_config_rejected",
    "Gemini OAuth endpoint rejected generationConfig; retrying without it"
);
```

This makes the retry queryable in structured log systems. If it never fires, we remove the workaround in a future cleanup.

### 5. VALIDATED mode behind config

**Add** an optional `tool_call_mode` field to the Gemini provider config. Values: `"auto"` (default), `"validated"`.

**Behavior:** When `force_tool_call` is false and `tool_call_mode` is `"validated"`, the provider sends `mode: "VALIDATED"` instead of `mode: "AUTO"`. This guarantees schema adherence on function calls without forcing one.

**Default is `"auto"`** (current behavior). Users opt in by setting `tool_call_mode: "validated"` in their config. If VALIDATED causes issues, they remove the field.

## Files Changed

| File | Changes |
|---|---|
| `src/providers/traits.rs` | Replace `required_tool_names` with `force_tool_call: bool` on `ChatRequest` |
| `src/providers/gemini.rs` | Simplify `build_tool_config_for_request`, model-aware thinking, tool role, logging, VALIDATED support |
| `src/providers/reliable.rs` | Update `ChatRequest` construction (drop old field, add new) |
| `src/agent/planner.rs` | Remove `required` variable, pass `force_tool_call: true` |
| `src/agent/loop_.rs` | Replace `required_tool_names` param with `force_tool_call`, pass through to `ChatRequest` |
| `src/agent/agent.rs` | Set `force_tool_call: false` in normal agent loop |
| `src/config/schema.rs` | Add optional `tool_call_mode` to Gemini provider config |

## Testing

1. **Existing tests pass** — `cargo test` with no regressions.
2. **`build_tool_config_for_request` unit tests** — Verify AUTO/ANY/VALIDATED mode selection.
3. **`thinking_config_for_hint` unit tests** — Verify correct field per model family.
4. **`is_gemini3_model` unit tests** — Cover `gemini-3-flash`, `models/gemini-3-pro`, `gemini-2.5-flash`, edge cases.
5. **Tool role integration** — Manual test with each auth path (API key, OAuth, Vertex) to confirm `role: "tool"` is accepted.

## Risk

- **Low:** Changes 1, 2, 4, 5 are safe. Change 1 fixes an active production bug. Changes 2 and 5 are additive. Change 4 is logging-only.
- **Medium:** Change 3 (tool role) may break on some auth paths. Mitigation: revert the single commit if needed.

## Rollback

Revert the PR. All changes together form one atomic simplification. No partial rollback needed — each change is independently safe, but they share no ordering dependencies that would require selective revert.
