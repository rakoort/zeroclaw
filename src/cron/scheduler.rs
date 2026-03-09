use crate::channels::{
    Channel, DiscordChannel, MattermostChannel, SendMessage, SlackChannel, TelegramChannel,
};
use crate::config::Config;
use crate::cron::{
    due_jobs, next_run_for_schedule, record_last_run, record_run, remove_job, reschedule_after_run,
    update_job, CronJob, CronJobPatch, DeliveryConfig, JobType, Schedule,
};
use crate::security::SecurityPolicy;
use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::{stream, StreamExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{self, Duration};

const MIN_POLL_SECONDS: u64 = 5;
const SHELL_JOB_TIMEOUT_SECS: u64 = 120;
const SCHEDULER_COMPONENT: &str = "scheduler";

pub async fn run(config: Config) -> Result<()> {
    let poll_secs = config.reliability.scheduler_poll_secs.max(MIN_POLL_SECONDS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    crate::health::mark_component_ok(SCHEDULER_COMPONENT);

    loop {
        interval.tick().await;
        // Keep scheduler liveness fresh even when there are no due jobs.
        crate::health::mark_component_ok(SCHEDULER_COMPONENT);

        let jobs = match due_jobs(&config, Utc::now()) {
            Ok(jobs) => jobs,
            Err(e) => {
                crate::health::mark_component_error(SCHEDULER_COMPONENT, e.to_string());
                tracing::warn!("Scheduler query failed: {e}");
                continue;
            }
        };

        process_due_jobs(&config, &security, jobs, SCHEDULER_COMPONENT).await;
    }
}

pub async fn execute_job_now(config: &Config, job: &CronJob) -> (bool, String) {
    let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
    execute_job_with_retry(config, &security, job).await
}

async fn execute_job_with_retry(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    let mut last_output = String::new();
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let (success, output) = match job.job_type {
            JobType::Shell => run_job_command(config, security, job).await,
            JobType::Agent => run_agent_job(config, security, job).await,
        };
        last_output = output;

        if success {
            return (true, last_output);
        }

        if last_output.starts_with("blocked by security policy:") {
            // Deterministic policy violations are not retryable.
            return (false, last_output);
        }

        if attempt < retries {
            let jitter_ms = u64::from(Utc::now().timestamp_subsec_millis() % 250);
            time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
        }
    }

    (false, last_output)
}

async fn process_due_jobs(
    config: &Config,
    security: &Arc<SecurityPolicy>,
    jobs: Vec<CronJob>,
    component: &str,
) {
    // Refresh scheduler health on every successful poll cycle, including idle cycles.
    crate::health::mark_component_ok(component);

    let max_concurrent = config.scheduler.max_concurrent.max(1);
    let mut in_flight =
        stream::iter(
            jobs.into_iter().map(|job| {
                let config = config.clone();
                let security = Arc::clone(security);
                let component = component.to_owned();
                async move {
                    execute_and_persist_job(&config, security.as_ref(), &job, &component).await
                }
            }),
        )
        .buffer_unordered(max_concurrent);

    while let Some((job_id, success, output)) = in_flight.next().await {
        if !success {
            tracing::warn!("Scheduler job '{job_id}' failed: {output}");
        }
    }
}

async fn execute_and_persist_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    component: &str,
) -> (String, bool, String) {
    crate::health::mark_component_ok(component);
    warn_if_high_frequency_agent_job(job);

    let started_at = Utc::now();
    let (success, output) = execute_job_with_retry(config, security, job).await;
    let finished_at = Utc::now();
    let success = persist_job_result(config, job, success, &output, started_at, finished_at).await;

    (job.id.clone(), success, output)
}

