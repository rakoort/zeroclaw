use crate::config::Config;
use crate::security::SecurityPolicy;
use anyhow::{bail, Result};

mod schedule;
mod store;
#[cfg(test)]
mod test_agent_fields;
mod types;

pub mod scheduler;

#[allow(unused_imports)]
pub use schedule::{
    next_run_for_schedule, normalize_expression, schedule_cron_expression, validate_schedule,
};
#[allow(unused_imports)]
pub use store::{
    add_agent_job, add_job, add_shell_job, due_jobs, find_by_name, get_job, list_jobs, list_runs,
    record_last_run, record_run, remove_job, reschedule_after_run, update_job,
};
pub use types::{
    CronJob, CronJobPatch, CronRun, DeliveryConfig, JobType, Schedule, SessionTarget, UpsertResult,
};

fn upsert_verb(result: UpsertResult) -> &'static str {
    match result {
        UpsertResult::Created => "\u{2705} Added",
        UpsertResult::Updated => "\u{2705} Updated",
    }
}

fn validate_job_type(job_type: &str) -> Result<()> {
    match job_type {
        "shell" | "agent" => Ok(()),
        other => bail!("Invalid --type '{other}'. Expected 'shell' or 'agent'."),
    }
}

fn require_name_for_agent(job_type: &str, name: Option<&String>) -> Result<()> {
    if job_type == "agent" && name.is_none() {
        bail!("--name is required for agent jobs (used as unique key for idempotent registration)");
    }
    Ok(())
}

fn build_delivery_config(
    channel: Option<String>,
    to: Option<String>,
) -> Result<Option<DeliveryConfig>> {
    if channel.is_none() && to.is_some() {
        bail!("--delivery-to requires --delivery-channel");
    }
    match channel {
        None => Ok(None),
        Some(ch) => Ok(Some(DeliveryConfig {
            mode: "announce".into(),
            channel: Some(ch),
            to,
            best_effort: true,
        })),
    }
}

