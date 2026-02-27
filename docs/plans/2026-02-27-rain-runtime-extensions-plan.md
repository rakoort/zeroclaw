# Rain Runtime Extensions — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable Gemini native function calling, typed Slack/Linear tools, and an event watch system so the Rain PM agent operates reliably.

**Architecture:** Three layered changes in `src/providers/gemini.rs` (native tools), `src/tools/slack/` + `src/tools/linear/` (typed CLI wrappers), and `src/watches/` (event-driven watch system with per-watch timer tasks). Each layer builds on the previous; each reverts independently.

**Tech Stack:** Rust, serde, tokio, rusqlite, tokio_util::sync::CancellationToken

**Design doc:** `docs/plans/2026-02-27-rain-runtime-extensions-design.md`

---

## Task 1: Extend Gemini Part structs for function calling

**Files:**
- Modify: `src/providers/gemini.rs:121-128` (GenerateContentRequest)
- Modify: `src/providers/gemini.rs:156-164` (InternalGenerateContentRequest)
- Modify: `src/providers/gemini.rs:173-176` (Part)
- Modify: `src/providers/gemini.rs:221-228` (ResponsePart)

**Step 1: Write failing test for Part serialization**

Add at the bottom of the existing `#[cfg(test)] mod tests` block in `src/providers/gemini.rs`:

```rust
#[test]
fn part_text_only_serializes_without_function_fields() {
    let part = Part { text: Some("hello".into()), function_call: None, function_response: None };
    let json = serde_json::to_value(&part).unwrap();
    assert_eq!(json, serde_json::json!({"text": "hello"}));
    assert!(json.get("functionCall").is_none());
    assert!(json.get("functionResponse").is_none());
}

#[test]
fn part_function_call_serializes_correctly() {
    let part = Part {
        text: None,
        function_call: Some(FunctionCallPart {
            name: "slack_dm".into(),
            args: serde_json::json!({"user_id": "U123"}),
        }),
        function_response: None,
    };
    let json = serde_json::to_value(&part).unwrap();
    assert_eq!(json["functionCall"]["name"], "slack_dm");
    assert_eq!(json["functionCall"]["args"]["user_id"], "U123");
    assert!(json.get("text").is_none());
}

#[test]
fn part_function_response_serializes_correctly() {
    let part = Part {
        text: None,
        function_call: None,
        function_response: Some(FunctionResponsePart {
            name: "slack_dm".into(),
            response: serde_json::json!({"ok": true}),
        }),
    };
    let json = serde_json::to_value(&part).unwrap();
    assert_eq!(json["functionResponse"]["name"], "slack_dm");
    assert_eq!(json["functionResponse"]["response"]["ok"], true);
}

#[test]
fn response_part_deserializes_function_call() {
    let json = serde_json::json!({
        "functionCall": {"name": "slack_dm", "args": {"user_id": "U123"}}
    });
    let part: ResponsePart = serde_json::from_value(json).unwrap();
    assert!(part.text.is_none());
    let fc = part.function_call.unwrap();
    assert_eq!(fc.name, "slack_dm");
    assert_eq!(fc.args["user_id"], "U123");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests::part_text_only -- --no-capture 2>&1 | head -20`
Expected: Compilation failure — `Part` has no field `function_call`

**Step 3: Implement the struct changes**

Replace `Part` (line 173-176):
```rust
#[derive(Debug, Serialize, Clone)]
struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<FunctionCallPart>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<FunctionResponsePart>,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionCallPart {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionResponsePart {
    name: String,
    response: serde_json::Value,
}
```

Add to `ResponsePart` (line 221-228):
```rust
#[derive(Debug, Deserialize)]
struct ResponsePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: bool,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FunctionCallResponse>,
}

#[derive(Debug, Deserialize)]
struct FunctionCallResponse {
    name: String,
    args: serde_json::Value,
}
```

Add tool fields to `GenerateContentRequest` (line 121-128):
```rust
#[derive(Debug, Serialize, Clone)]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolDeclaration>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolConfig>,
}

#[derive(Debug, Serialize, Clone)]
struct GeminiToolDeclaration {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Clone)]
struct GeminiToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: FunctionCallingConfigMode,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionCallingConfigMode {
    mode: String,
}
```