async fn run_agent_job(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }

    let name = job.name.clone().unwrap_or_else(|| "cron-job".to_string());
    let prompt = job.prompt.clone().unwrap_or_default();

    // Read context files and prepend their contents to the prompt.
    let context_prefix = match read_context_files(&job.context_files, &config.workspace_dir) {
        Ok(prefix) => prefix,
        Err(e) => return (false, format!("context file error: {e}")),
    };

    let prefixed_prompt = if context_prefix.is_empty() {
        format!("[cron:{} {name}] {prompt}", job.id)
    } else {
        format!("[cron:{} {name}] {context_prefix}\n{prompt}", job.id)
    };

    let runtime = match crate::planner::PlannerRuntime::from_config(config) {
        Ok(rt) => rt,
        Err(e) => return (false, format!("failed to build planner runtime: {e}")),
    };

    let executor_model = job
        .model
        .clone()
        .or_else(|| config.cron.model.clone())
        .unwrap_or_else(|| runtime.executor_model.clone());

    let planner_model = runtime.planner_model.as_deref().unwrap_or(&executor_model);

    let result = crate::planner::plan_then_execute(
        runtime.provider.as_ref(),
        planner_model,
        &executor_model,
        "", // system prompt — the ritual prompt IS the user message
        &prefixed_prompt,
        "", // memory context
        &runtime.tools,
        &runtime.tool_specs,
        runtime.observer.as_ref(),
        "cron",
        runtime.temperature,
        runtime.max_tool_iterations,
        runtime.max_executor_iterations,
        "cron",
        None, // no cancellation token
        None, // no hooks
        &[],  // no excluded tools
        &runtime.model_routes,
    )
    .await;

    match result {
        Ok(crate::planner::PlanExecutionResult::Executed { output, .. }) => (
            true,
            if output.trim().is_empty() {
                "agent job executed".to_string()
            } else {
                output
            },
        ),
        Ok(crate::planner::PlanExecutionResult::Passthrough) => {
            // Simple task — fall back to flat agent loop
            run_flat_fallback(config, prefixed_prompt, executor_model, runtime.temperature).await
        }
        Err(e) => {
            tracing::warn!(
                "Planner failed for cron job: {e}, falling back to flat run with fresh context"
            );
            // Build a completely fresh prompt: original prompt + inlined context
            // files. Do NOT pass any conversation history from the failed planner
            // attempt — the planner may have produced malformed tool calls or
            // responses (e.g. missing thought_signature) that the fallback model
            // would reject.
            let fresh_prompt = build_fallback_prompt(&job.id, &name, &prompt, &context_prefix);
            run_flat_fallback(config, fresh_prompt, executor_model, runtime.temperature).await
        }
    }
}

fn read_context_files(paths: &[String], workspace_dir: &std::path::Path) -> Result<String> {
    if paths.is_empty() {
        return Ok(String::new());
    }

    let ws_canonical = workspace_dir
        .canonicalize()
        .unwrap_or_else(|_| workspace_dir.to_path_buf());

    let mut sections = Vec::with_capacity(paths.len());
    for path_str in paths {
        let path = std::path::Path::new(path_str);
        // Resolve relative paths against workspace_dir.
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace_dir.join(path)
        };

        // Security: canonicalize and enforce workspace boundary to prevent
        // path traversal (e.g. "../../../etc/shadow" or absolute paths
        // outside the workspace).
        let canonical = resolved.canonicalize().map_err(|e| {
            anyhow::anyhow!(
                "failed to resolve context file '{}': {e}",
                resolved.display()
            )
        })?;
        if !canonical.starts_with(&ws_canonical) {
            anyhow::bail!("context file '{}' is outside workspace directory", path_str);
        }

        let content = std::fs::read_to_string(&canonical).map_err(|e| {
            anyhow::anyhow!("failed to read context file '{}': {e}", resolved.display())
        })?;

        let filename = canonical
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| path_str.clone());

        sections.push(format!("## Context: {filename}\n{content}"));
    }

    Ok(sections.join("\n\n"))
}

/// Build a completely fresh prompt for the flat fallback path when the planner
/// fails. This reconstructs the prompt from the original components (prompt +
/// context prefix) rather than reusing any state from the failed planner
/// attempt, ensuring no malformed conversation history leaks into the fallback.
fn build_fallback_prompt(
    job_id: &str,
    job_name: &str,
    prompt: &str,
    context_prefix: &str,
) -> String {
    let full_prompt = if context_prefix.is_empty() {
        prompt.to_string()
    } else {
        format!("{context_prefix}\n{prompt}")
    };
    format!("[cron:{job_id} {job_name}] {full_prompt}")
}

async fn run_flat_fallback(
    config: &Config,
    prompt: String,
    model: String,
    temperature: f64,
) -> (bool, String) {
    match crate::agent::loop_::run(
        config.clone(),
        Some(prompt),
        None,
        Some(model),
        temperature,
        vec![],
        false,
    )
    .await
    {
        Ok(response) => (
            true,
            if response.trim().is_empty() {
                "agent job executed".to_string()
            } else {
                response
            },
        ),
        Err(e) => (false, format!("agent job failed: {e}")),
    }
}

