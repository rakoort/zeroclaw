# CLI Cron Agent Jobs — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable the CLI to create agent cron jobs via `--type agent` on existing add commands.

**Architecture:** Add optional agent-related flags to four `CronCommands` variants in `src/lib.rs`, then branch the handler in `src/cron/mod.rs` to call `add_agent_job` when `--type agent`. No changes to scheduler, store, types, or execution paths.

**Tech Stack:** Rust, clap (derive), existing `cron::add_agent_job` / `cron::add_shell_job`.

**Design doc:** `docs/plans/2026-03-02-cli-cron-agent-jobs-design.md`

---

### Task 1: Add agent flags to CronCommands enum

**Files:**
- Modify: `src/lib.rs:197-252` (Add, AddAt, AddEvery, Once variants)

**Step 1: Add fields to all four variants**

Add these optional fields to `Add`, `AddAt`, `AddEvery`, and `Once`:

```rust
/// Job type: shell (default) or agent
#[arg(long, default_value = "shell")]
job_type: String,
/// Model override for agent jobs
#[arg(long)]
model: Option<String>,
/// Session target for agent jobs: isolated (default) or main
#[arg(long, default_value = "isolated")]
session_target: String,
/// Delivery channel type for agent jobs (telegram, discord, slack, mattermost)
#[arg(long)]
delivery_channel: Option<String>,
/// Delivery target (channel ID or chat ID) for agent jobs
#[arg(long)]
delivery_to: Option<String>,
/// Human-readable job name
#[arg(long)]
name: Option<String>,
```

For `Add`, update the `long_about` to mention `--type agent`. For `AddAt`, `AddEvery`, `Once`, do the same.

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | head -30`
Expected: Compiler errors in `src/cron/mod.rs` because the match arms don't destructure the new fields yet. That's expected — we fix the handler in subsequent tasks.

**Step 3: Fix match arms to destructure new fields (ignored for now)**

In `src/cron/mod.rs`, update each match arm to destructure the new fields with `_` prefixes so it compiles without behavior change:

```rust
// Add arm (line 57-71):
crate::CronCommands::Add {
    expression,
    tz,
    command,
    job_type: _job_type,
    model: _model,
    session_target: _session_target,
    delivery_channel: _delivery_channel,
    delivery_to: _delivery_to,
    name: _name,
} => {
    // ... existing body unchanged ...
}
```

Same pattern for `AddAt`, `AddEvery`, `Once`.

**Step 4: Verify it compiles and existing tests pass**

Run: `cargo test --lib cron::tests 2>&1 | tail -20`
Expected: All existing tests pass. No behavior change.

**Step 5: Commit**

```
feat(cron): add agent flags to CLI cron add commands

Add --type, --model, --session-target, --delivery-channel,
--delivery-to, and --name flags to Add, AddAt, AddEvery, and Once
CLI commands. Fields are destructured but unused — wiring follows
in subsequent commits.
```

---

### Task 2: Add helper functions for delivery config and session target parsing

**Files:**
- Modify: `src/cron/mod.rs` (add helpers + tests)

**Step 1: Write failing tests for the helpers**

Add to the `tests` module in `src/cron/mod.rs`:

```rust
#[test]
fn parse_session_target_isolated() {
    assert_eq!(super::parse_session_target("isolated"), SessionTarget::Isolated);
}

#[test]
fn parse_session_target_main() {
    assert_eq!(super::parse_session_target("main"), SessionTarget::Main);
}

#[test]
fn parse_session_target_defaults_to_isolated() {
    assert_eq!(super::parse_session_target("bogus"), SessionTarget::Isolated);
}

#[test]
fn build_delivery_config_none_when_no_channel() {
    assert!(super::build_delivery_config(None, None).is_none());
}

