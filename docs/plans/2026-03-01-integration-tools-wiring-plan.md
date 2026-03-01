# Integration Tools Wiring Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire native integration tools into the agent's tool registry and remove the redundant CLI tool system.

**Architecture:** Replace CLI tool registration in `all_tools_with_runtime()` with a call to `collect_integrations()`, which already creates the native tools. Delete CLI tool modules, the `ToolsConfig` struct, and dead code.

**Tech Stack:** Rust, serde, async-trait

---

### Task 1: Wire integration tools into the tool registry

**Files:**
- Modify: `src/tools/mod.rs:323-400` (replace CLI blocks with integration wiring)

**Step 1: Write the failing test**

Add a test to `src/tools/mod.rs` that verifies integration tools appear in the registry when `[integrations.slack]` is configured:

```rust
#[test]
fn all_tools_includes_integration_slack_when_configured() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = crate::config::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    cfg.integrations.slack = Some(crate::config::SlackIntegrationConfig {
        bot_token: "xoxb-test".into(),
        app_token: "xapp-test".into(),
        channel_id: None,
        allowed_users: vec![],
        mention_only: true,
        mention_regex: None,
        triage_model: None,
    });

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        mem,
        None,
        None,
        &browser,
        &http,
        &crate::config::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
    );
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"slack_send"), "expected slack_send from integration");
    assert!(names.contains(&"slack_history"), "expected slack_history from integration");
}

#[test]
fn all_tools_includes_integration_linear_when_configured() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = crate::config::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    cfg.integrations.linear = Some(crate::config::LinearIntegrationConfig {
        api_key: "lin_api_test".into(),
    });

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        mem,
        None,
        None,
        &browser,
        &http,
        &crate::config::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
    );
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"linear_issues"), "expected linear_issues from integration");
    assert!(names.contains(&"linear_create_issue"), "expected linear_create_issue from integration");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib tools::tests::all_tools_includes_integration_slack_when_configured -- --nocapture`
Expected: FAIL — `slack_send` not found in tools list (integration tools not wired yet)

**Step 3: Wire integration tools into `all_tools_with_runtime()`**

In `src/tools/mod.rs`, replace lines 323-400 (the two CLI registration blocks) with:

```rust
    // Native integration tools (Slack, Linear, etc.)
    for integration in crate::integrations::collect_integrations(root_config) {
        for tool in integration.tools() {
            tool_arcs.push(tool);
        }
    }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib tools::tests::all_tools_includes_integration -- --nocapture`
Expected: PASS — both slack and linear integration tests pass

**Step 5: Commit**

```
feat(tools): wire native integration tools into agent tool registry

Replaces CLI-script-based Slack/Linear tool registration with native
integration tools from collect_integrations(). Integration tools use
direct API calls instead of spawning external TypeScript processes.
```

---

### Task 2: Delete CLI tool modules

**Files:**
- Delete: `src/tools/slack/` (10 files)
- Delete: `src/tools/linear/` (15 files)
- Modify: `src/tools/mod.rs:43,55,85,99` (remove module declarations and re-exports)

**Step 1: Delete the directories**

```bash
rm -rf src/tools/slack/ src/tools/linear/
```

**Step 2: Remove module declarations and re-exports from `src/tools/mod.rs`**

Remove these lines:
- Line 43: `pub mod linear;`
- Line 55: `pub mod slack;`
- Line 85: `pub use linear::LinearToolConfig;` (and the `#[allow(unused_imports)]` on line 84)
- Line 99: `pub use slack::SlackToolConfig;` (and the `#[allow(unused_imports)]` on line 98)

**Step 3: Delete the four CLI tool tests from `src/tools/mod.rs`**

Remove these test functions (lines 703-855):
- `all_tools_includes_slack_when_configured`
- `all_tools_excludes_slack_when_not_configured`
- `all_tools_includes_linear_when_configured`
- `all_tools_excludes_linear_when_not_configured`

**Step 4: Run tests to verify nothing broke**

Run: `cargo test --lib tools::tests`
Expected: PASS — all remaining tests pass, no compilation errors

**Step 5: Commit**

```
refactor(tools): remove CLI-based Slack/Linear tool modules

These are fully replaced by native integration tools in
src/integrations/{slack,linear}/. The CLI tools required external
TypeScript scripts; native tools call APIs directly.

Removes 25 files.
```

