use super::traits::{Observer, ObserverEvent, ObserverMetric};
use std::any::Any;
use tracing::{info, warn};

/// Log-based observer — uses tracing, zero external deps
pub struct LogObserver;

impl LogObserver {
    pub fn new() -> Self {
        Self
    }
}

impl Observer for LogObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::AgentStart { provider, model } => {
                info!(provider = %provider, model = %model, "agent.start");
            }
            ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => {
                let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                info!(provider = %provider, model = %model, duration_ms = ms, tokens = ?tokens_used, cost_usd = ?cost_usd, "agent.end");
            }
            ObserverEvent::ToolCallStart { tool } => {
                info!(tool = %tool, "tool.start");
            }
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => {
                let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                info!(tool = %tool, duration_ms = ms, success = success, "tool.call");
            }
            ObserverEvent::TurnComplete => {
                info!("turn.complete");
            }
            ObserverEvent::ChannelMessage { channel, direction } => {
                info!(channel = %channel, direction = %direction, "channel.message");
            }
            ObserverEvent::HeartbeatTick => {
                info!("heartbeat.tick");
            }
            ObserverEvent::Error { component, message } => {
                info!(component = %component, error = %message, "error");
            }
            ObserverEvent::LlmRequest {
                provider,
                model,
                messages_count,
            } => {
                info!(
                    provider = %provider,
                    model = %model,
                    messages_count = messages_count,
                    "llm.request"
                );
            }
            ObserverEvent::LlmResponse {
                provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
            } => {
                let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                info!(
                    provider = %provider,
                    model = %model,
                    duration_ms = ms,
                    success = success,
                    error = ?error_message,
                    input_tokens = ?input_tokens,
                    output_tokens = ?output_tokens,
                    "llm.response"
                );
            }
            ObserverEvent::ClassificationResult {
                tier,
                confidence,
                agentic_score,
                signals,
            } => {
                info!(
                    tier = %tier,
                    confidence = confidence,
                    agentic_score = agentic_score,
                    signals = ?signals,
                    "classification.result"
                );
            }
            ObserverEvent::PlannerRequest { model } => {
                info!(model = %model, "planner.request");
            }
            ObserverEvent::PlannerResponse { model, plan_text } => {
                info!(
                    model = %model,
                    plan_text_len = plan_text.len(),
                    "planner.response"
                );
            }
            ObserverEvent::FallbackTriggered {
                hint,
                failed_model,
                fallback_model,
                error,
            } => {
                warn!(
                    hint = %hint,
                    failed_model = %failed_model,
                    fallback_model = %fallback_model,
                    error = %error,
                    "fallback.triggered"
                );
            }
            ObserverEvent::IntegrationApiCall {
                integration,
                method,
                success,
                duration_ms,
                error,
                retries,
                status_code,
                response_size_bytes,
                rate_limit_wait_ms,
            } => {
                if *success {
                    info!(
                        integration = %integration,
                        method = %method,
                        duration_ms = duration_ms,
                        retries = retries,
                        status_code = status_code.unwrap_or(0),
                        response_size_bytes = response_size_bytes.unwrap_or(0),
                        rate_limit_wait_ms = rate_limit_wait_ms.unwrap_or(0),
                        "integration.api_call"
                    );
                } else {
                    warn!(
                        integration = %integration,
                        method = %method,
                        duration_ms = duration_ms,
                        retries = retries,
                        status_code = status_code.unwrap_or(0),
                        response_size_bytes = response_size_bytes.unwrap_or(0),
                        rate_limit_wait_ms = rate_limit_wait_ms.unwrap_or(0),
                        error = error.as_deref().unwrap_or("unknown"),
                        "integration.api_call.error"
                    );
                }
            }
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        match metric {
            ObserverMetric::RequestLatency(d) => {
                let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
                info!(latency_ms = ms, "metric.request_latency");
            }
            ObserverMetric::TokensUsed(t) => {
                info!(tokens = t, "metric.tokens_used");
            }
            ObserverMetric::ActiveSessions(s) => {
                info!(sessions = s, "metric.active_sessions");
            }
            ObserverMetric::QueueDepth(d) => {
                info!(depth = d, "metric.queue_depth");
            }
        }
    }

    fn name(&self) -> &str {
        "log"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn log_observer_name() {
        assert_eq!(LogObserver::new().name(), "log");
    }

    #[test]
    fn log_observer_all_events_no_panic() {
        let obs = LogObserver::new();
        obs.record_event(&ObserverEvent::AgentStart {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(500),
            tokens_used: Some(100),
            cost_usd: Some(0.0015),
        });
        obs.record_event(&ObserverEvent::AgentEnd {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(150),
            success: true,
            error_message: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
        });
        obs.record_event(&ObserverEvent::LlmResponse {
            provider: "openrouter".into(),
            model: "claude-sonnet".into(),
            duration: Duration::from_millis(200),
            success: false,
            error_message: Some("rate limited".into()),
            input_tokens: None,
            output_tokens: None,
        });
        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(10),
            success: false,
        });
        obs.record_event(&ObserverEvent::ChannelMessage {
            channel: "telegram".into(),
            direction: "outbound".into(),
        });
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::Error {
            component: "provider".into(),
            message: "timeout".into(),
        });
    }

    #[test]
    fn log_observer_integration_api_call_success_no_panic() {
        let obs = LogObserver::new();
        obs.record_event(&ObserverEvent::IntegrationApiCall {
            integration: "github".into(),
            method: "graphql".into(),
            success: true,
            duration_ms: 200,
            error: None,
            retries: 0,
            status_code: Some(200),
            response_size_bytes: Some(4096),
            rate_limit_wait_ms: None,
        });
    }

    #[test]
    fn log_observer_integration_api_call_failure_no_panic() {
        let obs = LogObserver::new();
        obs.record_event(&ObserverEvent::IntegrationApiCall {
            integration: "slack".into(),
            method: "chat.postMessage".into(),
            success: false,
            duration_ms: 5000,
            error: Some("rate limited".into()),
            retries: 3,
            status_code: Some(429),
            response_size_bytes: None,
            rate_limit_wait_ms: Some(30000),
        });
    }

    #[test]
    fn log_observer_all_metrics_no_panic() {
        let obs = LogObserver::new();
        obs.record_metric(&ObserverMetric::RequestLatency(Duration::from_secs(2)));
        obs.record_metric(&ObserverMetric::TokensUsed(0));
        obs.record_metric(&ObserverMetric::TokensUsed(u64::MAX));
        obs.record_metric(&ObserverMetric::ActiveSessions(1));
        obs.record_metric(&ObserverMetric::QueueDepth(999));
    }
}