Add the same `tools` and `tool_config` optional fields to `InternalGenerateContentRequest` (line 156-164).

Fix every existing `Part { text: ... }` construction site in the file to use `Part { text: Some(...), function_call: None, function_response: None }`. Search the file for `Part {` — there are ~8 occurrences in `chat_with_system`, `chat_with_history`, `chat`, and tests.

Fix every `GenerateContentRequest { ... }` construction site to add `tools: None, tool_config: None`. There is one in `send_generate_content` at ~line 1291.

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw --lib providers::gemini -- --no-capture`
Expected: All pass, including the 4 new tests

**Step 5: Commit**

```
git add src/providers/gemini.rs
git commit -m "feat(gemini): extend Part/Request structs for native function calling"
```

---

## Task 2: Override provider trait methods for native tools

**Files:**
- Modify: `src/providers/gemini.rs` (Provider impl block, ~line 1452)

**Step 1: Write failing test**

```rust
#[test]
fn gemini_provider_capabilities_include_native_tools() {
    let provider = GeminiProvider::new(
        Some("test-key".into()),
        None, None, None,
    );
    let caps = provider.capabilities();
    assert!(caps.native_tool_calling);
    assert!(caps.vision);
}

#[test]
fn gemini_provider_convert_tools_returns_gemini_payload() {
    use crate::providers::traits::{ToolSpec, ToolsPayload};
    let provider = GeminiProvider::new(
        Some("test-key".into()),
        None, None, None,
    );
    let tools = vec![ToolSpec {
        name: "slack_dm".into(),
        description: "Send a DM".into(),
        parameters: serde_json::json!({"type": "object", "properties": {"user_id": {"type": "string"}}}),
    }];
    let payload = provider.convert_tools(&tools);
    match payload {
        ToolsPayload::Gemini { function_declarations } => {
            assert_eq!(function_declarations.len(), 1);
            assert_eq!(function_declarations[0]["name"], "slack_dm");
        }
        _ => panic!("Expected Gemini payload"),
    }
}

#[test]
fn gemini_supports_native_tools() {
    let provider = GeminiProvider::new(
        Some("test-key".into()),
        None, None, None,
    );
    assert!(provider.supports_native_tools());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests::gemini_provider_capabilities -- --no-capture`
Expected: FAIL — `capabilities()` returns default (native_tool_calling: false)

**Step 3: Add trait overrides**

In the `impl Provider for GeminiProvider` block (~line 1452), add before `chat_with_system`:

```rust
fn capabilities(&self) -> ProviderCapabilities {
    ProviderCapabilities {
        native_tool_calling: true,
        vision: true,
    }
}

fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
    ToolsPayload::Gemini {
        function_declarations: tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect(),
    }
}
```

Add the necessary imports at the top of the Provider impl: `use crate::providers::traits::{ProviderCapabilities, ToolSpec, ToolsPayload};`

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib providers::gemini -- --no-capture`
Expected: All pass

**Step 5: Commit**

```
git add src/providers/gemini.rs
git commit -m "feat(gemini): declare native tool calling capabilities"
```

---

## Task 3: Implement chat_with_tools and response parsing

**Files:**
- Modify: `src/providers/gemini.rs` — `send_generate_content` signature, `chat_with_tools` override, `CandidateContent` parsing

**Step 1: Write failing test**

```rust
#[test]
fn candidate_content_extracts_function_calls() {
    let content = CandidateContent {
        parts: vec![
            ResponsePart {
                text: None,
                thought: false,
                function_call: Some(FunctionCallResponse {
                    name: "slack_dm".into(),
                    args: serde_json::json!({"user_id": "U123", "message": "hello"}),
                }),
            },
        ],
    };
    let (text, calls) = content.extract_response();
    assert!(text.is_none());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "slack_dm");
}

#[test]
fn candidate_content_extracts_mixed_text_and_calls() {
    let content = CandidateContent {
        parts: vec![
            ResponsePart {
                text: Some("I'll send a DM.".into()),
                thought: false,
                function_call: None,
            },
            ResponsePart {
                text: None,
                thought: false,
                function_call: Some(FunctionCallResponse {
                    name: "slack_dm".into(),
                    args: serde_json::json!({"user_id": "U123"}),
                }),
            },
        ],
    };
    let (text, calls) = content.extract_response();
    assert_eq!(text.as_deref(), Some("I'll send a DM."));
    assert_eq!(calls.len(), 1);
}
```

