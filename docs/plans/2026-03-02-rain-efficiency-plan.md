# Rain Efficiency Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reduce Rain's per-interaction cost by 80%+ through three independent levers: stripping thinking text from history, adding thinkingConfig to Gemini requests, and wiring the planner into the channel orchestrator.

**Architecture:** Three independent levers that stack multiplicatively. Lever 1 (strip thinking) and Lever 2 (thinkingConfig) are isolated to `gemini.rs` + `traits.rs`. Lever 3 (planner wiring) extends the existing planner/executor in `planner.rs` and replaces `run_tool_call_loop` in `orchestrator.rs`.

**Tech Stack:** Rust, serde, Gemini API (thinkingConfig inside generationConfig), existing Provider trait system.

**Design doc:** `docs/plans/2026-03-02-rain-efficiency-design.md`

**API correction:** The design doc states `thinkingConfig` is a top-level peer to `generationConfig`. The actual Gemini API nests it **inside** `generationConfig`. This plan follows the actual API spec.

---

## Task 1: Strip Thinking Text from History

**Lever 1 — the simplest, highest-impact change.**

Currently `extract_response` in `gemini.rs` pushes every part (including raw chain-of-thought text) into `all_parts`. These get stored in `raw_model_parts` and replayed on every subsequent API call, growing context linearly.

**Files:**
- Modify: `src/providers/gemini.rs:402-419` (`extract_response`)
- Test: `src/providers/gemini.rs` (inline tests)

**Step 1: Write the failing test**

Add a test in the `#[cfg(test)]` module of `gemini.rs`:

```rust
#[test]
fn extract_response_strips_thinking_only_parts() {
    let content = CandidateContent {
        parts: vec![
            // Thinking part with NO signature — should be DROPPED
            ResponsePart {
                text: Some("Let me think about this...".to_string()),
                thought: true,
                thought_signature: None,
                function_call: None,
            },
            // Thinking part WITH signature — should be KEPT
            ResponsePart {
                text: None,
                thought: true,
                thought_signature: Some("sig123abc".to_string()),
                function_call: None,
            },
            // Non-thought text — should be KEPT
            ResponsePart {
                text: Some("Hello!".to_string()),
                thought: false,
                thought_signature: None,
                function_call: None,
            },
            // Function call — should be KEPT
            ResponsePart {
                text: None,
                thought: false,
                thought_signature: None,
                function_call: Some(FunctionCallResponse {
                    name: "get_weather".to_string(),
                    args: serde_json::json!({"city": "London"}),
                }),
            },
        ],
        role: Some("model".to_string()),
    };

    let (_text, _tool_calls, parts) = content.extract_response();

    // Should have 3 parts: signature, text, function_call (thinking-only dropped)
    assert_eq!(parts.len(), 3, "thinking-only part should be stripped");

    // First kept part: thought_signature
    assert!(parts[0].thought_signature.is_some());
    assert_eq!(parts[0].thought_signature.as_deref(), Some("sig123abc"));

    // Second kept part: non-thought text
    assert_eq!(parts[1].text.as_deref(), Some("Hello!"));
    assert!(parts[1].thought.is_none());

    // Third kept part: function call
    assert!(parts[2].function_call.is_some());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw extract_response_strips_thinking_only_parts -- --nocapture`

Expected: FAIL — currently all 4 parts are kept, assertion `parts.len() == 3` fails.

**Step 3: Implement the filter in `extract_response`**

In `gemini.rs` at line 408, change the `for part in self.parts` loop to conditionally push to `all_parts`. Replace the unconditional push (lines 410-419) with:

```rust
for part in self.parts {
    // Decide whether to keep this part in history.
    // Drop thinking-only parts (thought=true, no signature) — they're raw
    // chain-of-thought text that grows context without serving any purpose
    // on replay. Keep signatures (required by Gemini for continuity),
    // function calls, and non-thought text.
    let dominated_by_thinking = part.thought
        && part.thought_signature.is_none()
        && part.function_call.is_none();

    if !dominated_by_thinking {
        all_parts.push(Part {
            text: part.text.clone(),
            thought: if part.thought { Some(true) } else { None },
            thought_signature: part.thought_signature.clone(),
            function_call: part.function_call.as_ref().map(|fc| FunctionCallPart {
                name: fc.name.clone(),
                args: fc.args.clone(),
            }),
            function_response: None,
        });
    }

    // Tool call and answer extraction continues for ALL parts
    // (we still want to surface thinking text as the response even
    // though we don't store it in history for replay).
    if let Some(fc) = part.function_call {
        // ... existing tool call extraction (unchanged)
    }
    if let Some(text) = part.text {
        // ... existing text extraction (unchanged)
    }
}
```