#[test]
fn build_delivery_config_some_when_channel_set() {
    let cfg = super::build_delivery_config(
        Some("discord".into()),
        Some("123456".into()),
    ).unwrap();
    assert_eq!(cfg.channel, Some("discord".into()));
    assert_eq!(cfg.to, Some("123456".into()));
    assert_eq!(cfg.mode, "announce");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib cron::tests::parse_session_target 2>&1 | tail -10`
Expected: FAIL — `parse_session_target` not found.

**Step 3: Implement the helpers**

Add above `handle_command` in `src/cron/mod.rs`:

```rust
fn parse_session_target(s: &str) -> SessionTarget {
    match s {
        "main" => SessionTarget::Main,
        _ => SessionTarget::Isolated,
    }
}

fn build_delivery_config(
    channel: Option<String>,
    to: Option<String>,
) -> Option<DeliveryConfig> {
    channel.as_ref()?;
    Some(DeliveryConfig {
        mode: "announce".into(),
        channel,
        to,
        best_effort: true,
    })
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib cron::tests 2>&1 | tail -20`
Expected: All tests pass including the new ones.

**Step 5: Commit**

```
feat(cron): add helpers for session target and delivery config parsing
```

---

### Task 3: Wire Add handler to support agent jobs

**Files:**
- Modify: `src/cron/mod.rs:57-71` (Add match arm)

**Step 1: Write failing test**

Add to `tests` module in `src/cron/mod.rs`:

```rust
#[test]
fn add_agent_job_via_handler() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    handle_command(
        crate::CronCommands::Add {
            expression: "*/5 * * * *".into(),
            tz: None,
            command: "Summarize alerts".into(),
            job_type: "agent".into(),
            model: Some("gpt-4o".into()),
            session_target: "isolated".into(),
            delivery_channel: None,
            delivery_to: None,
            name: Some("alert-summary".into()),
        },
        &config,
    )
    .unwrap();

    let jobs = list_jobs(&config).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_type, JobType::Agent);
    assert_eq!(jobs[0].prompt.as_deref(), Some("Summarize alerts"));
    assert_eq!(jobs[0].model.as_deref(), Some("gpt-4o"));
    assert_eq!(jobs[0].name.as_deref(), Some("alert-summary"));
    assert!(jobs[0].command.is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib cron::tests::add_agent_job_via_handler 2>&1 | tail -10`
Expected: FAIL — job is created as shell, not agent.

**Step 3: Implement the Add handler branch**

Replace the Add match arm in `handle_command`:

```rust
crate::CronCommands::Add {
    expression,
    tz,
    command,
    job_type,
    model,
    session_target,
    delivery_channel,
    delivery_to,
    name,
} => {
    let schedule = Schedule::Cron {
        expr: expression,
        tz,
    };
    match job_type.as_str() {
        "agent" => {
            let delivery = build_delivery_config(delivery_channel, delivery_to);
            let target = parse_session_target(&session_target);
            let job = add_agent_job(
                config, name, schedule, &command, target, model, delivery, false,
            )?;
            println!("✅ Added agent cron job {}", job.id);
            println!("  Expr  : {}", job.expression);
            println!("  Next  : {}", job.next_run.to_rfc3339());
            println!("  Prompt: {}", job.prompt.as_deref().unwrap_or(""));
        }
        _ => {
            let job = add_shell_job(config, name, schedule, &command)?;
            println!("✅ Added cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
        }
    }
    Ok(())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --lib cron::tests 2>&1 | tail -20`
Expected: All tests pass.

**Step 5: Commit**

```
feat(cron): wire Add CLI handler to support --type agent
```

---

### Task 4: Wire AddAt handler to support agent jobs

**Files:**
- Modify: `src/cron/mod.rs:73-82` (AddAt match arm)

**Step 1: Write failing test**

```rust
#[test]
fn add_at_agent_job_via_handler() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();

    handle_command(
        crate::CronCommands::AddAt {
            at: future,
            command: "Send reminder".into(),
            job_type: "agent".into(),
            model: None,
            session_target: "isolated".into(),
            delivery_channel: Some("discord".into()),
            delivery_to: Some("999".into()),
            name: None,
        },
        &config,
    )
    .unwrap();

    let jobs = list_jobs(&config).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_type, JobType::Agent);
    assert_eq!(jobs[0].prompt.as_deref(), Some("Send reminder"));
    assert_eq!(jobs[0].delivery.channel.as_deref(), Some("discord"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib cron::tests::add_at_agent_job_via_handler 2>&1 | tail -10`
Expected: FAIL.

**Step 3: Implement the AddAt handler branch**

Replace the AddAt match arm:

```rust
crate::CronCommands::AddAt {
    at,
    command,
    job_type,
    model,
    session_target,
    delivery_channel,
    delivery_to,
    name,
} => {
    let at = chrono::DateTime::parse_from_rfc3339(&at)
        .map_err(|e| anyhow::anyhow!("Invalid RFC3339 timestamp for --at: {e}"))?
        .with_timezone(&chrono::Utc);
    let schedule = Schedule::At { at };
    match job_type.as_str() {
        "agent" => {
            let delivery = build_delivery_config(delivery_channel, delivery_to);
            let target = parse_session_target(&session_target);
            let job = add_agent_job(
                config, name, schedule, &command, target, model, delivery, true,
            )?;
            println!("✅ Added one-shot agent job {}", job.id);
            println!("  At    : {}", job.next_run.to_rfc3339());
            println!("  Prompt: {}", job.prompt.as_deref().unwrap_or(""));
        }
        _ => {
            let job = add_shell_job(config, name, schedule, &command)?;
            println!("✅ Added one-shot cron job {}", job.id);
            println!("  At  : {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
        }
    }
    Ok(())
}
```

**Step 4: Run tests**

Run: `cargo test --lib cron::tests 2>&1 | tail -20`
Expected: All pass.

**Step 5: Commit**

```
feat(cron): wire AddAt CLI handler to support --type agent
```

---

### Task 5: Wire AddEvery handler to support agent jobs

**Files:**
- Modify: `src/cron/mod.rs:84-91` (AddEvery match arm)

**Step 1: Write failing test**

```rust
#[test]
fn add_every_agent_job_via_handler() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    handle_command(
        crate::CronCommands::AddEvery {
            every_ms: 60000,
            command: "Check health".into(),
            job_type: "agent".into(),
            model: None,
            session_target: "main".into(),
            delivery_channel: None,
            delivery_to: None,
            name: None,
        },
        &config,
    )
    .unwrap();

    let jobs = list_jobs(&config).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_type, JobType::Agent);
    assert_eq!(jobs[0].prompt.as_deref(), Some("Check health"));
    assert_eq!(jobs[0].session_target, SessionTarget::Main);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib cron::tests::add_every_agent_job_via_handler 2>&1 | tail -10`
Expected: FAIL.

**Step 3: Implement the AddEvery handler branch**

Replace the AddEvery match arm:

```rust
crate::CronCommands::AddEvery {
    every_ms,
    command,
    job_type,
    model,
    session_target,
    delivery_channel,
    delivery_to,
    name,
} => {
    let schedule = Schedule::Every { every_ms };
    match job_type.as_str() {
        "agent" => {
            let delivery = build_delivery_config(delivery_channel, delivery_to);
            let target = parse_session_target(&session_target);
            let job = add_agent_job(
                config, name, schedule, &command, target, model, delivery, false,
            )?;
            println!("✅ Added interval agent job {}", job.id);
            println!("  Every(ms): {every_ms}");
            println!("  Next     : {}", job.next_run.to_rfc3339());
            println!("  Prompt   : {}", job.prompt.as_deref().unwrap_or(""));
        }
        _ => {
            let job = add_shell_job(config, name, schedule, &command)?;
            println!("✅ Added interval cron job {}", job.id);
            println!("  Every(ms): {every_ms}");
            println!("  Next     : {}", job.next_run.to_rfc3339());
            println!("  Cmd      : {}", job.command);
        }
    }
    Ok(())
}
```

**Step 4: Run tests**

Run: `cargo test --lib cron::tests 2>&1 | tail -20`
Expected: All pass.

**Step 5: Commit**

```
feat(cron): wire AddEvery CLI handler to support --type agent
```

---

### Task 6: Wire Once handler to support agent jobs

**Files:**
- Modify: `src/cron/mod.rs:93-98` (Once match arm)

**Step 1: Write failing test**

```rust
#[test]
fn once_agent_job_via_handler() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp);

    handle_command(
        crate::CronCommands::Once {
            delay: "30m".into(),
            command: "Remind me".into(),
            job_type: "agent".into(),
            model: None,
            session_target: "isolated".into(),
            delivery_channel: None,
            delivery_to: None,
            name: Some("reminder".into()),
        },
        &config,
    )
    .unwrap();

    let jobs = list_jobs(&config).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].job_type, JobType::Agent);
    assert_eq!(jobs[0].prompt.as_deref(), Some("Remind me"));
    assert_eq!(jobs[0].name.as_deref(), Some("reminder"));
    assert!(jobs[0].delete_after_run);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib cron::tests::once_agent_job_via_handler 2>&1 | tail -10`
Expected: FAIL.

**Step 3: Implement the Once handler branch**

Replace the Once match arm. Note: bypass `add_once` for agent jobs — build `Schedule::At` directly:

```rust
crate::CronCommands::Once {
    delay,
    command,
    job_type,
    model,
    session_target,
    delivery_channel,
    delivery_to,
    name,
} => {
    match job_type.as_str() {
        "agent" => {
            let duration = parse_delay(&delay)?;
            let at = chrono::Utc::now() + duration;
            let schedule = Schedule::At { at };
            let delivery = build_delivery_config(delivery_channel, delivery_to);
            let target = parse_session_target(&session_target);
            let job = add_agent_job(
                config, name, schedule, &command, target, model, delivery, true,
            )?;
            println!("✅ Added one-shot agent job {}", job.id);
            println!("  At    : {}", job.next_run.to_rfc3339());
            println!("  Prompt: {}", job.prompt.as_deref().unwrap_or(""));
        }
        _ => {
            let job = add_once(config, &delay, &command)?;
            println!("✅ Added one-shot cron job {}", job.id);
            println!("  At  : {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
        }
    }
    Ok(())
}
```

**Step 4: Run tests**

Run: `cargo test --lib cron::tests 2>&1 | tail -20`
Expected: All pass.

**Step 5: Commit**

```
feat(cron): wire Once CLI handler to support --type agent
```

---

### Task 7: Final validation

**Step 1: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: All pass.

**Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: No warnings.

**Step 3: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues.

**Step 4: Verify shell jobs still work (existing behavior preserved)**

Confirm the existing handler tests pass — they exercise shell job creation through the handler without `--type` being set.