#[allow(clippy::needless_pass_by_value)]
pub fn handle_command(command: crate::CronCommands, config: &Config) -> Result<()> {
    match command {
        crate::CronCommands::List => {
            let jobs = list_jobs(config)?;
            if jobs.is_empty() {
                println!("No scheduled tasks yet.");
                println!("\nUsage:");
                println!("  zeroclaw cron add '0 9 * * *' 'agent -m \"Good morning!\"'");
                return Ok(());
            }

            println!("🕒 Scheduled jobs ({}):", jobs.len());
            for job in jobs {
                let last_run = job
                    .last_run
                    .map_or_else(|| "never".into(), |d| d.to_rfc3339());
                let last_status = job.last_status.unwrap_or_else(|| "n/a".into());
                println!(
                    "- {} | {:?} | next={} | last={} ({})",
                    job.id,
                    job.schedule,
                    job.next_run.to_rfc3339(),
                    last_run,
                    last_status,
                );
                if !job.command.is_empty() {
                    println!("    cmd: {}", job.command);
                }
                if let Some(prompt) = &job.prompt {
                    println!("    prompt: {prompt}");
                }
            }
            Ok(())
        }
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
            validate_job_type(&job_type)?;
            require_name_for_agent(&job_type, name.as_ref())?;
            match job_type.as_str() {
                "agent" => {
                    let delivery = build_delivery_config(delivery_channel, delivery_to)?;
                    let target = SessionTarget::parse(&session_target);
                    let (job, upsert) = add_agent_job(
                        config, name, schedule, &command, target, model, delivery, false,
                    )?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} agent cron job {}", job.id);
                    println!("  Expr  : {}", job.expression);
                    println!("  Next  : {}", job.next_run.to_rfc3339());
                    println!("  Prompt: {}", job.prompt.as_deref().unwrap_or(""));
                }
                _ => {
                    let (job, upsert) = add_shell_job(config, name, schedule, &command)?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} cron job {}", job.id);
                    println!("  Expr: {}", job.expression);
                    println!("  Next: {}", job.next_run.to_rfc3339());
                    println!("  Cmd : {}", job.command);
                }
            }
            Ok(())
        }
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
            validate_job_type(&job_type)?;
            require_name_for_agent(&job_type, name.as_ref())?;
            match job_type.as_str() {
                "agent" => {
                    let delivery = build_delivery_config(delivery_channel, delivery_to)?;
                    let target = SessionTarget::parse(&session_target);
                    let (job, upsert) = add_agent_job(
                        config, name, schedule, &command, target, model, delivery, true,
                    )?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} one-shot agent cron job {}", job.id);
                    println!("  At    : {}", job.next_run.to_rfc3339());
                    println!("  Prompt: {}", job.prompt.as_deref().unwrap_or(""));
                }
                _ => {
                    let (job, upsert) = add_shell_job(config, name, schedule, &command)?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} one-shot cron job {}", job.id);
                    println!("  At  : {}", job.next_run.to_rfc3339());
                    println!("  Cmd : {}", job.command);
                }
            }
            Ok(())
        }
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
            validate_job_type(&job_type)?;
            require_name_for_agent(&job_type, name.as_ref())?;
            match job_type.as_str() {
                "agent" => {
                    let delivery = build_delivery_config(delivery_channel, delivery_to)?;
                    let target = SessionTarget::parse(&session_target);
                    let (job, upsert) = add_agent_job(
                        config, name, schedule, &command, target, model, delivery, false,
                    )?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} interval agent cron job {}", job.id);
                    println!("  Every(ms): {every_ms}");
                    println!("  Next     : {}", job.next_run.to_rfc3339());
                    println!("  Prompt   : {}", job.prompt.as_deref().unwrap_or(""));
                }
                _ => {
                    let (job, upsert) = add_shell_job(config, name, schedule, &command)?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} interval cron job {}", job.id);
                    println!("  Every(ms): {every_ms}");
                    println!("  Next     : {}", job.next_run.to_rfc3339());
                    println!("  Cmd      : {}", job.command);
                }
            }
            Ok(())
        }
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
            validate_job_type(&job_type)?;
            require_name_for_agent(&job_type, name.as_ref())?;
            match job_type.as_str() {
                "agent" => {
                    let duration = parse_delay(&delay)?;
                    let at = chrono::Utc::now() + duration;
                    let schedule = Schedule::At { at };
                    let delivery = build_delivery_config(delivery_channel, delivery_to)?;
                    let target = SessionTarget::parse(&session_target);
                    let (job, upsert) = add_agent_job(
                        config, name, schedule, &command, target, model, delivery, true,
                    )?;
                    let verb = upsert_verb(upsert);
                    println!("{verb} one-shot agent cron job {}", job.id);
                    println!("  At    : {}", job.next_run.to_rfc3339());
                    println!("  Prompt: {}", job.prompt.as_deref().unwrap_or(""));
                }
                _ => {
                    let (job, _) = add_once(config, &delay, &command)?;
                    println!("\u{2705} Added one-shot cron job {}", job.id);
                    println!("  At  : {}", job.next_run.to_rfc3339());
                    println!("  Cmd : {}", job.command);
                }
            }
            Ok(())
        }
        crate::CronCommands::Update {
            id,
            expression,
            tz,
            command,
            name,
        } => {
            if expression.is_none() && tz.is_none() && command.is_none() && name.is_none() {
                bail!("At least one of --expression, --tz, --command, or --name must be provided");
            }

            // Merge expression/tz with the existing schedule so that
            // --tz alone updates the timezone and --expression alone
            // preserves the existing timezone.
            let schedule = if expression.is_some() || tz.is_some() {
                let existing = get_job(config, &id)?;
                let (existing_expr, existing_tz) = match existing.schedule {
                    Schedule::Cron {
                        expr,
                        tz: existing_tz,
                    } => (expr, existing_tz),
                    _ => bail!("Cannot update expression/tz on a non-cron schedule"),
                };
                Some(Schedule::Cron {
                    expr: expression.unwrap_or(existing_expr),
                    tz: tz.or(existing_tz),
                })
            } else {
                None
            };

            if let Some(ref cmd) = command {
                let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
                if !security.is_command_allowed(cmd) {
                    bail!("Command blocked by security policy: {cmd}");
                }
            }

            let patch = CronJobPatch {
                schedule,
                command,
                name,
                ..CronJobPatch::default()
            };

            let job = update_job(config, &id, patch)?;
            println!("\u{2705} Updated cron job {}", job.id);
            println!("  Expr: {}", job.expression);
            println!("  Next: {}", job.next_run.to_rfc3339());
            println!("  Cmd : {}", job.command);
            Ok(())
        }
        crate::CronCommands::Remove { id } => remove_job(config, &id),
        crate::CronCommands::Pause { id } => {
            pause_job(config, &id)?;
            println!("⏸️  Paused cron job {id}");
            Ok(())
        }
        crate::CronCommands::Resume { id } => {
            resume_job(config, &id)?;
            println!("▶️  Resumed cron job {id}");
            Ok(())
        }
    }
}