Key: the `if let Some(fc)` and `if let Some(text)` blocks must remain **outside** the `if !dominated_by_thinking` guard so the function's text and tool_call return values are unaffected.

**Step 4: Run tests to verify**

Run: `cargo test -p zeroclaw extract_response_strips_thinking -- --nocapture`

Expected: PASS — new test passes, existing `raw_model_parts_round_trip_preserves_thinking_signatures` test still passes (signatures are kept).

**Step 5: Run full test suite**

Run: `cargo test -p zeroclaw`

Expected: All tests pass.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): strip thinking-only parts from history replay

Raw chain-of-thought parts (thought=true, no signature) were stored in
raw_model_parts and replayed on every subsequent API call. This grew
context linearly with each iteration. Now only structurally necessary
parts are kept: thought signatures (required by Gemini), function calls,
and non-thought text.

~40-60% token reduction per iteration, compounding across roundtrips."
```

---

## Task 2: Add ThinkingConfig to GenerationConfig

**Lever 2, part 1 — add the struct and wire it into the request body.**

Per the Gemini API spec, `thinkingConfig` nests **inside** `generationConfig`:
```json
{
  "generationConfig": {
    "temperature": 0.7,
    "maxOutputTokens": 8192,
    "thinkingConfig": {
      "thinkingBudget": 1024
    }
  }
}
```

For Gemini 2.5 models: use `thinkingBudget` (integer). For Gemini 3 models: use `thinkingLevel` (string). We support both fields with `skip_serializing_if` so the correct one is sent.

**Files:**
- Modify: `src/providers/gemini.rs:304-309` (`GenerationConfig` struct)
- Test: `src/providers/gemini.rs` (inline tests)

**Step 1: Write the failing test**

```rust
#[test]
fn thinking_config_serializes_with_budget() {
    let config = GenerationConfig {
        temperature: 0.7,
        max_output_tokens: 8192,
        thinking_config: Some(ThinkingConfig {
            thinking_budget: Some(1024),
            thinking_level: None,
        }),
    };
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["thinkingConfig"]["thinkingBudget"], 1024);
    assert!(json["thinkingConfig"].get("thinkingLevel").is_none());
}

#[test]
fn thinking_config_serializes_with_level() {
    let config = GenerationConfig {
        temperature: 0.7,
        max_output_tokens: 8192,
        thinking_config: Some(ThinkingConfig {
            thinking_budget: None,
            thinking_level: Some("low".to_string()),
        }),
    };
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["thinkingConfig"]["thinkingLevel"], "low");
    assert!(json["thinkingConfig"].get("thinkingBudget").is_none());
}