---

### Task 3: Delete `ToolsConfig` struct and `config.tools` field

**Files:**
- Modify: `src/config/integrations.rs:900-909` (delete `ToolsConfig` struct)
- Modify: `src/config/schema.rs:148-150` (remove `tools` field from `Config`)
- Modify: `src/config/schema.rs:311` (remove `tools: ToolsConfig::default()` from `Default`)
- Modify: `src/config/schema.rs:1549-1554` (delete `tools_config_defaults_to_none` test)
- Modify: `src/config/schema.rs:1854` (remove from test config construction)
- Modify: `src/config/schema.rs:2038` (remove from test config construction)
- Modify: `src/config/mod.rs:23` (remove `ToolsConfig` from re-exports)
- Modify: `src/onboard/wizard.rs:149` (remove `tools` field)
- Modify: `src/onboard/wizard.rs:485` (remove `tools` field)

**Step 1: Delete `ToolsConfig` from `src/config/integrations.rs`**

Remove lines 900-909 (the comment, struct definition, and both fields).

**Step 2: Remove `tools` field from `Config` struct in `src/config/schema.rs`**

Remove lines 148-150:
```rust
    /// External CLI tool scripts (`[tools]`).
    #[serde(default)]
    pub tools: ToolsConfig,
```

**Step 3: Remove all `tools: ToolsConfig::default()` from Default impls and test constructors**

- `src/config/schema.rs` line 311
- `src/config/schema.rs` line 1854
- `src/config/schema.rs` line 2038
- `src/onboard/wizard.rs` line 149
- `src/onboard/wizard.rs` line 485

**Step 4: Delete the `tools_config_defaults_to_none` test**

Remove lines 1549-1554 from `src/config/schema.rs`.

**Step 5: Remove `ToolsConfig` from re-exports in `src/config/mod.rs`**

Line 23: remove `ToolsConfig` from the `use` list.

**Step 6: Remove `root_config.tools` reference from `src/tools/mod.rs`**

This was already handled in Task 1 (the CLI blocks that referenced `root_config.tools` were replaced).

**Step 7: Run tests**

Run: `cargo test --lib`
Expected: PASS — compiles clean, all tests pass

**Step 8: Commit**

```
chore(config): remove empty ToolsConfig struct

ToolsConfig held slack_script and linear_script paths for the now-deleted
CLI tool system. Native integrations use [integrations.*] config instead.
```

---

### Task 4: Remove dead code and debug dump

**Files:**
- Modify: `src/integrations/slack/client.rs:141-166` (delete `api_post_multipart`)
- Modify: `src/providers/gemini.rs:1473-1478` (delete debug dump)

**Step 1: Delete `api_post_multipart()` from `src/integrations/slack/client.rs`**

Remove lines 141-166 (the method and its doc comment). This method is never called — file uploads use the 3-step presigned URL flow in the `SlackSendFileTool`.

**Step 2: Delete the debug request dump from `src/providers/gemini.rs`**

Remove lines 1473-1478:
```rust
        if tracing::enabled!(tracing::Level::DEBUG) {
            if let Ok(body) = serde_json::to_string_pretty(&request) {
                let _ = std::fs::write("/tmp/gemini-request-dump.json", &body);
                tracing::debug!(len = body.len(), "Gemini request dumped to /tmp/gemini-request-dump.json");
            }
        }
```

**Step 3: Run full validation**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: PASS — no warnings, no test failures

**Step 4: Commit**

```
chore: remove dead api_post_multipart and gemini debug dump

api_post_multipart was never called (file uploads use presigned URLs).
The gemini request dump was temporary debugging code writing to /tmp.
```

---

## Validation

After all tasks, run full CI checks:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Summary

| Task | What | Files touched |
|------|------|---------------|
| 1 | Wire integration tools into registry | `src/tools/mod.rs` |
| 2 | Delete CLI tool modules | `src/tools/{slack,linear}/` (25 files), `src/tools/mod.rs` |
| 3 | Delete `ToolsConfig` | `src/config/{integrations.rs,schema.rs,mod.rs}`, `src/onboard/wizard.rs` |
| 4 | Remove dead code + debug dump | `src/integrations/slack/client.rs`, `src/providers/gemini.rs` |