**Step 2: Run tests to verify they fail**

Expected: `extract_response` method does not exist

**Step 3: Implement**

Add a new method on `CandidateContent` alongside existing `effective_text`:

```rust
/// Extract text and function calls from response parts.
fn extract_response(self) -> (Option<String>, Vec<ToolCall>) {
    let mut answer_parts: Vec<String> = Vec::new();
    let mut first_thinking: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for part in self.parts {
        if let Some(fc) = part.function_call {
            tool_calls.push(ToolCall {
                id: format!("gemini_call_{}", tool_calls.len()),
                name: fc.name,
                arguments: fc.args.to_string(),
            });
        }
        if let Some(text) = part.text {
            if text.is_empty() {
                continue;
            }
            if !part.thought {
                answer_parts.push(text);
            } else if first_thinking.is_none() {
                first_thinking = Some(text);
            }
        }
    }

    let text = if answer_parts.is_empty() {
        first_thinking
    } else {
        Some(answer_parts.join(""))
    };

    (text, tool_calls)
}
```

Change `send_generate_content` to accept optional tools and return a richer response struct. Add parameters `tools: Option<Vec<GeminiToolDeclaration>>` and `tool_config: Option<GeminiToolConfig>`. Populate the `GenerateContentRequest` with these. Parse `extract_response` instead of `effective_text` at the end. Return a new `GeminiResponse` struct:

```rust
struct GeminiResponse {
    text: Option<String>,
    tool_calls: Vec<ToolCall>,
    usage: Option<TokenUsage>,
}
```

Update all three callers of `send_generate_content` (`chat_with_system`, `chat_with_history`, `chat`) to pass `None, None` for tools/tool_config and destructure the new return type.

Override `chat_with_tools` in the Provider impl:

```rust
async fn chat_with_tools(
    &self,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
    model: &str,
    temperature: f64,
) -> anyhow::Result<ChatResponse> {
    let messages = sanitize_transcript_for_gemini(messages);
    let mut system_parts: Vec<&str> = Vec::new();
    let mut contents: Vec<Content> = Vec::new();

    for msg in &messages {
        match msg.role.as_str() {
            "system" => system_parts.push(&msg.content),
            "user" => contents.push(Content {
                role: Some("user".into()),
                parts: vec![Part { text: Some(msg.content.clone()), function_call: None, function_response: None }],
            }),
            "assistant" => {
                // Check if this is a tool-call message (JSON with tool_calls field)
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    if let Some(tool_calls) = parsed.get("tool_calls").and_then(|tc| tc.as_array()) {
                        let parts: Vec<Part> = tool_calls.iter().filter_map(|tc| {
                            Some(Part {
                                text: None,
                                function_call: Some(FunctionCallPart {
                                    name: tc.get("name")?.as_str()?.to_string(),
                                    args: serde_json::from_str(tc.get("arguments")?.as_str()?).ok()?,
                                }),
                                function_response: None,
                            })
                        }).collect();
                        if !parts.is_empty() {
                            contents.push(Content { role: Some("model".into()), parts });
                            continue;
                        }
                    }
                }
                contents.push(Content {
                    role: Some("model".into()),
                    parts: vec![Part { text: Some(msg.content.clone()), function_call: None, function_response: None }],
                });
            }
            "tool" => {
                // Parse tool result and convert to functionResponse
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    let tool_call_id = parsed.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let content_str = parsed.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    contents.push(Content {
                        role: Some("user".into()),
                        parts: vec![Part {
                            text: None,
                            function_call: None,
                            function_response: Some(FunctionResponsePart {
                                name: tool_call_id.to_string(),
                                response: serde_json::json!({"result": content_str}),
                            }),
                        }],
                    });
                }
            }
            _ => {}
        }
    }

    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(Content {
            role: None,
            parts: vec![Part { text: Some(system_parts.join("\n\n")), function_call: None, function_response: None }],
        })
    };

    let gemini_tools = if tools.is_empty() {
        None
    } else {
        Some(vec![GeminiToolDeclaration {
            function_declarations: tools.to_vec(),
        }])
    };

    let tool_config = if gemini_tools.is_some() {
        Some(GeminiToolConfig {
            function_calling_config: FunctionCallingConfigMode {
                mode: "AUTO".into(),
            },
        })
    } else {
        None
    };

    let resp = self.send_generate_content(
        contents, system_instruction, model, temperature,
        gemini_tools, tool_config,
    ).await?;

    Ok(ChatResponse {
        text: resp.text,
        tool_calls: resp.tool_calls,
        usage: resp.usage,
        reasoning_content: None,
    })
}
```

