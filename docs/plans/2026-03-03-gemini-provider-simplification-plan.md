# Gemini Provider Simplification — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove `required_tool_names` (fixes production 400 errors), add model-aware thinking config, switch tool result role to `"tool"`, improve retry observability, and add opt-in VALIDATED mode.

**Architecture:** Five changes to the Gemini provider, all in one branch. Each task is one atomic commit. TDD throughout — tests first, then implementation.

**Tech Stack:** Rust, serde, tracing, Gemini REST API

---

### Task 1: Manual baseline — verify current tests pass

Before touching any code, confirm the existing test suite is green.

**Step 1: Run the full test suite**

Run: `cargo test`
Expected: All tests pass. Note any pre-existing failures.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings.

---

### Task 2: Manual API test — verify `role: "tool"` is accepted

Before writing code, confirm the Gemini API accepts `role: "tool"` for function responses. This de-risks Task 6.

**Files:**
- None (manual curl test)

**Step 1: Send a two-turn function calling request with `role: "tool"`**

Replace `$GEMINI_API_KEY` with a real key and run:

```bash
curl -s "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key=$GEMINI_API_KEY" \
  -H 'Content-Type: application/json' \
  -X POST \
  -d '{
    "contents": [
      {"role": "user", "parts": [{"text": "What is 2+2? Use the calculator tool."}]},
      {"role": "model", "parts": [{"functionCall": {"name": "calculator", "args": {"expression": "2+2"}}}]},
      {"role": "tool", "parts": [{"functionResponse": {"name": "calculator", "response": {"result": 4}}}]}
    ],
    "tools": [{"functionDeclarations": [{"name": "calculator", "description": "Evaluate math", "parameters": {"type": "OBJECT", "properties": {"expression": {"type": "STRING"}}}}]}]
  }'
```

Expected: 200 OK with a text response mentioning "4". If this returns an error about invalid role, we skip Task 6 (keep `role: "user"`).

**Step 2: Test with multiple consecutive `role: "tool"` entries (no merge)**

```bash
curl -s "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key=$GEMINI_API_KEY" \
  -H 'Content-Type: application/json' \
  -X POST \
  -d '{
    "contents": [
      {"role": "user", "parts": [{"text": "What is 2+2 and 3+3?"}]},
      {"role": "model", "parts": [
        {"functionCall": {"name": "calculator", "args": {"expression": "2+2"}}},
        {"functionCall": {"name": "calculator", "args": {"expression": "3+3"}}}
      ]},
      {"role": "tool", "parts": [{"functionResponse": {"name": "calculator", "response": {"result": 4}}}]},
      {"role": "tool", "parts": [{"functionResponse": {"name": "calculator", "response": {"result": 6}}}]}
    ],
    "tools": [{"functionDeclarations": [{"name": "calculator", "description": "Evaluate math", "parameters": {"type": "OBJECT", "properties": {"expression": {"type": "STRING"}}}}]}]
  }'
```

Expected: 200 OK. If this fails but Step 1 succeeded, we can use `role: "tool"` but must keep the merge logic. If both fail, skip Task 6 entirely.

**Step 3: Document the result**

Record which combination works:
- Both pass → Task 6: switch to `"tool"` AND remove merge logic
- Step 1 passes, Step 2 fails → Task 6: switch to `"tool"`, keep merge logic
- Both fail → Skip Task 6

---

### Task 3: Manual API test — verify `ANY` mode without `allowedFunctionNames`

Confirm that `mode: "ANY"` without `allowedFunctionNames` forces a function call.

**Step 1: Send request with `ANY` mode, no `allowedFunctionNames`**

```bash
curl -s "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key=$GEMINI_API_KEY" \
  -H 'Content-Type: application/json' \
  -X POST \
  -d '{
    "contents": [
      {"role": "user", "parts": [{"text": "Say hello to the general channel"}]}
    ],
    "tools": [{"functionDeclarations": [
      {"name": "send_message", "description": "Send a message to a channel", "parameters": {"type": "OBJECT", "properties": {"channel": {"type": "STRING"}, "text": {"type": "STRING"}}}},
      {"name": "get_status", "description": "Get channel status", "parameters": {"type": "OBJECT", "properties": {"channel": {"type": "STRING"}}}}
    ]}],
    "toolConfig": {"functionCallingConfig": {"mode": "ANY"}}
  }'
```

Expected: 200 OK with a `functionCall` in the response (not text). The model should pick `send_message` without us specifying `allowedFunctionNames`.

---