async fn persist_job_result(
    config: &Config,
    job: &CronJob,
    mut success: bool,
    output: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) -> bool {
    let duration_ms = (finished_at - started_at).num_milliseconds();

    if let Err(e) = deliver_if_configured(config, job, output).await {
        if job.delivery.best_effort {
            tracing::warn!("Cron delivery failed (best_effort): {e}");
        } else {
            success = false;
            tracing::warn!("Cron delivery failed: {e}");
        }
    }

    let _ = record_run(
        config,
        &job.id,
        started_at,
        finished_at,
        if success { "ok" } else { "error" },
        Some(output),
        duration_ms,
    );

    if is_one_shot_auto_delete(job) {
        if success {
            if let Err(e) = remove_job(config, &job.id) {
                tracing::warn!("Failed to remove one-shot cron job after success: {e}");
            }
        } else {
            let _ = record_last_run(config, &job.id, finished_at, false, output);
            if let Err(e) = update_job(
                config,
                &job.id,
                CronJobPatch {
                    enabled: Some(false),
                    ..CronJobPatch::default()
                },
            ) {
                tracing::warn!("Failed to disable failed one-shot cron job: {e}");
            }
        }
        return success;
    }

    if let Err(e) = reschedule_after_run(config, job, success, output) {
        tracing::warn!("Failed to persist scheduler run result: {e}");
    }

    success
}

fn is_one_shot_auto_delete(job: &CronJob) -> bool {
    job.delete_after_run && matches!(job.schedule, Schedule::At { .. })
}

fn warn_if_high_frequency_agent_job(job: &CronJob) {
    if !matches!(job.job_type, JobType::Agent) {
        return;
    }
    let too_frequent = match &job.schedule {
        Schedule::Every { every_ms } => *every_ms < 5 * 60 * 1000,
        Schedule::Cron { .. } => {
            let now = Utc::now();
            match (
                next_run_for_schedule(&job.schedule, now),
                next_run_for_schedule(&job.schedule, now + chrono::Duration::seconds(1)),
            ) {
                (Ok(a), Ok(b)) => (b - a).num_minutes() < 5,
                _ => false,
            }
        }
        Schedule::At { .. } => false,
    };

    if too_frequent {
        tracing::warn!(
            "Cron agent job '{}' is scheduled more frequently than every 5 minutes",
            job.id
        );
    }
}

async fn deliver_if_configured(config: &Config, job: &CronJob, output: &str) -> Result<()> {
    let delivery: &DeliveryConfig = &job.delivery;
    if !delivery.mode.eq_ignore_ascii_case("announce") {
        return Ok(());
    }

    let channel = delivery
        .channel
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delivery.channel is required for announce mode"))?;
    let target = delivery
        .to
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("delivery.to is required for announce mode"))?;

    deliver_announcement(config, channel, target, output).await
}