**Step 4: Run all tests**

Run: `cargo test -p zeroclaw --lib providers::gemini -- --no-capture`
Expected: All pass

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

**Step 5: Commit**

```
git add src/providers/gemini.rs
git commit -m "feat(gemini): implement chat_with_tools and function call parsing"
```

---

## Task 4: Config schema — add tool script paths

**Files:**
- Modify: `src/config/schema.rs` — add `ToolsConfig` struct and field on `Config`

**Step 1: Write failing test**

In `src/config/schema.rs` tests:

```rust
#[test]
fn tools_config_defaults_to_none() {
    let config = Config::default();
    assert!(config.tools.slack_script.is_none());
    assert!(config.tools.linear_script.is_none());
}
```

**Step 2: Run to verify fail**

Expected: `Config` has no field `tools`

**Step 3: Add the config struct**

Add near the other config sub-structs:

```rust
/// External CLI tool script configuration (`[tools]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ToolsConfig {
    /// Path to Slack CLI script relative to workspace (e.g. "skills/slack/scripts/slack-cli.ts").
    pub slack_script: Option<String>,
    /// Path to Linear CLI script relative to workspace (e.g. "skills/linear/scripts/linear-cli.ts").
    pub linear_script: Option<String>,
}
```

Add to the `Config` struct (after `web_search`):

```rust
/// External CLI tool scripts (`[tools]`).
#[serde(default)]
pub tools: ToolsConfig,
```

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib config -- --no-capture`
Expected: All pass

**Step 5: Commit**

```
git add src/config/schema.rs
git commit -m "feat(config): add tools.slack_script and tools.linear_script config fields"
```

---

## Task 5: Slack tool module — shared config and runner

**Files:**
- Create: `src/tools/slack/mod.rs`
- Modify: `src/tools/mod.rs` — add `pub mod slack;`

**Step 1: Write failing test**

In `src/tools/slack/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_tool_config_resolves_script_path() {
        let cfg = SlackToolConfig::new("skills/slack/scripts/slack-cli.ts", std::path::Path::new("/workspace"));
        assert_eq!(cfg.script_path, std::path::PathBuf::from("/workspace/skills/slack/scripts/slack-cli.ts"));
        assert_eq!(cfg.workspace_dir, std::path::PathBuf::from("/workspace"));
    }
}
```

**Step 2: Run to verify fail**

Expected: Module doesn't exist

**Step 3: Create `src/tools/slack/mod.rs`**

```rust
pub mod dm;
pub mod dm_history;
pub mod history;
pub mod presence;
pub mod react;
pub mod send;
pub mod send_file;
pub mod send_thread;
pub mod threads;

use std::path::{Path, PathBuf};

pub struct SlackToolConfig {
    pub script_path: PathBuf,
    pub workspace_dir: PathBuf,
}

impl SlackToolConfig {
    pub fn new(script: &str, workspace_dir: &Path) -> Self {
        Self {
            script_path: workspace_dir.join(script),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Run the Slack CLI script with the given args. Returns stdout on success.
    pub async fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("npx")
            .args(["tsx", &self.script_path.to_string_lossy()])
            .args(args)
            .current_dir(&self.workspace_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("slack-cli failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_tool_config_resolves_script_path() {
        let cfg = SlackToolConfig::new("skills/slack/scripts/slack-cli.ts", Path::new("/workspace"));
        assert_eq!(cfg.script_path, PathBuf::from("/workspace/skills/slack/scripts/slack-cli.ts"));
        assert_eq!(cfg.workspace_dir, PathBuf::from("/workspace"));
    }
}
```

Add `pub mod slack;` to `src/tools/mod.rs` (after `pub mod shell;` line 53). Also add `pub use slack::SlackToolConfig;`.

**Step 4: Run tests** (this will fail on missing submodule files — create stubs)