### Task 4: Replace `required_tool_names` with `force_tool_call` on `ChatRequest`

**Files:**
- Modify: `src/providers/traits.rs:110-122`
- Modify: `src/providers/traits.rs` (all test `ChatRequest` constructions)
- Modify: `src/providers/reliable.rs:781`
- Modify: `src/providers/reliable.rs` (all test `ChatRequest` constructions)
- Modify: `src/providers/anthropic.rs:562`
- Modify: `src/agent/agent.rs:692`
- Modify: `src/agent/classifier.rs:812`
- Modify: `src/channels/orchestrator.rs:1543`
- Modify: `src/tools/delegate.rs:415`
- Modify: `tests/openai_codex_vision_e2e.rs:90,214`
- Modify: `src/agent/loop_.rs:2102,2191` (and all test calls)
- Modify: `src/agent/loop_.rs:1910` (wrapper function)

**Step 1: Update the `ChatRequest` struct**

In `src/providers/traits.rs`, replace:

```rust
    /// When set, forces providers to require a structured tool call using only
    /// these function names (e.g., Gemini `mode: "ANY"` + `allowedFunctionNames`).
    /// `None` means the provider decides (AUTO mode).
    pub required_tool_names: Option<&'a [String]>,
```

with:

```rust
    /// When true, forces providers to require a structured tool call
    /// (e.g., Gemini `mode: "ANY"`). When false, the provider decides (AUTO mode).
    pub force_tool_call: bool,
```

**Step 2: Update `run_tool_call_loop` signature**

In `src/agent/loop_.rs`, replace the `required_tool_names: Option<&[String]>` parameter (line 2102) with `force_tool_call: bool`. Update the `ChatRequest` construction at line 2191 to use `force_tool_call` instead.

Also update the thin wrapper function around line 1910 — it passes `None` for the last parameter. Change to `false`.

**Step 3: Update all call sites**

Every file that constructs `ChatRequest` or calls `run_tool_call_loop`:

| File | Old | New |
|---|---|---|
| `src/providers/traits.rs` (tests) | `required_tool_names: None` | `force_tool_call: false` |
| `src/providers/reliable.rs:781` | `required_tool_names: request.required_tool_names` | `force_tool_call: request.force_tool_call` |
| `src/providers/reliable.rs` (tests) | `required_tool_names: None` | `force_tool_call: false` |
| `src/providers/anthropic.rs:562` | `required_tool_names: None` | `force_tool_call: false` |
| `src/agent/agent.rs:692` | `required_tool_names: None` | `force_tool_call: false` |
| `src/agent/classifier.rs:812` | `required_tool_names: None` | `force_tool_call: false` |
| `src/channels/orchestrator.rs:1543` | `None, // required_tool_names` | `false, // force_tool_call` |
| `src/tools/delegate.rs:415` | `None, // required_tool_names` | `false, // force_tool_call` |
| `tests/openai_codex_vision_e2e.rs:90,214` | `required_tool_names: None` | `force_tool_call: false` |
| `src/agent/planner.rs:235` | `required_tool_names: None` | `force_tool_call: false` |
| All `run_tool_call_loop` test calls in `loop_.rs` | Last arg `None` | Last arg `false` |

**Step 4: Update planner to pass `force_tool_call: true`**

In `src/agent/planner.rs`, replace lines 312-316:

```rust
let required = if wanted_tools.is_empty() {
    None
} else {
    Some(wanted_tools.as_slice())
};
```

with:

```rust
let force_tool_call = !wanted_tools.is_empty();
```

And update the `run_tool_call_loop` call at line 335 to pass `force_tool_call` instead of `required`.

**Step 5: Run tests**

Run: `cargo test`
Expected: All pass. This is a pure refactor — behavior changes come in the next task.

**Step 6: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: Clean.

**Step 7: Commit**

```
git add -A && git commit -m "refactor(providers): replace required_tool_names with force_tool_call bool

The Option<&[String]> field threaded tool names through 4 layers to populate
Gemini's allowedFunctionNames. A bool is sufficient: the executor needs to
force a tool call (ANY mode) but does not need to restrict which tools by
name — the tool_specs already handle that via exclusion filtering.

No behavior change yet; Gemini provider still uses the old signature
internally (updated in next commit)."
```

---

### Task 5: Simplify `build_tool_config_for_request` and `FunctionCallingConfigMode`