#[test]
fn thinking_config_omitted_when_none() {
    let config = GenerationConfig {
        temperature: 0.7,
        max_output_tokens: 8192,
        thinking_config: None,
    };
    let json = serde_json::to_value(&config).unwrap();
    assert!(json.get("thinkingConfig").is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw thinking_config_serializes -- --nocapture`

Expected: FAIL — `ThinkingConfig` type doesn't exist, `thinking_config` field doesn't exist on `GenerationConfig`.

**Step 3: Add ThinkingConfig struct and update GenerationConfig**

Add above `GenerationConfig` (around line 303):

```rust
#[derive(Debug, Serialize, Clone)]
struct ThinkingConfig {
    /// Token budget for thinking (Gemini 2.5). 0 = disable, -1 = dynamic.
    #[serde(rename = "thinkingBudget", skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<i32>,
    /// Thinking level (Gemini 3+): "minimal", "low", "medium", "high".
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none")]
    thinking_level: Option<String>,
}
```

Update `GenerationConfig`:

```rust
#[derive(Debug, Serialize, Clone)]
struct GenerationConfig {
    temperature: f64,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    thinking_config: Option<ThinkingConfig>,
}
```

**Step 4: Fix all GenerationConfig construction sites**

Add `thinking_config: None` to every `GenerationConfig { ... }` construction. There should be one at line ~1491:

```rust
generation_config: GenerationConfig {
    temperature,
    max_output_tokens: 8192,
    thinking_config: None,  // Set per-call in a later task
},
```

Search for any other `GenerationConfig {` constructions in the file and add `thinking_config: None` to each.

**Step 5: Run tests**

Run: `cargo test -p zeroclaw thinking_config -- --nocapture`

Expected: All three new tests PASS.

Run: `cargo test -p zeroclaw`

Expected: Full suite passes.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): add ThinkingConfig struct to GenerationConfig

Adds thinkingConfig support inside generationConfig per Gemini API spec.
Supports both thinkingBudget (Gemini 2.5) and thinkingLevel (Gemini 3+)
with skip_serializing_if for correct field selection.

No behavioral change yet — thinking_config is set to None at all call
sites. Per-call control wired in next task."
```

---

## Task 3: Add route_hint to ChatRequest

**Lever 2, part 2 — thread the route hint through the Provider trait interface.**

The `ChatRequest` in `traits.rs` is the public struct passed to `Provider::chat()`. Adding `route_hint` lets callers communicate intent (triage, planner, simple, complex, etc.) so providers can adjust behavior — in this case, Gemini sets thinkingConfig.

**Files:**
- Modify: `src/providers/traits.rs:112-115` (`ChatRequest` struct)
- Modify: all files that construct `traits::ChatRequest { ... }` — primarily `traits.rs` tests, `reliable.rs`, `router.rs`, `agent/planner.rs`, `agent/loop_.rs`
- **Do NOT modify** internal provider `ChatRequest` types (openai.rs, anthropic.rs, etc. have their own internal types with the same name)

**Step 1: Add route_hint field to ChatRequest**

```rust
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
    /// Semantic hint for the request context (e.g., "triage", "simple", "complex").
    /// Providers may use this to adjust behavior (e.g., thinking budget).
    pub route_hint: Option<&'a str>,
}
```

**Step 2: Update all construction sites with `route_hint: None`**

Search for `ChatRequest {` in:
- `src/providers/traits.rs` (test constructions)
- `src/providers/reliable.rs` (wrapper delegation)
- `src/providers/router.rs` (wrapper delegation)
- `src/agent/planner.rs` (planner call)
- `src/agent/loop_.rs` (tool loop call)

At each site, add `route_hint: None`.

Also check if any provider's `chat()` method destructures ChatRequest — if so, add `route_hint: _` to the pattern.

**Step 3: Compile and run tests**

Run: `cargo test -p zeroclaw`

Expected: Everything compiles, all tests pass. No behavioral change.

**Step 4: Commit**

```bash
git add src/providers/traits.rs src/providers/reliable.rs src/providers/router.rs src/agent/planner.rs src/agent/loop_.rs
git commit -m "refactor(providers): add route_hint field to ChatRequest

Adds an optional route_hint to the public ChatRequest struct so callers
can communicate request context (triage, planner, simple, complex, etc.)
to providers. All call sites default to None — no behavioral change."
```

---

## Task 4: Map route_hint to thinkingConfig in Gemini Provider

**Lever 2, part 3 — the Gemini provider reads route_hint and sets thinkingBudget.**

**Files:**
- Modify: `src/providers/gemini.rs` — the `chat()` method (line ~1802) and request construction (line ~1488)
- Test: `src/providers/gemini.rs` (inline tests)

**Step 1: Write the failing test**

```rust
#[test]
fn route_hint_maps_to_thinking_budget() {
    // Test the mapping function directly
    let cases = vec![
        (Some("triage"), Some(0)),
        (Some("heartbeat"), Some(0)),
        (Some("simple"), Some(0)),
        (Some("planner"), Some(1024)),
        (Some("medium"), Some(1024)),
        (Some("complex"), Some(4096)),
        (Some("reasoning"), Some(-1)),
        (None, None),                    // No hint → no thinkingConfig
        (Some("unknown"), None),         // Unknown hint → no thinkingConfig
    ];

    for (hint, expected_budget) in cases {
        let config = thinking_config_for_hint(hint);
        match expected_budget {
            Some(budget) => {
                let tc = config.expect(&format!("expected ThinkingConfig for hint {:?}", hint));
                assert_eq!(tc.thinking_budget, Some(budget),
                    "wrong budget for hint {:?}", hint);
            }
            None => {
                assert!(config.is_none(),
                    "expected no ThinkingConfig for hint {:?}", hint);
            }
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw route_hint_maps_to_thinking_budget -- --nocapture`

Expected: FAIL — `thinking_config_for_hint` function doesn't exist.

**Step 3: Implement the mapping function**

Add a helper function in `gemini.rs`:

```rust
/// Maps a route hint to a Gemini thinkingConfig.
///
/// | Hint       | Budget | Rationale                    |
/// |------------|--------|------------------------------|
/// | triage     | 0      | Binary relevance check       |
/// | heartbeat  | 0      | Periodic check-in            |
/// | simple     | 0      | Greetings, acknowledgments   |
/// | planner    | 1024   | Structured output, not deep  |
/// | medium     | 1024   | Standard tool use            |
/// | complex    | 4096   | Multi-step reasoning         |
/// | reasoning  | -1     | Dynamic (maximum)            |
fn thinking_config_for_hint(hint: Option<&str>) -> Option<ThinkingConfig> {
    let budget = match hint? {
        "triage" | "heartbeat" | "simple" => 0,
        "planner" | "medium" => 1024,
        "complex" => 4096,
        "reasoning" => -1,
        _ => return None,
    };
    Some(ThinkingConfig {
        thinking_budget: Some(budget),
        thinking_level: None,
    })
}
```

**Step 4: Wire into request construction**

In the `chat()` method (line ~1802), after building `contents` and before constructing `GenerateContentRequest`, extract the route hint and compute thinking config:

```rust
let thinking_config = thinking_config_for_hint(request.route_hint);
```

Then in the `GenerateContentRequest` construction (line ~1488):

```rust
generation_config: GenerationConfig {
    temperature,
    max_output_tokens: 8192,
    thinking_config,
},
```

**Step 5: Run tests**

Run: `cargo test -p zeroclaw route_hint_maps -- --nocapture`

Expected: PASS.

Run: `cargo test -p zeroclaw`

Expected: Full suite passes.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): map route hints to thinking budget

Gemini provider now reads route_hint from ChatRequest and sets
thinkingConfig.thinkingBudget accordingly. Triage/heartbeat/simple
calls disable thinking (budget=0), planner/medium use 1024 tokens,
complex uses 4096, and reasoning gets dynamic (-1).

~50-70% token reduction on simple/medium calls."
```

---

## Task 5: Thread route_hint from Orchestrator and Agent

**Lever 2, part 4 — callers pass meaningful hints to provider.**

**Files:**
- Modify: `src/agent/loop_.rs` — `run_tool_call_loop` signature + ChatRequest construction
- Modify: `src/channels/orchestrator.rs` — pass route hint from route selection
- Modify: `src/agent/agent.rs` — pass hint from classification context
- Modify: `src/agent/planner.rs` — pass "planner" hint for planning call

**Step 1: Add route_hint parameter to run_tool_call_loop**

Add `route_hint: Option<&str>` to the function signature (it already has many params). Pass it through to the `ChatRequest` construction inside the loop.

**Step 2: Update all run_tool_call_loop call sites**

Search for all calls to `run_tool_call_loop`. For each, pass the appropriate hint:

- `orchestrator.rs` — pass the route hint from route selection (check what `get_route_selection` returns; it may have a `hint` field, or derive from model name)
- `agent.rs` — pass from classification context (agent already classifies queries)
- `planner.rs` — executor calls: pass `None` (executor uses the resolved model)
- Any test call sites — pass `None`

**Step 3: Pass "triage" hint for triage calls**

In `orchestrator.rs`, the triage call uses `ctx.provider.chat_with_history()`. This doesn't go through ChatRequest, so it doesn't get a route_hint. For now, this is acceptable — the triage model is already a separate, smaller model. If triage cost is still high after the other levers, we can add route_hint to `chat_with_history` later.

**Step 4: Pass "planner" hint for planning call in planner.rs**

In `plan_then_execute`, the planning call constructs a `ChatRequest`. Set `route_hint: Some("planner")` on that request.

**Step 5: Run tests**

Run: `cargo test -p zeroclaw`

Expected: All tests pass.

**Step 6: Commit**

```bash
git add src/agent/loop_.rs src/channels/orchestrator.rs src/agent/agent.rs src/agent/planner.rs
git commit -m "feat: thread route hints from orchestrator and agent to provider

run_tool_call_loop now accepts route_hint and forwards it to
ChatRequest. The orchestrator passes the route hint from route
selection, and the planner passes 'planner' for planning calls.

This enables per-call thinking budget control in the Gemini provider."
```

---

## Task 6: Extend plan_then_execute for Channel Context

**Lever 3, part 1 — the planner needs channel-specific parameters to work in the channel orchestrator.**

Currently `plan_then_execute` is wired only into the CLI path. It lacks parameters needed for channel execution: channel_name, excluded_tools, hooks, cancellation_token, and on_delta.

**Files:**
- Modify: `src/agent/planner.rs:174-187` (function signature)
- Modify: `src/agent/planner.rs:~293-310` (executor's run_tool_call_loop calls)
- Modify: `src/agent/agent.rs:~556-569` (CLI call site)
- Test: `src/agent/planner.rs` (update existing tests)

**Step 1: Extend the function signature**

Add these parameters to `plan_then_execute`:

```rust
pub async fn plan_then_execute(
    provider: &dyn Provider,
    planner_model: &str,
    executor_model: &str,
    system_prompt: &str,
    user_message: &str,
    memory_context: &str,
    tools_registry: &[Box<dyn Tool>],
    tool_specs: &[ToolSpec],
    observer: &dyn Observer,
    provider_name: &str,
    temperature: f64,
    max_tool_iterations: usize,
    // Channel context (new)
    channel_name: &str,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
) -> Result<PlanExecutionResult>
```

**Step 2: Forward new params to executor's run_tool_call_loop calls**

In the executor loop (around line 293), update the `run_tool_call_loop` call to pass the new params:

```rust
let result = crate::agent::loop_::run_tool_call_loop(
    provider,
    &mut action_messages,
    tools_registry,
    observer,
    provider_name,
    executor_model,
    temperature,
    true,                    // silent
    None,                    // approval
    channel_name,            // was ""
    &crate::config::MultimodalConfig::default(),
    max_tool_iterations.min(5),
    cancellation_token.clone(), // was None
    None,                    // on_delta: don't stream individual actions
    hooks,                   // was None
    &combined_excluded,      // action-specific + channel-specific exclusions
    route_hint,              // from Task 5
)
.await;
```

Note: `on_delta` for individual executor actions should be `None` — we don't stream partial action results. The final assembled result is sent at the end.

**Step 3: Update CLI call site in agent.rs**

```rust
let plan_result = super::planner::plan_then_execute(
    self.provider.as_ref(),
    &planner_model,
    &effective_model,
    &system_prompt,
    user_message,
    &context,
    &self.tools,
    &self.tool_specs,
    self.observer.as_ref(),
    "router",
    self.temperature,
    self.config.max_tool_iterations,
    // Channel context defaults for CLI
    "cli",
    None,  // cancellation_token
    None,  // on_delta
    None,  // hooks
    &[],   // excluded_tools
)
.await;
```

**Step 4: Update existing planner tests**

All existing tests that call `plan_then_execute` need the new params added:

```rust
let result = super::plan_then_execute(
    &provider,
    "hint:planner",
    "hint:complex",
    "System.",
    "Hello",
    "",
    &[],
    &[],
    &observer,
    "test",
    0.7,
    5,
    // New params
    "",     // channel_name
    None,   // cancellation_token
    None,   // on_delta
    None,   // hooks
    &[],    // excluded_tools
)
.await;
```

**Step 5: Run tests**

Run: `cargo test -p zeroclaw planner -- --nocapture`

Expected: All planner tests pass.

Run: `cargo test -p zeroclaw`

Expected: Full suite passes.

**Step 6: Commit**

```bash
git add src/agent/planner.rs src/agent/agent.rs
git commit -m "refactor(planner): extend plan_then_execute for channel context

Adds channel_name, cancellation_token, on_delta, hooks, and
excluded_tools parameters to plan_then_execute. These are forwarded
to the executor's run_tool_call_loop calls. CLI call site passes
safe defaults. No behavioral change — prepares for channel wiring."
```

---

## Task 7: Wire Planner into Channel Orchestrator

**Lever 3, part 2 — the biggest behavioral change.**

Replace the `run_tool_call_loop` call in `process_channel_message` with `plan_then_execute`. On `Passthrough`, fall back to `run_tool_call_loop` (existing behavior). On `Executed`, send the assembled result to the channel.

**Files:**
- Modify: `src/channels/orchestrator.rs:~1313-1439` (replace the tool loop invocation)

**Step 1: Import plan_then_execute**

At the top of `orchestrator.rs`, ensure planner types are available:

```rust
use crate::agent::planner::{plan_then_execute, PlanExecutionResult};
```

**Step 2: Check if planner model route is configured**

Before the existing `run_tool_call_loop` call site (around line 1413), add planner activation logic. The planner should only activate if a planner model route is configured:

```rust
let planner_model = ctx.route_model_by_hint
    .as_ref()
    .and_then(|m| m.get("planner").cloned());
```

Check what fields `ChannelRuntimeContext` has — it may not have `route_model_by_hint`. If not, check the route config for a planner hint. If no planner model is configured, skip planner and use `run_tool_call_loop` as before (backward compatible).

**Step 3: Add planner path**

Replace the direct `run_tool_call_loop` call with:

```rust
if let Some(planner_model) = planner_model {
    // Build tool specs for planner
    let tool_specs: Vec<_> = ctx.tools_registry.iter()
        .map(|t| t.spec())
        .collect();

    let plan_result = plan_then_execute(
        active_provider.as_ref(),
        &planner_model,
        route.model.as_str(),
        &system_prompt,
        &enriched_content,
        "",  // memory_context already injected into history
        ctx.tools_registry.as_ref(),
        &tool_specs,
        ctx.observer.as_ref(),
        route.provider.as_str(),
        runtime_defaults.temperature,
        ctx.max_tool_iterations,
        msg.channel.as_str(),
        Some(cancellation_token.clone()),
        delta_tx.clone(),
        ctx.hooks.as_deref(),
        if msg.channel == "cli" { &[] } else { ctx.non_cli_excluded_tools.as_ref() },
    )
    .await;

    match plan_result {
        Ok(PlanExecutionResult::Passthrough) => {
            tracing::info!("Planner returned passthrough, using tool call loop");
            // Fall through to run_tool_call_loop below
        }
        Ok(PlanExecutionResult::Executed { output, action_results }) => {
            tracing::info!(
                actions = action_results.len(),
                "Planner/executor completed for channel"
            );
            // Use output as the response — skip run_tool_call_loop
            // ... (handle like a successful run_tool_call_loop result)
        }
        Err(e) => {
            tracing::warn!("Planner failed ({e}), falling back to tool call loop");
            // Fall through to run_tool_call_loop below
        }
    }
}
```

**Step 4: Handle the Executed result**

When planner returns `Executed`, the `output` string is the final response. Handle it the same way as a successful `run_tool_call_loop` result:
- Run on_message_sending hook
- Sanitize the response
- Persist to conversation history
- Send to channel (or finalize draft)

Extract the existing response-handling code (currently after `run_tool_call_loop`) into a helper function so both paths can use it, OR structure the code so `Executed` returns early and `Passthrough`/`Err` fall through to the existing `run_tool_call_loop` path.

**Step 5: Test manually**

This is a behavioral change with no good unit test surface (the orchestrator requires a full runtime context). Verify by:

1. Checking that `cargo test -p zeroclaw` passes (no regressions)
2. Running Rain in a test channel and observing:
   - Simple greeting → Passthrough → single tool loop call → fast response
   - Multi-step request → Plan + parallel execution → faster than before
   - Planner not configured → falls back to existing behavior

**Step 6: Commit**

```bash
git add src/channels/orchestrator.rs
git commit -m "feat(orchestrator): wire planner into channel message processing

Channel messages now route through plan_then_execute when a planner
model is configured. Simple messages get Passthrough (fall back to
existing tool loop). Multi-step tasks get independent executor calls
with fresh context per action.

Eliminates context accumulation across iterations — cost becomes
additive (planning + N actions) instead of multiplicative
(N iterations x growing context)."
```

---

## Task 8: Verification and Cleanup

**Final pass — verify all three levers work together.**

**Step 1: Run full test suite**

Run: `cargo test -p zeroclaw`

Expected: All tests pass.

**Step 2: Run clippy and fmt**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings`

Expected: No warnings, no format issues.

**Step 3: Verify config**

Check `zeroclaw.toml` (or equivalent config) to ensure:
- A `planner` model route is configured (needed for Lever 3)
- Query classification thresholds are reasonable

If planner route is missing, add it:

```toml
[[model_routes]]
hint = "planner"
provider = "gemini"  # or whatever provider Rain uses
model = "gemini-2.0-flash"
```

**Step 4: Commit any fixes**

```bash
git add -A
git commit -m "chore: fix lint/format issues from rain efficiency changes"
```