Create each submodule as an empty file for now. They'll be populated in Tasks 6-7.

**Step 5: Run tests**

Run: `cargo test -p zeroclaw --lib tools::slack -- --no-capture`
Expected: Pass

**Step 6: Commit**

```
git add src/tools/slack/ src/tools/mod.rs
git commit -m "feat(tools): add Slack tool module with shared config and runner"
```

---

## Task 6: Implement Slack write tools (dm, send, send_thread, send_file, react)

**Files:**
- Create: `src/tools/slack/dm.rs`, `send.rs`, `send_thread.rs`, `send_file.rs`, `react.rs`

Each file follows the same pattern. Implement one at a time with its test. Here's `dm.rs` as the template — repeat the pattern for each.

**Step 1: Write `dm.rs` with test**

```rust
use super::SlackToolConfig;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::sync::Arc;

pub struct SlackDmTool {
    config: Arc<SlackToolConfig>,
}

impl SlackDmTool {
    pub fn new(config: Arc<SlackToolConfig>) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for SlackDmTool {
    fn name(&self) -> &str { "slack_dm" }
    fn description(&self) -> &str { "Send a direct message to a Slack user" }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "user_id": {"type": "string", "description": "Slack user ID (e.g. U05SQR41Q0L)"},
                "message": {"type": "string", "description": "Message text to send"},
                "ritual": {"type": "string", "description": "Ritual context for this action"},
                "context": {"type": "string", "description": "Reason for sending this message"}
            },
            "required": ["user_id", "message", "ritual", "context"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let user_id = args["user_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing user_id"))?;
        let message = args["message"].as_str().ok_or_else(|| anyhow::anyhow!("missing message"))?;
        let ritual = args["ritual"].as_str().ok_or_else(|| anyhow::anyhow!("missing ritual"))?;
        let context = args["context"].as_str().ok_or_else(|| anyhow::anyhow!("missing context"))?;
        let output = self.config.run(&["dm", user_id, message, "--ritual", ritual, "--context", context]).await?;
        Ok(ToolResult { success: true, output, error: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_dm_tool_metadata() {
        let cfg = Arc::new(SlackToolConfig::new("test.ts", std::path::Path::new("/tmp")));
        let tool = SlackDmTool::new(cfg);
        assert_eq!(tool.name(), "slack_dm");
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("ritual")));
        assert!(required.contains(&serde_json::json!("context")));
    }
}
```

Repeat this pattern for `send.rs` (name: `slack_send`, CLI cmd: `send`, params: `channel_id, message, ritual, context`), `send_thread.rs` (name: `slack_send_thread`, CLI cmd: `send-thread`, params: `channel_id, thread_ts, message, ritual, context`), `send_file.rs` (name: `slack_send_file`, CLI cmd: `send-file`, params: `channel_id, file_path, ritual, context`), `react.rs` (name: `slack_react`, CLI cmd: `react`, params: `channel_id, timestamp, emoji, ritual, context`).

**Step 2: Run tests**

Run: `cargo test -p zeroclaw --lib tools::slack -- --no-capture`
Expected: All pass

**Step 3: Commit**

```
git add src/tools/slack/
git commit -m "feat(tools): implement Slack write tools (dm, send, send_thread, send_file, react)"
```

---

## Task 7: Implement Slack read tools (history, dm_history, threads, presence)

**Files:**
- Create: `src/tools/slack/history.rs`, `dm_history.rs`, `threads.rs`, `presence.rs`

Same pattern as Task 6 but read tools omit `ritual` and `context` from required params.

Example for `history.rs`:
- name: `slack_history`, CLI cmd: `history`, params: `channel_id` (required), `limit` (optional integer)
- No `ritual`/`context` in required

**Step 1: Implement all four read tools with tests**

**Step 2: Run tests**

Run: `cargo test -p zeroclaw --lib tools::slack -- --no-capture`

**Step 3: Commit**

```
git add src/tools/slack/
git commit -m "feat(tools): implement Slack read tools (history, dm_history, threads, presence)"
```

---

## Task 8: Linear tool module — shared config, all 14 tools

**Files:**
- Create: `src/tools/linear/mod.rs` and all 14 submodule files
- Modify: `src/tools/mod.rs` — add `pub mod linear;`