**Files:**
- Modify: `src/providers/gemini.rs:212-220` (`FunctionCallingConfigMode` struct)
- Modify: `src/providers/gemini.rs:711-732` (`build_tool_config_for_request`)
- Modify: `src/providers/gemini.rs:2275-2276` (call site in `chat()`)
- Modify: `src/providers/gemini.rs:4139-4164` (existing tests)

**Step 1: Write the new tests**

Replace the three existing `build_tool_config_for_request` tests (lines 4139-4164) with:

```rust
#[test]
fn tool_config_any_mode_when_force_tool_call() {
    let config = build_tool_config_for_request(true, true, None);
    let tc = config.unwrap();
    assert_eq!(tc.function_calling_config.mode, "ANY");
}

#[test]
fn tool_config_auto_mode_by_default() {
    let config = build_tool_config_for_request(true, false, None);
    let tc = config.unwrap();
    assert_eq!(tc.function_calling_config.mode, "AUTO");
}

#[test]
fn tool_config_validated_mode_when_configured() {
    let config = build_tool_config_for_request(true, false, Some("validated"));
    let tc = config.unwrap();
    assert_eq!(tc.function_calling_config.mode, "VALIDATED");
}

#[test]
fn tool_config_force_overrides_validated() {
    let config = build_tool_config_for_request(true, true, Some("validated"));
    let tc = config.unwrap();
    assert_eq!(tc.function_calling_config.mode, "ANY");
}

#[test]
fn tool_config_none_when_no_tools() {
    let config = build_tool_config_for_request(false, false, None);
    assert!(config.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test tool_config_ -- --nocapture 2>&1 | head -30`
Expected: Compile error — signature mismatch.

**Step 3: Update `FunctionCallingConfigMode`**

Replace:

```rust
#[derive(Debug, Serialize, Clone)]
struct FunctionCallingConfigMode {
    mode: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "allowedFunctionNames"
    )]
    allowed_function_names: Option<Vec<String>>,
}
```

with:

```rust
#[derive(Debug, Serialize, Clone)]
struct FunctionCallingConfigMode {
    mode: String,
}
```

**Step 4: Update `build_tool_config_for_request`**

Replace:

```rust
fn build_tool_config_for_request(
    has_tools: bool,
    required_tool_names: Option<&[String]>,
) -> Option<GeminiToolConfig> {
    if !has_tools {
        return None;
    }
    Some(GeminiToolConfig {
        function_calling_config: match required_tool_names {
            Some(names) => FunctionCallingConfigMode {
                mode: "ANY".into(),
                allowed_function_names: Some(names.to_vec()),
            },
            None => FunctionCallingConfigMode {
                mode: "AUTO".into(),
                allowed_function_names: None,
            },
        },
    })
}
```

with:

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

**Step 5: Update the call site in `chat()`**

At line 2275-2276, replace:

```rust
let tool_config =
    build_tool_config_for_request(gemini_tools.is_some(), request.required_tool_names);
```

with:

```rust
let tool_config =
    build_tool_config_for_request(gemini_tools.is_some(), request.force_tool_call, self.tool_call_mode.as_deref());
```

(The `self.tool_call_mode` field is added in Task 8. For now, use `None` as a placeholder to get tests passing, then update once the field exists.)

Temporary:

```rust
let tool_config =
    build_tool_config_for_request(gemini_tools.is_some(), request.force_tool_call, None);
```

**Step 6: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 7: Commit**

```
git add -A && git commit -m "fix(gemini): remove allowedFunctionNames, simplify tool config to ANY/AUTO/VALIDATED

allowedFunctionNames caused 400 errors when names in the list did not
match the function_declarations sent in the same request. ANY mode
without allowedFunctionNames achieves the same goal (force a structured
tool call) without the validation pitfall.

Also adds groundwork for opt-in VALIDATED mode (wired in a later commit)."
```

---

### Task 6: Model-aware thinking config

**Files:**
- Modify: `src/providers/gemini.rs:697-709` (`thinking_config_for_hint`)

**Step 1: Write the failing tests**

Add after the existing tests in `gemini.rs`:

```rust
#[test]
fn thinking_config_gemini25_uses_budget() {
    let config = thinking_config_for_hint(Some("complex"), "gemini-2.5-flash").unwrap();
    assert_eq!(config.thinking_budget, Some(4096));
    assert!(config.thinking_level.is_none());
}

#[test]
fn thinking_config_gemini3_uses_level() {
    let config = thinking_config_for_hint(Some("complex"), "gemini-3-flash-preview").unwrap();
    assert!(config.thinking_budget.is_none());
    assert_eq!(config.thinking_level.as_deref(), Some("medium"));
}

#[test]
fn thinking_config_gemini3_simple_is_minimal() {
    let config = thinking_config_for_hint(Some("simple"), "gemini-3-pro").unwrap();
    assert!(config.thinking_budget.is_none());
    assert_eq!(config.thinking_level.as_deref(), Some("minimal"));
}

#[test]
fn thinking_config_gemini3_reasoning_is_high() {
    let config = thinking_config_for_hint(Some("reasoning"), "models/gemini-3-flash-preview").unwrap();
    assert!(config.thinking_budget.is_none());
    assert_eq!(config.thinking_level.as_deref(), Some("high"));
}

#[test]
fn thinking_config_unknown_hint_returns_none() {
    assert!(thinking_config_for_hint(Some("unknown"), "gemini-2.5-flash").is_none());
    assert!(thinking_config_for_hint(None, "gemini-2.5-flash").is_none());
}

#[test]
fn is_gemini3_model_detection() {
    assert!(is_gemini3_model("gemini-3-flash-preview"));
    assert!(is_gemini3_model("models/gemini-3-pro"));
    assert!(is_gemini3_model("gemini-3.1-pro"));
    assert!(!is_gemini3_model("gemini-2.5-flash"));
    assert!(!is_gemini3_model("gemini-2.0-flash"));
    assert!(!is_gemini3_model("models/gemini-1.5-pro"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test thinking_config_gemini -- --nocapture 2>&1 | head -20`
Expected: Compile error — `thinking_config_for_hint` takes 2 args now but old signature has 1.

**Step 3: Implement `is_gemini3_model`**

Add above `thinking_config_for_hint`:

```rust
/// Returns true if the model name indicates a Gemini 3.x family model.
fn is_gemini3_model(model: &str) -> bool {
    let normalized = model.strip_prefix("models/").unwrap_or(model);
    normalized.starts_with("gemini-3")
}
```

**Step 4: Update `thinking_config_for_hint`**

Replace:

```rust
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

with:

```rust
fn thinking_config_for_hint(hint: Option<&str>, model: &str) -> Option<ThinkingConfig> {
    let hint = hint?;
    if is_gemini3_model(model) {
        let level = match hint {
            "triage" | "heartbeat" | "simple" => "minimal",
            "planner" | "medium" => "low",
            "complex" => "medium",
            "reasoning" => "high",
            _ => return None,
        };
        Some(ThinkingConfig {
            thinking_budget: None,
            thinking_level: Some(level.into()),
        })
    } else {
        let budget = match hint {
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
}
```

**Step 5: Update the call site**

Find where `thinking_config_for_hint` is called in the `chat()` method and pass the model string. Search for `thinking_config_for_hint(` in gemini.rs and add the `model` argument.

**Step 6: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 7: Commit**

```
git add -A && git commit -m "feat(gemini): use thinkingLevel for Gemini 3 models, thinkingBudget for 2.5

Gemini 3 docs recommend thinkingLevel (minimal/low/medium/high) over
thinkingBudget, which is accepted for backwards compatibility but may
lead to unexpected performance. Detect model family and use the correct
field."
```

---

### Task 7: Switch tool result role to `"tool"`

**Prerequisite:** Task 2 confirmed `role: "tool"` is accepted by the API.

If Task 2 showed both steps passing, proceed with this full task. If only Step 1 passed, do 7a only (switch role but keep merge). If both failed, skip this task entirely.

**Files:**
- Modify: `src/providers/gemini.rs:2224` (role change)
- Modify: `src/providers/gemini.rs:2216-2222` (merge logic — remove if Task 2 Step 2 passed)

**Step 7a: Change the role string**

At line 2224, replace:

```rust
role: Some("user".into()),
```

with:

```rust
role: Some("tool".into()),
```

**Step 7b: Remove merge logic (only if Task 2 Step 2 passed)**

Remove the merge block at lines 2216-2222:

```rust
// Merge consecutive tool results into one Content to maintain
// strict role alternation (Gemini requires user/model/user/model).
if let Some(last) = contents.last_mut() {
    if last.role.as_deref() == Some("user")
        && last.parts.iter().all(|p| p.function_response.is_some())
    {
        last.parts.push(part);
        continue;
    }
}
```

If keeping the merge logic (Step 2 failed), update the merge check from `Some("user")` to `Some("tool")`:

```rust
if last.role.as_deref() == Some("tool")
```

**Step 7c: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 7d: Commit**

```
git add -A && git commit -m "fix(gemini): use role 'tool' for functionResponse content

The Gemini API supports role: 'tool' for function response entries.
Using the correct role instead of the 'user' workaround."
```

---

### Task 8: Add `tool_call_mode` config and wire into provider

**Files:**
- Modify: `src/providers/gemini.rs:23-32` (`GeminiProvider` struct)
- Modify: `src/providers/gemini.rs:879-900` (`new` constructor)
- Modify: `src/providers/gemini.rs:902+` (`new_with_auth` constructor)
- Modify: `src/providers/gemini.rs:2275` (replace `None` placeholder from Task 5)
- Modify: `src/providers/mod.rs:993` (factory — pass config value)
- Modify: `src/config/schema.rs` (add field to provider config)

**Step 1: Add field to `GeminiProvider`**

```rust
pub struct GeminiProvider {
    auth: Option<GeminiAuth>,
    oauth_project: Arc<tokio::sync::Mutex<Option<String>>>,
    oauth_cred_paths: Vec<PathBuf>,
    oauth_index: Arc<tokio::sync::Mutex<usize>>,
    auth_service: Option<AuthService>,
    auth_profile_override: Option<String>,
    /// Optional tool-call mode override: "auto" (default) or "validated".
    tool_call_mode: Option<String>,
}
```

**Step 2: Update constructors**

In `new()` and `new_with_auth()`, add `tool_call_mode: None` to the `Self { ... }` block.

Add a builder-style setter:

```rust
/// Set the tool-call mode (e.g., "validated"). None means "auto".
pub fn with_tool_call_mode(mut self, mode: Option<String>) -> Self {
    self.tool_call_mode = mode;
    self
}
```

**Step 3: Wire from factory**

In `src/providers/mod.rs:993`, check if the provider config has a `tool_call_mode` field and pass it:

```rust
Ok(Box::new(gemini::GeminiProvider::new_with_auth(
    key,
    auth_service,
    options.auth_profile_override.clone(),
).with_tool_call_mode(options.tool_call_mode.clone())))
```

This requires `tool_call_mode` on the provider options struct. Check the config schema and add the field there. Use `#[serde(default)]` so existing configs don't break.

**Step 4: Replace placeholder in `chat()`**

Replace the `None` from Task 5:

```rust
let tool_config =
    build_tool_config_for_request(gemini_tools.is_some(), request.force_tool_call, self.tool_call_mode.as_deref());
```

**Step 5: Write a test**

```rust
#[test]
fn provider_stores_tool_call_mode() {
    let provider = GeminiProvider::new(Some("key"))
        .with_tool_call_mode(Some("validated".into()));
    assert_eq!(provider.tool_call_mode.as_deref(), Some("validated"));
}
```

**Step 6: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 7: Commit**

```
git add -A && git commit -m "feat(gemini): add opt-in tool_call_mode config for VALIDATED mode

Adds tool_call_mode field to GeminiProvider and provider config. When
set to 'validated', the normal agent loop uses Gemini's VALIDATED mode
which guarantees schema adherence on function calls without forcing one.
Defaults to 'auto' (current behavior)."
```

---

### Task 9: Improve `generationConfig` retry observability

**Files:**
- Modify: `src/providers/gemini.rs:1804-1805` (first warn)
- Modify: `src/providers/gemini.rs:1830-1831` (second warn)

**Step 1: Update the two `tracing::warn!` calls**

Replace both instances of:

```rust
tracing::warn!(
    "Gemini OAuth internal endpoint rejected generationConfig; retrying without generationConfig"
);
```

with:

```rust
tracing::warn!(
    retry_reason = "generation_config_rejected",
    auth_method = %auth.method_name(),
    "Gemini OAuth endpoint rejected generationConfig; retrying without it"
);
```

Check if `auth` has a `method_name()` or similar method. If not, use a simpler label:

```rust
tracing::warn!(
    retry_reason = "generation_config_rejected",
    "Gemini OAuth endpoint rejected generationConfig; retrying without it"
);
```

**Step 2: Run tests**

Run: `cargo test`
Expected: All pass.

**Step 3: Commit**

```
git add -A && git commit -m "chore(gemini): add structured fields to generationConfig retry logging

Adds retry_reason field so the retry path is queryable in structured
log systems. Helps determine if this workaround is still needed."
```

---

### Task 10: Final validation

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All pass.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: Clean.

**Step 3: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: Clean.

**Step 4: Review diff**

Run: `git diff main --stat`
Expected: Changes limited to the files listed in the design doc. No unrelated modifications.
