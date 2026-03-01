# Design: Wire Native Integration Tools into Agent Tool Registry

**Date:** 2026-03-01
**Status:** Draft
**Slug:** `integration-tools-wiring`

## Problem

Native integration tools (9 Slack, 14 Linear) implement the `Tool` trait and are created by `collect_integrations()` in `src/integrations/mod.rs`, but they never reach the agent's tool registry. The LLM cannot call them.

The old CLI tool system (`src/tools/slack/`, `src/tools/linear/`) wraps external TypeScript scripts and *is* wired into the registry via `all_tools_with_runtime()`. Native integrations are a strict functional superset of CLI tools: same tool names, direct API calls, better error handling, no external script dependencies.

## Solution

Wire native integration tools into the registry and remove the CLI tool system entirely.

## Changes

### 1. Wire integration tools in `all_tools_with_runtime()` (`src/tools/mod.rs`)

Call `collect_integrations(root_config)` inside the function (no signature change needed since `root_config: &Config` is already a parameter). Append each integration's tools to `tool_arcs`.

Replace the two conditional CLI registration blocks (lines ~323-400) with:

```rust
for integration in crate::integrations::collect_integrations(root_config) {
    for tool in integration.tools() {
        tool_arcs.push(tool);
    }
}
```

### 2. Delete CLI tool modules

Remove entirely:
- `src/tools/slack/` (10 files: mod.rs + 9 tool modules)
- `src/tools/linear/` (15 files: mod.rs + 14 tool modules)

Remove from `src/tools/mod.rs`:
- `pub mod slack;` and `pub mod linear;` declarations
- `pub use slack::SlackToolConfig;` and `pub use linear::LinearToolConfig;` re-exports
- Four CLI tool tests (`all_tools_includes_slack_when_configured`, etc.)

### 3. Delete `ToolsConfig`

Remove from `src/config/schema.rs`:
- The `ToolsConfig` struct (held only `slack_script` and `linear_script`)
- The `tools` field from `Config`
- Related test assertions

### 4. Remove dead code

- `api_post_multipart()` in `src/integrations/slack/client.rs` (never called)
- Debug request body dump in `src/providers/gemini.rs` (~line 1473)

## What stays unchanged

- `src/integrations/` (Slack and Linear native tools, clients, trait) -- this is what we're wiring up
- `all_tools_with_runtime()` function signature -- `root_config` already provides access to integrations config
- Agent loop call sites in `src/agent/loop_.rs` -- no changes needed
- Provider `convert_tools()` -- generic, works with any `ToolSpec`
- Helper function duplication across `slack/tools.rs` and `linear/tools.rs` -- acceptable per rule-of-three (only 2 integrations)

## Risk

**Low.** Tool names are identical between CLI and native versions. The `Tool` trait interface is the same. No provider-specific changes needed.

**Backward compatibility:** None. Users with `[tools]` config section will get a parse error on startup (if `deny_unknown_fields` is active) or silent ignore. This is intentional -- the config key is gone and `[integrations.*]` is the replacement.

## Rollback

Revert the commit.