Identical structure to Slack. `LinearToolConfig` has the same shape as `SlackToolConfig`.

Write tools (require ritual + context): `create_issue`, `update_issue`, `archive_issue`, `add_comment`, `create_label`, `create_project`, `create_cycle`.

Read tools (no ritual/context): `issues`, `teams`, `users`, `projects`, `cycles`, `labels`, `states`.

CLI commands match the file names with hyphens: `create-issue`, `update-issue`, etc.

**Step 1: Create module, config, and all 14 tool files with tests**

**Step 2: Run tests**

Run: `cargo test -p zeroclaw --lib tools::linear -- --no-capture`

**Step 3: Commit**

```
git add src/tools/linear/ src/tools/mod.rs
git commit -m "feat(tools): implement Linear tool module with 14 typed tools"
```

---

## Task 9: Register Slack/Linear tools conditionally

**Files:**
- Modify: `src/tools/mod.rs` — `all_tools_with_runtime` function (~line 197-349)

**Step 1: Write failing test**

```rust
#[test]
fn all_tools_includes_slack_when_configured() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig { backend: "markdown".into(), ..MemoryConfig::default() };
    let mem: Arc<dyn Memory> = Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
    let browser = BrowserConfig::default();
    let http = crate::config::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    cfg.tools.slack_script = Some("test.ts".into());

    let tools = all_tools(
        Arc::new(cfg.clone()), &security, mem, None, None,
        &browser, &http, &crate::config::WebFetchConfig::default(),
        tmp.path(), &HashMap::new(), None, &cfg,
    );
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"slack_dm"));
    assert!(names.contains(&"slack_history"));
}

#[test]
fn all_tools_excludes_slack_when_not_configured() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig { backend: "markdown".into(), ..MemoryConfig::default() };
    let mem: Arc<dyn Memory> = Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
    let browser = BrowserConfig::default();
    let http = crate::config::HttpRequestConfig::default();
    let cfg = test_config(&tmp);

    let tools = all_tools(
        Arc::new(Config::default()), &security, mem, None, None,
        &browser, &http, &crate::config::WebFetchConfig::default(),
        tmp.path(), &HashMap::new(), None, &cfg,
    );
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(!names.contains(&"slack_dm"));
}
```

**Step 2: Run to verify fail**

**Step 3: Add registration code**

In `all_tools_with_runtime`, after the web search block (~line 298) and before the PDF block (~line 301), add:

```rust
// Slack CLI tools (conditional on config)
if let Some(ref slack_script) = root_config.tools.slack_script {
    let cfg = Arc::new(slack::SlackToolConfig::new(slack_script, workspace_dir));
    tool_arcs.push(Arc::new(slack::dm::SlackDmTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::send::SlackSendTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::send_thread::SlackSendThreadTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::send_file::SlackSendFileTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::history::SlackHistoryTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::dm_history::SlackDmHistoryTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::threads::SlackThreadsTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::presence::SlackPresenceTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(slack::react::SlackReactTool::new(Arc::clone(&cfg))));
}

// Linear CLI tools (conditional on config)
if let Some(ref linear_script) = root_config.tools.linear_script {
    let cfg = Arc::new(linear::LinearToolConfig::new(linear_script, workspace_dir));
    tool_arcs.push(Arc::new(linear::issues::LinearIssuesTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::create_issue::LinearCreateIssueTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::update_issue::LinearUpdateIssueTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::archive_issue::LinearArchiveIssueTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::add_comment::LinearAddCommentTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::teams::LinearTeamsTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::users::LinearUsersTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::projects::LinearProjectsTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::cycles::LinearCyclesTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::labels::LinearLabelsTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::states::LinearStatesTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::create_label::LinearCreateLabelTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::create_project::LinearCreateProjectTool::new(Arc::clone(&cfg))));
    tool_arcs.push(Arc::new(linear::create_cycle::LinearCreateCycleTool::new(Arc::clone(&cfg))));
}
```

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib tools -- --no-capture`
Expected: All pass

**Step 5: Commit**

```
git add src/tools/mod.rs
git commit -m "feat(tools): register Slack and Linear tools conditionally from config"
```

---

## Task 10: WatchStore — SQLite schema and CRUD

**Files:**
- Create: `src/watches/mod.rs`
- Modify: `src/lib.rs` — add `pub mod watches;`
- Modify: `src/memory/sqlite.rs` — add watches table to `init_schema`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> WatchStore {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        WatchStore::init_schema(&conn).unwrap();
        WatchStore { conn }
    }

    #[test]
    fn register_and_retrieve_watch() {
        let store = test_store();
        let id = store.register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U123".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "Waiting for standup reply".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: Some(240),
            on_expire: Some("Post summary".into()),
            channel_name: "slack".into(),
        }).unwrap();
        assert!(!id.is_empty());

        let watches = store.active_watches();
        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].id, id);
        assert_eq!(watches[0].context, "Waiting for standup reply");
    }

    #[test]
    fn check_message_matches_user_id() {
        let store = test_store();
        store.register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U123".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "test context".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        }).unwrap();

        // Should match
        let result = store.check_message("U123", "D456", None, "slack");
        assert!(result.is_some());

        // Wrong user — should not match
        let result = store.check_message("U999", "D456", None, "slack");
        assert!(result.is_none());
    }

    #[test]
    fn mark_matched_removes_from_active() {
        let store = test_store();
        let id = store.register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: Some("U123".into()),
            match_channel_id: None,
            match_thread_ts: None,
            context: "test".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        }).unwrap();

        store.mark_matched(&id).unwrap();
        assert!(store.active_watches().is_empty());
    }

    #[test]
    fn cancel_watch() {
        let store = test_store();
        let id = store.register(&NewWatch {
            event_type: "dm_reply".into(),
            match_user_id: None,
            match_channel_id: None,
            match_thread_ts: None,
            context: "test".into(),
            reminder_after_minutes: None,
            reminder_message: None,
            expires_minutes: None,
            on_expire: None,
            channel_name: "slack".into(),
        }).unwrap();

        store.cancel(&id).unwrap();
        assert!(store.active_watches().is_empty());
    }
}
```

**Step 2: Run to verify fail**

Expected: Module doesn't exist

**Step 3: Create `src/watches/mod.rs`**

Implement `WatchStore`, `Watch`, `NewWatch` structs, `init_schema`, and all CRUD methods. Use `uuid::Uuid::new_v4()` for IDs (already a dependency) or a simple timestamp-based ID.

The `check_message` query:
```sql
SELECT * FROM watches WHERE status = 'active' AND channel_name = ?
  AND (match_user_id IS NULL OR match_user_id = ?)
  AND (match_channel_id IS NULL OR match_channel_id = ?)
  AND (match_thread_ts IS NULL OR match_thread_ts = ?)
LIMIT 1
```

Add `pub mod watches;` to `src/lib.rs` (after `pub mod tools;` line 71).

Also add the watches table creation to `src/memory/sqlite.rs` `init_schema()` — append after the embedding_cache block (~line 171).

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib watches -- --no-capture`
Expected: All pass

**Step 5: Commit**

```
git add src/watches/ src/lib.rs src/memory/sqlite.rs
git commit -m "feat(watches): implement WatchStore with SQLite schema and CRUD"
```

---

## Task 11: WatchManager — timer spawning and cancellation

**Files:**
- Modify: `src/watches/mod.rs` — add WatchManager

**Step 1: Write failing tests**

```rust
#[tokio::test]
async fn watch_manager_register_and_check() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    WatchStore::init_schema(&conn).unwrap();
    let store = Arc::new(WatchStore { conn });
    let (tx, _rx) = tokio::sync::mpsc::channel(16);

    let manager = WatchManager::new(store, tx);
    let id = manager.register(NewWatch {
        event_type: "dm_reply".into(),
        match_user_id: Some("U123".into()),
        match_channel_id: None,
        match_thread_ts: None,
        context: "test".into(),
        reminder_after_minutes: None,
        reminder_message: None,
        expires_minutes: None,
        on_expire: None,
        channel_name: "slack".into(),
    }).await.unwrap();

    let matched = manager.check_message("U123", "D456", None, "slack").await;
    assert!(matched.is_some());
    assert_eq!(matched.unwrap().id, id);
}