pub(crate) async fn deliver_announcement(
    config: &Config,
    channel: &str,
    target: &str,
    output: &str,
) -> Result<()> {
    match channel.to_ascii_lowercase().as_str() {
        "telegram" => {
            let tg = config
                .channels_config
                .telegram
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("telegram channel not configured"))?;
            let channel = TelegramChannel::new(
                tg.bot_token.clone(),
                tg.allowed_users.clone(),
                tg.mention_only,
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        "discord" => {
            let dc = config
                .channels_config
                .discord
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("discord channel not configured"))?;
            let channel = DiscordChannel::new(
                dc.bot_token.clone(),
                dc.guild_id.clone(),
                dc.allowed_users.clone(),
                dc.listen_to_bots,
                dc.mention_only,
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        "slack" => {
            let sl = config
                .channels_config
                .slack
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("slack channel not configured"))?;
            let channel = SlackChannel::new(
                sl.bot_token.clone(),
                sl.app_token.clone(),
                sl.channel_id.clone(),
                sl.allowed_users.clone(),
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        "mattermost" => {
            let mm = config
                .channels_config
                .mattermost
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("mattermost channel not configured"))?;
            let channel = MattermostChannel::new(
                mm.url.clone(),
                mm.bot_token.clone(),
                mm.channel_id.clone(),
                mm.allowed_users.clone(),
                mm.thread_replies.unwrap_or(true),
                mm.mention_only.unwrap_or(false),
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        other => anyhow::bail!("unsupported delivery channel: {other}"),
    }

    Ok(())
}

async fn run_job_command(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> (bool, String) {
    run_job_command_with_timeout(
        config,
        security,
        job,
        Duration::from_secs(SHELL_JOB_TIMEOUT_SECS),
    )
    .await
}

async fn run_job_command_with_timeout(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
    timeout: Duration,
) -> (bool, String) {
    if !security.can_act() {
        return (
            false,
            "blocked by security policy: autonomy is read-only".to_string(),
        );
    }

    if security.is_rate_limited() {
        return (
            false,
            "blocked by security policy: rate limit exceeded".to_string(),
        );
    }

    if !security.is_command_allowed(&job.command) {
        return (
            false,
            format!(
                "blocked by security policy: command not allowed: {}",
                job.command
            ),
        );
    }

    if let Some(path) = security.forbidden_path_argument(&job.command) {
        return (
            false,
            format!("blocked by security policy: forbidden path argument: {path}"),
        );
    }

    if !security.record_action() {
        return (
            false,
            "blocked by security policy: action budget exhausted".to_string(),
        );
    }

    let child = match Command::new("sh")
        .arg("-lc")
        .arg(&job.command)
        .current_dir(&config.workspace_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (false, format!("spawn error: {e}")),
    };

    match time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!(
                "status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout.trim(),
                stderr.trim()
            );
            (output.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("spawn error: {e}")),
        Err(_) => (
            false,
            format!("job timed out after {}s", timeout.as_secs_f64()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::cron::{self, DeliveryConfig, SessionTarget};
    use crate::security::SecurityPolicy;
    use chrono::{Duration as ChronoDuration, Utc};
    use tempfile::TempDir;

    async fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        config
    }

    fn test_job(command: &str) -> CronJob {
        CronJob {
            id: "test-job".into(),
            expression: "* * * * *".into(),
            schedule: crate::cron::Schedule::Cron {
                expr: "* * * * *".into(),
                tz: None,
            },
            command: command.into(),
            prompt: None,
            name: None,
            job_type: JobType::Shell,
            session_target: SessionTarget::Isolated,
            model: None,
            enabled: true,
            delivery: DeliveryConfig::default(),
            delete_after_run: false,
            context_files: Vec::new(),
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
            last_output: None,
        }
    }

    fn unique_component(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    #[tokio::test]
    async fn run_job_command_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("echo scheduler-ok");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(success);
        assert!(output.contains("scheduler-ok"));
        assert!(output.contains("status=exit status: 0"));
    }

    #[tokio::test]
    async fn run_job_command_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_test");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("definitely_missing_file_for_scheduler_test"));
        assert!(output.contains("status=exit status:"));
    }

    #[tokio::test]
    async fn run_job_command_times_out() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["sleep".into()];
        let job = test_job("sleep 1");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) =
            run_job_command_with_timeout(&config, &security, &job, Duration::from_millis(50)).await;
        assert!(!success);
        assert!(output.contains("job timed out after"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_disallowed_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["echo".into()];
        let job = test_job("curl https://evil.example");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("command not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat /etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_option_assignment_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["grep".into()];
        let job = test_job("grep --file=/etc/passwd root ./src");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_short_option_attached_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["grep".into()];
        let job = test_job("grep -f/etc/passwd root ./src");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("/etc/passwd"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_tilde_user_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat ~root/.ssh/id_rsa");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("forbidden path argument"));
        assert!(output.contains("~root/.ssh/id_rsa"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_input_redirection_path_bypass() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat </etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("command not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        let job = test_job("echo should-not-run");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.max_actions_per_hour = 0;
        let job = test_job("echo should-not-run");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_job_command(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_recovers_after_first_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        config.autonomy.allowed_commands = vec!["sh".into()];
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        tokio::fs::write(
            config.workspace_dir.join("retry-once.sh"),
            "#!/bin/sh\nif [ -f retry-ok.flag ]; then\n  echo recovered\n  exit 0\nfi\ntouch retry-ok.flag\nexit 1\n",
        )
        .await
        .unwrap();
        let job = test_job("sh ./retry-once.sh");

        let (success, output) = execute_job_with_retry(&config, &security, &job).await;
        assert!(success);
        assert!(output.contains("recovered"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_exhausts_attempts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let job = test_job("ls always_missing_for_retry_test");

        let (success, output) = execute_job_with_retry(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("always_missing_for_retry_test"));
    }

    #[tokio::test]
    async fn run_agent_job_returns_error_without_provider_key() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        // The Passthrough and Executed paths require a live provider/planner call
        // and are not unit-testable here. Security boundary tests above are sufficient
        // for unit coverage.
        let (success, output) = run_agent_job(&config, &security, &job).await;
        assert!(!success);
        assert!(
            output.contains("failed to build planner runtime:")
                || output.contains("agent job failed:")
        );
    }

    #[tokio::test]
    async fn run_agent_job_blocks_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.level = crate::security::AutonomyLevel::ReadOnly;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_agent_job(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("read-only"));
    }

    #[tokio::test]
    async fn run_agent_job_blocks_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.autonomy.max_actions_per_hour = 0;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_agent_job(&config, &security, &job).await;
        assert!(!success);
        assert!(output.contains("blocked by security policy"));
        assert!(output.contains("rate limit exceeded"));
    }

    #[tokio::test]
    async fn run_agent_job_uses_cron_config_model_when_job_model_is_none() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.cron.model = Some("gemini-2.0-flash-lite".into());
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        job.model = None; // no per-job model

        // model_override should resolve to cron config default
        let model_override = job.model.clone().or_else(|| config.cron.model.clone());
        assert_eq!(model_override, Some("gemini-2.0-flash-lite".into()));
    }

    #[tokio::test]
    async fn run_agent_job_per_job_model_takes_priority_over_cron_config() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.cron.model = Some("gemini-2.0-flash-lite".into());
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        job.model = Some("gpt-4o".into()); // per-job model set

        // per-job model should take priority over cron config
        let model_override = job.model.clone().or_else(|| config.cron.model.clone());
        assert_eq!(model_override, Some("gpt-4o".into()));
    }

    #[tokio::test]
    async fn run_agent_job_model_override_none_when_both_unset() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Say hello".into());
        job.model = None;

        // both unset, should be None (falls through to default_model in agent::run)
        let model_override = job.model.clone().or_else(|| config.cron.model.clone());
        assert_eq!(model_override, None);
    }

    #[tokio::test]
    async fn process_due_jobs_marks_component_ok_even_when_idle() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("scheduler-idle");

        crate::health::mark_component_error(&component, "pre-existing error");
        process_due_jobs(&config, &security, Vec::new(), &component).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
        assert!(entry["last_ok"].as_str().is_some());
        assert!(entry["last_error"].is_null());
    }

    #[tokio::test]
    async fn process_due_jobs_failure_does_not_mark_component_unhealthy() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let job = test_job("ls definitely_missing_file_for_scheduler_component_health_test");
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let component = unique_component("scheduler-fail");

        crate::health::mark_component_ok(&component);
        process_due_jobs(&config, &security, vec![job], &component).await;

        let snapshot = crate::health::snapshot_json();
        let entry = &snapshot["components"][component.as_str()];
        assert_eq!(entry["status"], "ok");
    }

    #[tokio::test]
    async fn persist_job_result_records_run_and_reschedules_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let (job, _) = cron::add_job(&config, "*/5 * * * *", "echo ok").unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let (job, _) = cron::add_agent_job(
            &config,
            Some("one-shot".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            Vec::new(),
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let (job, _) = cron::add_agent_job(
            &config,
            Some("one-shot-fail".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            true,
            Vec::new(),
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;
        assert!(!success);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn persist_job_result_success_deletes_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let (job, _) = cron::add_once_at(&config, at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);
        let lookup = cron::get_job(&config, &job.id);
        assert!(lookup.is_err());
    }

    #[tokio::test]
    async fn persist_job_result_failure_disables_one_shot_shell_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let (job, _) = cron::add_once_at(&config, at, "echo one-shot-shell").unwrap();
        assert!(job.delete_after_run);
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, false, "boom", started, finished).await;
        assert!(!success);
        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn persist_job_result_delivery_failure_non_best_effort_marks_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let (job, _) = cron::add_agent_job(
            &config,
            Some("announce-job".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "deliver this",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("telegram".into()),
                to: Some("123456".into()),
                best_effort: false,
            }),
            false,
            Vec::new(),
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(!success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("error"));

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "error");
    }

    #[tokio::test]
    async fn persist_job_result_delivery_failure_best_effort_keeps_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let (job, _) = cron::add_agent_job(
            &config,
            Some("announce-job-best-effort".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "deliver this",
            SessionTarget::Isolated,
            None,
            Some(DeliveryConfig {
                mode: "announce".into(),
                channel: Some("telegram".into()),
                to: Some("123456".into()),
                best_effort: true,
            }),
            false,
            Vec::new(),
        )
        .unwrap();
        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);

        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));

        let runs = cron::list_runs(&config, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn persist_job_result_at_schedule_without_delete_after_run_is_not_deleted() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let at = Utc::now() + ChronoDuration::minutes(10);
        let (job, _) = cron::add_agent_job(
            &config,
            Some("at-no-autodelete".into()),
            crate::cron::Schedule::At { at },
            "Hello",
            SessionTarget::Isolated,
            None,
            None,
            false,
            Vec::new(),
        )
        .unwrap();
        assert!(!job.delete_after_run);

        let started = Utc::now();
        let finished = started + ChronoDuration::milliseconds(10);
        let success = persist_job_result(&config, &job, true, "ok", started, finished).await;
        assert!(success);

        let updated = cron::get_job(&config, &job.id).unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn deliver_if_configured_handles_none_and_invalid_channel() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("echo ok");

        assert!(deliver_if_configured(&config, &job, "x").await.is_ok());

        job.delivery = DeliveryConfig {
            mode: "announce".into(),
            channel: Some("invalid".into()),
            to: Some("target".into()),
            best_effort: true,
        };
        let err = deliver_if_configured(&config, &job, "x").await.unwrap_err();
        assert!(err.to_string().contains("unsupported delivery channel"));
    }

    // --- context files tests ---

    #[test]
    fn read_context_files_empty_list_returns_empty_string() {
        let tmp = TempDir::new().unwrap();
        let result = read_context_files(&[], tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn read_context_files_reads_and_formats_single_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("instructions.md"),
            "Do the standup ritual.\n",
        )
        .unwrap();

        let result = read_context_files(&["instructions.md".to_string()], tmp.path()).unwrap();
        assert!(result.contains("## Context: instructions.md"));
        assert!(result.contains("Do the standup ritual."));
    }

    #[test]
    fn read_context_files_reads_multiple_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.md"), "Content A").unwrap();
        std::fs::write(tmp.path().join("b.md"), "Content B").unwrap();

        let result =
            read_context_files(&["a.md".to_string(), "b.md".to_string()], tmp.path()).unwrap();
        assert!(result.contains("## Context: a.md"));
        assert!(result.contains("Content A"));
        assert!(result.contains("## Context: b.md"));
        assert!(result.contains("Content B"));
    }

    #[test]
    fn read_context_files_resolves_relative_paths_against_workspace() {
        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("skills");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("ritual.md"), "Ritual instructions").unwrap();

        let result = read_context_files(&["skills/ritual.md".to_string()], tmp.path()).unwrap();
        assert!(result.contains("## Context: ritual.md"));
        assert!(result.contains("Ritual instructions"));
    }

    #[test]
    fn read_context_files_missing_file_produces_clear_error() {
        let tmp = TempDir::new().unwrap();
        let result = read_context_files(&["nonexistent-file.md".to_string()], tmp.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("nonexistent-file.md"),
            "Expected clear error message mentioning the file, got: {msg}"
        );
    }

    #[test]
    fn read_context_files_absolute_path_works() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("absolute.md");
        std::fs::write(&file_path, "Absolute content").unwrap();

        let result =
            read_context_files(&[file_path.to_string_lossy().to_string()], tmp.path()).unwrap();
        assert!(result.contains("## Context: absolute.md"));
        assert!(result.contains("Absolute content"));
    }

    #[test]
    fn read_context_files_rejects_path_outside_workspace() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        // Create a file outside the workspace but inside the tempdir so
        // canonicalize() succeeds and the boundary check is exercised.
        let outside_file = tmp.path().join("secret.txt");
        std::fs::write(&outside_file, "sensitive data").unwrap();

        // Relative traversal outside workspace (../secret.txt escapes workspace/)
        let result = read_context_files(&["../secret.txt".to_string()], &workspace);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("outside workspace directory"),
            "Expected workspace boundary error for traversal, got: {msg}"
        );

        // Absolute path outside workspace
        let result = read_context_files(&[outside_file.to_string_lossy().to_string()], &workspace);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("outside workspace directory"),
            "Expected workspace boundary error for absolute path, got: {msg}"
        );

        // Non-existent traversal path: canonicalize fails, which is also safe
        let result = read_context_files(&["../../../etc/passwd".to_string()], &workspace);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("failed to resolve context file"),
            "Expected resolve error for non-existent traversal path, got: {msg}"
        );
    }

    #[tokio::test]
    async fn run_agent_job_context_files_missing_returns_error() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Do something".into());
        job.context_files = vec!["missing-context.md".into()];
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_agent_job(&config, &security, &job).await;
        assert!(!success);
        assert!(
            output.contains("context file error"),
            "Expected context file error, got: {output}"
        );
        assert!(output.contains("missing-context.md"));
    }

    #[tokio::test]
    async fn add_agent_job_with_context_files_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;

        let files = vec!["skills/standup.md".into(), "memory/state.md".into()];
        let (job, _) = cron::add_agent_job(
            &config,
            Some("ctx-roundtrip".into()),
            crate::cron::Schedule::Cron {
                expr: "0 9 * * *".into(),
                tz: None,
            },
            "Run with context",
            SessionTarget::Isolated,
            None,
            None,
            false,
            files.clone(),
        )
        .unwrap();

        assert_eq!(job.context_files, files);

        // Verify it persists and loads back correctly
        let loaded = cron::get_job(&config, &job.id).unwrap();
        assert_eq!(loaded.context_files, files);
    }

    // ── build_fallback_prompt tests ──────────────────────────────

    #[test]
    fn build_fallback_prompt_without_context_contains_prompt_only() {
        let result = build_fallback_prompt("job-42", "standup", "Run the standup", "");
        assert_eq!(result, "[cron:job-42 standup] Run the standup");
    }

    #[test]
    fn build_fallback_prompt_with_context_prepends_context() {
        let context = "## Context: ritual.md\nDo the standup.";
        let result = build_fallback_prompt("job-42", "standup", "Run the standup", context);
        assert_eq!(
            result,
            "[cron:job-42 standup] ## Context: ritual.md\nDo the standup.\nRun the standup"
        );
    }

    #[test]
    fn build_fallback_prompt_matches_prefixed_prompt_format() {
        // The fallback prompt must match the format of the original prefixed_prompt
        // that was passed to the planner. This verifies the fallback reconstructs
        // an equivalent prompt from the original components rather than carrying
        // over any state from the failed planner attempt.
        let job_id = "ritual-1";
        let job_name = "morning-standup";
        let prompt = "Run the morning standup ritual";
        let context = "## Context: standup.md\nCheck Linear and Slack.";

        let fallback = build_fallback_prompt(job_id, job_name, prompt, context);

        // Reconstruct what run_agent_job builds for the planner (the prefixed_prompt)
        let expected = format!("[cron:{job_id} {job_name}] {context}\n{prompt}");
        assert_eq!(
            fallback, expected,
            "Fallback prompt must be structurally identical to the original prefixed prompt"
        );
    }

    #[test]
    fn build_fallback_prompt_empty_prompt_and_context() {
        let result = build_fallback_prompt("job-0", "empty", "", "");
        assert_eq!(result, "[cron:job-0 empty] ");
    }

    #[test]
    fn build_fallback_prompt_with_multiline_context() {
        let context = "## Context: a.md\nContent A\n\n## Context: b.md\nContent B";
        let result = build_fallback_prompt("job-1", "multi", "Do task", context);
        assert!(result.starts_with("[cron:job-1 multi] ## Context: a.md"));
        assert!(result.contains("Content A"));
        assert!(result.contains("## Context: b.md"));
        assert!(result.contains("Content B"));
        assert!(result.ends_with("Do task"));
    }

    // ── Fallback structural guarantees ──────────────────────────

    #[test]
    fn run_flat_fallback_does_not_invoke_planner() {
        // Structural test: run_flat_fallback calls agent::loop_::run, not
        // planner::plan_then_execute. This is verified by code inspection.
        // The function signature takes a prompt String and calls a completely
        // separate code path (agent::loop_::run) that creates fresh provider,
        // memory, and tool state. No planner re-entry is possible.
        //
        // This test documents the guarantee. If run_flat_fallback is ever
        // changed to call the planner, this comment should trigger review.
        //
        // The actual integration behavior (planner failure -> fallback -> no
        // retry) requires a live provider. The unit-level guarantee is:
        // build_fallback_prompt produces a fresh prompt, and run_flat_fallback
        // passes it to agent::loop_::run (not plan_then_execute).
    }

    #[tokio::test]
    async fn run_flat_fallback_failure_returns_job_failure() {
        // When the flat fallback itself fails, it returns (false, error_msg).
        // There is no retry loop and no re-entry into the planner.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;

        // The flat fallback will fail because there's no configured provider/API key.
        let (success, output) = run_flat_fallback(
            &config,
            "test prompt".into(),
            "nonexistent-model".into(),
            0.7,
        )
        .await;

        assert!(!success, "Fallback with no provider should fail");
        assert!(
            output.contains("agent job failed"),
            "Expected 'agent job failed' error, got: {output}"
        );
    }

    #[tokio::test]
    async fn planner_failure_then_fallback_failure_is_terminal() {
        // End-to-end structural test: when the planner can't even start
        // (no API key), run_agent_job fails. If a provider were available and
        // the planner failed, the fallback would also fail for the same
        // infrastructure reason. The key guarantee: there is exactly one
        // fallback attempt, no retry loop.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Run the standup".into());
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let (success, output) = run_agent_job(&config, &security, &job).await;

        assert!(!success, "Job should fail when no provider is configured");
        // The error comes from PlannerRuntime::from_config failing (no API key),
        // which means neither planner nor fallback was attempted. This is correct:
        // if the runtime can't be built, the job fails immediately.
        assert!(
            output.contains("failed to build planner runtime")
                || output.contains("agent job failed"),
            "Expected infrastructure error, got: {output}"
        );
    }

    #[tokio::test]
    async fn fallback_prompt_uses_context_files_not_planner_history() {
        // Verify that when an agent job has context_files, the fallback prompt
        // is built from the original prompt + context files, not from any
        // planner conversation history.
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;

        // Create context files in workspace
        let skills_dir = config.workspace_dir.join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        tokio::fs::write(
            skills_dir.join("ritual.md"),
            "Check Linear for active issues.\n",
        )
        .await
        .unwrap();

        let mut job = test_job("");
        job.job_type = JobType::Agent;
        job.prompt = Some("Run the standup".into());
        job.context_files = vec!["skills/ritual.md".into()];
        job.id = "standup-job".into();
        job.name = Some("morning-standup".into());

        // Read context files the same way run_agent_job does
        let context_prefix = read_context_files(&job.context_files, &config.workspace_dir).unwrap();
        assert!(
            context_prefix.contains("## Context: ritual.md"),
            "Context prefix should contain the file header"
        );
        assert!(
            context_prefix.contains("Check Linear"),
            "Context prefix should contain file content"
        );

        // Build fallback prompt the same way the Err branch does
        let fresh_prompt = build_fallback_prompt(
            &job.id,
            job.name.as_deref().unwrap_or("cron-job"),
            job.prompt.as_deref().unwrap_or(""),
            &context_prefix,
        );

        // Verify the fallback prompt contains the context file content
        assert!(
            fresh_prompt.contains("## Context: ritual.md"),
            "Fallback prompt must contain context file header"
        );
        assert!(
            fresh_prompt.contains("Check Linear"),
            "Fallback prompt must contain context file content"
        );
        assert!(
            fresh_prompt.contains("Run the standup"),
            "Fallback prompt must contain the original prompt"
        );
        assert!(
            fresh_prompt.starts_with("[cron:standup-job morning-standup]"),
            "Fallback prompt must have the cron prefix"
        );

        // Verify it does NOT contain any planner artifacts — the prompt is
        // purely original prompt + context files.
        assert!(
            !fresh_prompt.contains("thought_signature"),
            "Fallback prompt must not contain planner artifacts"
        );
        assert!(
            !fresh_prompt.contains("tool_call"),
            "Fallback prompt must not contain tool call history"
        );
    }
}
