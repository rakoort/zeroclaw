# CLI Cron Agent Jobs — Design

**Date:** 2026-03-02
**Slug:** `cli-cron-agent-jobs`
**Status:** Accepted

## Problem

All four CLI cron add commands (`add`, `add-at`, `add-every`, `once`) call `add_shell_job`, hardcoding `job_type = shell`. The `add_agent_job` function exists but is reachable only from the agent tool (`src/tools/cron_add.rs`) during a running session.

Cron jobs registered via CLI with natural-language prompts are stored as shell commands. The scheduler tries to execute them with `sh -lc`, and the security policy blocks them:

```
WARN: blocked by security policy: command not allowed:
  Test cron fire — send the word PONG to channel:C088R30DSSW
```

Any deployment that registers cron jobs through the CLI (e.g. Docker `entrypoint.sh`) has non-functional agent cron. The only working path to create agent cron jobs is through the agent's own `cron_add` tool.

## Solution

Add a `--type shell|agent` flag (defaulting to `shell`) to the four CLI add commands. When `--type agent`, route to `add_agent_job` instead of `add_shell_job`. Expose agent-specific options as individual flags.

## CLI Surface

New flags on `Add`, `AddAt`, `AddEvery`, `Once`:

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--type` | `shell \| agent` | `shell` | Job type |
| `--model` | `String` | `None` (uses `cron.model` config) | Model override (agent only) |
| `--session-target` | `isolated \| main` | `isolated` | Session mode (agent only) |
| `--delivery-channel` | `String` | `None` | Channel type: telegram, discord, slack, mattermost (agent only) |
| `--delivery-to` | `String` | `None` | Channel/chat ID (agent only) |
| `--name` | `String` | `None` | Human-readable job name (both types) |

The existing `command` positional argument doubles as the prompt when `--type agent`. No rename needed.

### Usage Examples

```bash
# Agent job — recurring
zeroclaw cron add --type agent '0 9 * * 1-5' 'Summarize overnight alerts'

# Agent job — with delivery
zeroclaw cron add --type agent \
  --model gpt-4o \
  --delivery-channel discord --delivery-to 123456789 \
  '0 9 * * 1-5' 'Run the morning standup ritual'

# Agent job — one-shot at specific time
zeroclaw cron add-at --type agent '2026-03-03T14:00:00Z' 'Send PONG to channel'

# Agent job — one-shot with delay
zeroclaw cron once --type agent '30m' 'Remind me to check the deployment'

# Agent job — fixed interval
zeroclaw cron add-every --type agent 3600000 'Check system health'

# Shell job — unchanged (--type defaults to shell)
zeroclaw cron add '*/5 * * * *' 'pg_dump mydb > /backups/db.sql'
```

## Handler Changes (`src/cron/mod.rs`)

Each add arm branches on `job_type`:

```
match job_type:
  "agent" → build delivery config, parse session target, call add_agent_job()
  "shell" → call add_shell_job() (existing behavior)
```

The `Once` arm bypasses the `add_once` / `add_once_at` helpers for agent jobs. It builds `Schedule::At` directly and calls `add_agent_job`, matching how `AddAt` handles it.

Output for agent jobs prints `Prompt:` instead of `Cmd:`.

## Files Touched

- `src/lib.rs` — add fields to `CronCommands::Add`, `AddAt`, `AddEvery`, `Once`
- `src/cron/mod.rs` — branch handler logic, add delivery/session-target parsing helpers

## Not In Scope

- No changes to scheduler, store, types, or execution paths
- No `--type` on `cron update` (remove and re-add to change type)
- No changes to the agent tool (`cron_add.rs`)
- No changes to security policy

## Risk

**Low.** The change adds a new code path in the CLI handler. The default (`shell`) preserves existing behavior. The agent execution path (`add_agent_job`, `run_agent_job`) is already tested and production-proven through the agent tool.

**Rollback:** Revert the commit. Any agent jobs already registered will remain in the database and execute correctly — the scheduler handles them regardless of how they were created.