#[tokio::test]
async fn watch_manager_cancel_kills_timers() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    WatchStore::init_schema(&conn).unwrap();
    let store = Arc::new(WatchStore { conn });
    let (tx, _rx) = tokio::sync::mpsc::channel(16);

    let manager = WatchManager::new(store, tx);
    let id = manager.register(NewWatch {
        event_type: "dm_reply".into(),
        match_user_id: Some("U123".into()),
        match_channel_id: None,
        match_thread_ts: None,
        context: "test".into(),
        reminder_after_minutes: Some(1),
        reminder_message: Some("nudge".into()),
        expires_minutes: Some(5),
        on_expire: Some("expire prompt".into()),
        channel_name: "slack".into(),
    }).await.unwrap();

    manager.cancel(&id).await.unwrap();
    // Timers should be removed
    assert!(manager.timers.lock().await.get(&id).is_none());
}
```

**Step 2: Run to verify fail**

**Step 3: Implement WatchManager**

Add to `src/watches/mod.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use crate::channels::traits::ChannelMessage;

pub struct WatchManager {
    store: Arc<WatchStore>,
    pub(crate) timers: Mutex<HashMap<String, CancellationToken>>,
    channel_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
}
```

Implement `new`, `register` (with timer spawning), `check_message` (with timer cancellation), `cancel`, and `init` (for daemon restart recovery).

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib watches -- --no-capture`
Expected: All pass

**Step 5: Commit**

```
git add src/watches/mod.rs
git commit -m "feat(watches): implement WatchManager with event-driven timer spawning"
```

---

## Task 12: Watch tools (watch, watch_list, watch_cancel)

**Files:**
- Create: `src/watches/tools.rs`
- Modify: `src/watches/mod.rs` — add `pub mod tools;`
- Modify: `src/tools/mod.rs` — register watch tools

**Step 1: Implement three tools with tests**

Each tool wraps `WatchManager` (passed as `Arc<WatchManager>`):

- `WatchTool` — name: `watch`, calls `manager.register()`
- `WatchListTool` — name: `watch_list`, calls `manager.store.active_watches()`
- `WatchCancelTool` — name: `watch_cancel`, calls `manager.cancel()`

Tests verify name, schema, and required fields.

**Step 2: Register in `all_tools_with_runtime`**

Watch tools need `Arc<WatchManager>` passed into the function. Add an optional `watch_manager: Option<Arc<WatchManager>>` parameter, or register them in the orchestrator startup where the manager is available.

**Step 3: Run tests**

Run: `cargo test -p zeroclaw --lib watches -- --no-capture`

**Step 4: Commit**

```
git add src/watches/ src/tools/mod.rs
git commit -m "feat(watches): implement watch, watch_list, watch_cancel tools"
```

---

## Task 13: Wire WatchManager into orchestrator dispatch loop

**Files:**
- Modify: `src/channels/orchestrator.rs` — `run_message_dispatch_loop` (~line 1797)

**Step 1: Write test**

Test that when a WatchManager has an active watch and a matching message arrives, the message content gets the watch context prepended.

**Step 2: Add watch_manager parameter and injection**

Change `run_message_dispatch_loop` signature to accept `Option<Arc<WatchManager>>`. At line 1810, after `while let Some(msg) = rx.recv().await`, insert the watch check:

```rust
let mut msg = msg;
if let Some(ref wm) = watch_manager {
    if let Some(watch) = wm.check_message(
        &msg.sender,
        &msg.reply_target,
        msg.thread_ts.as_deref(),
        &msg.channel,
    ).await {
        msg.content = format!(
            "[Watch context — id: {}]\n{}\n\n---\nIncoming message:\n{}",
            watch.id, watch.context, msg.content
        );
    }
}
```

Update all callers of `run_message_dispatch_loop` to pass the watch manager (or `None` initially).

**Step 3: Run full test suite**

Run: `cargo test -p zeroclaw -- --no-capture`
Run: `cargo clippy --all-targets -- -D warnings`
Expected: All pass, no warnings

**Step 4: Commit**

```
git add src/channels/orchestrator.rs
git commit -m "feat(watches): wire WatchManager into message dispatch loop"
```

---

## Task 14: Full integration verification

**Step 1: Run full test suite**

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

**Step 2: Verify no regressions**

Check that all existing tests pass, no new warnings, code is formatted.

**Step 3: Final commit if any fixups needed**

```
git add -A
git commit -m "chore: fix clippy/fmt issues from rain runtime extensions"
```