pub fn add_once(config: &Config, delay: &str, command: &str) -> Result<(CronJob, UpsertResult)> {
    let duration = parse_delay(delay)?;
    let at = chrono::Utc::now() + duration;
    add_once_at(config, at, command)
}

pub fn add_once_at(
    config: &Config,
    at: chrono::DateTime<chrono::Utc>,
    command: &str,
) -> Result<(CronJob, UpsertResult)> {
    let schedule = Schedule::At { at };
    add_shell_job(config, None, schedule, command)
}

pub fn pause_job(config: &Config, id: &str) -> Result<CronJob> {
    update_job(
        config,
        id,
        CronJobPatch {
            enabled: Some(false),
            ..CronJobPatch::default()
        },
    )
}

pub fn resume_job(config: &Config, id: &str) -> Result<CronJob> {
    update_job(
        config,
        id,
        CronJobPatch {
            enabled: Some(true),
            ..CronJobPatch::default()
        },
    )
}

fn parse_delay(input: &str) -> Result<chrono::Duration> {
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("delay must not be empty");
    }
    let split = input
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(input.len());
    let (num, unit) = input.split_at(split);
    let amount: i64 = num.parse()?;
    let unit = if unit.is_empty() { "m" } else { unit };
    let duration = match unit {
        "s" => chrono::Duration::seconds(amount),
        "m" => chrono::Duration::minutes(amount),
        "h" => chrono::Duration::hours(amount),
        "d" => chrono::Duration::days(amount),
        _ => anyhow::bail!("unsupported delay unit '{unit}', use s/m/h/d"),
    };
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    fn make_job(config: &Config, expr: &str, tz: Option<&str>, cmd: &str) -> CronJob {
        let (job, _) = add_shell_job(
            config,
            None,
            Schedule::Cron {
                expr: expr.into(),
                tz: tz.map(Into::into),
            },
            cmd,
        )
        .unwrap();
        job
    }

    fn run_update(
        config: &Config,
        id: &str,
        expression: Option<&str>,
        tz: Option<&str>,
        command: Option<&str>,
        name: Option<&str>,
    ) -> Result<()> {
        handle_command(
            crate::CronCommands::Update {
                id: id.into(),
                expression: expression.map(Into::into),
                tz: tz.map(Into::into),
                command: command.map(Into::into),
                name: name.map(Into::into),
            },
            config,
        )
    }

    #[test]
    fn update_changes_command_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo original");

        run_update(&config, &job.id, None, None, Some("echo updated"), None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.command, "echo updated");
        assert_eq!(updated.id, job.id);
    }

    #[test]
    fn update_changes_expression_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        run_update(&config, &job.id, Some("0 9 * * *"), None, None, None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.expression, "0 9 * * *");
    }

    #[test]
    fn update_changes_name_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        run_update(&config, &job.id, None, None, None, Some("new-name")).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.name.as_deref(), Some("new-name"));
    }

    #[test]
    fn update_tz_alone_sets_timezone() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        run_update(
            &config,
            &job.id,
            None,
            Some("America/Los_Angeles"),
            None,
            None,
        )
        .unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(
            updated.schedule,
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: Some("America/Los_Angeles".into()),
            }
        );
    }

    #[test]
    fn update_expression_preserves_existing_tz() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(
            &config,
            "*/5 * * * *",
            Some("America/Los_Angeles"),
            "echo test",
        );

        run_update(&config, &job.id, Some("0 9 * * *"), None, None, None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(
            updated.schedule,
            Schedule::Cron {
                expr: "0 9 * * *".into(),
                tz: Some("America/Los_Angeles".into()),
            }
        );
    }

    #[test]
    fn update_preserves_unchanged_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let (job, _) = add_shell_job(
            &config,
            Some("original-name".into()),
            Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo original",
        )
        .unwrap();

        run_update(&config, &job.id, None, None, Some("echo changed"), None).unwrap();

        let updated = get_job(&config, &job.id).unwrap();
        assert_eq!(updated.command, "echo changed");
        assert_eq!(updated.name.as_deref(), Some("original-name"));
        assert_eq!(updated.expression, "*/5 * * * *");
    }

    #[test]
    fn update_no_flags_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = make_job(&config, "*/5 * * * *", None, "echo test");

        let result = run_update(&config, &job.id, None, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("At least one of"));
    }

    #[test]
    fn update_nonexistent_job_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = run_update(
            &config,
            "nonexistent-id",
            None,
            None,
            Some("echo test"),
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn update_security_allows_safe_command() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
        assert!(security.is_command_allowed("echo safe"));
    }

    #[test]
    fn validate_job_type_accepts_shell() {
        super::validate_job_type("shell").unwrap();
    }

    #[test]
    fn validate_job_type_accepts_agent() {
        super::validate_job_type("agent").unwrap();
    }

    #[test]
    fn validate_job_type_rejects_invalid() {
        let err = super::validate_job_type("foobar").unwrap_err();
        assert!(err.to_string().contains("Invalid --type"));
    }

    #[test]
    fn build_delivery_config_none_when_no_channel() {
        assert!(super::build_delivery_config(None, None).unwrap().is_none());
    }

    #[test]
    fn build_delivery_config_some_when_channel_set() {
        let cfg = super::build_delivery_config(Some("discord".into()), Some("123456".into()))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.channel, Some("discord".into()));
        assert_eq!(cfg.to, Some("123456".into()));
        assert_eq!(cfg.mode, "announce");
    }

    #[test]
    fn build_delivery_config_rejects_to_without_channel() {
        let err = super::build_delivery_config(None, Some("123".into())).unwrap_err();
        assert!(err.to_string().contains("--delivery-to requires"));
    }

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
                name: Some("at-reminder".into()),
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
                name: Some("health-check".into()),
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

    #[test]
    fn agent_job_requires_name_at_cli_level() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                tz: None,
                command: "Run standup".into(),
                job_type: "agent".into(),
                model: None,
                session_target: "isolated".into(),
                delivery_channel: None,
                delivery_to: None,
                name: None,
            },
            &config,
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--name is required for agent jobs"));
    }

    #[test]
    fn shell_job_allows_omitting_name() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        handle_command(
            crate::CronCommands::Add {
                expression: "*/5 * * * *".into(),
                tz: None,
                command: "echo hello".into(),
                job_type: "shell".into(),
                model: None,
                session_target: "isolated".into(),
                delivery_channel: None,
                delivery_to: None,
                name: None,
            },
            &config,
        )
        .unwrap();

        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].name.is_none());
    }

    #[test]
    fn add_agent_job_upserts_via_handler() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        // First add
        handle_command(
            crate::CronCommands::Add {
                expression: "0 9 * * *".into(),
                tz: None,
                command: "Run standup v1".into(),
                job_type: "agent".into(),
                model: None,
                session_target: "isolated".into(),
                delivery_channel: None,
                delivery_to: None,
                name: Some("standup".into()),
            },
            &config,
        )
        .unwrap();

        assert_eq!(list_jobs(&config).unwrap().len(), 1);

        // Same name, different schedule -> should upsert
        handle_command(
            crate::CronCommands::Add {
                expression: "0 10 * * *".into(),
                tz: None,
                command: "Run standup v2".into(),
                job_type: "agent".into(),
                model: None,
                session_target: "isolated".into(),
                delivery_channel: None,
                delivery_to: None,
                name: Some("standup".into()),
            },
            &config,
        )
        .unwrap();

        // Still only one job
        let jobs = list_jobs(&config).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].prompt.as_deref(), Some("Run standup v2"));
        assert_eq!(jobs[0].expression, "0 10 * * *");
    }
}
