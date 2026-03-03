//! Shared type aliases, constants, structs, and enums for the channel subsystem.

use crate::channels::traits::Channel;
use crate::memory::Memory;
use crate::observability::Observer;
use crate::providers::{self, ChatMessage, Provider};
use crate::tools::Tool;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio_util::sync::CancellationToken;

/// Per-sender conversation history for channel messages.
pub(crate) type ConversationHistoryMap = Arc<Mutex<HashMap<String, Vec<ChatMessage>>>>;
/// Maximum history messages to keep per sender.
pub(crate) const MAX_CHANNEL_HISTORY: usize = 50;
/// Minimum user-message length (in chars) for auto-save to memory.
/// Messages shorter than this (e.g. "ok", "thanks") are not stored,
/// reducing noise in memory recall.
pub(crate) const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

/// Maximum characters per injected workspace file (matches `OpenClaw` default).
pub(crate) const BOOTSTRAP_MAX_CHARS: usize = 20_000;

pub(crate) const DEFAULT_CHANNEL_INITIAL_BACKOFF_SECS: u64 = 2;
pub(crate) const DEFAULT_CHANNEL_MAX_BACKOFF_SECS: u64 = 60;
pub(crate) const MIN_CHANNEL_MESSAGE_TIMEOUT_SECS: u64 = 30;
/// Default timeout for processing a single channel message (LLM + tools).
/// Used as fallback when not configured in channels_config.message_timeout_secs.
pub(crate) const CHANNEL_MESSAGE_TIMEOUT_SECS: u64 = 300;
/// Cap timeout scaling so large max_tool_iterations values do not create unbounded waits.
pub(crate) const CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP: u64 = 4;
pub(crate) const CHANNEL_PARALLELISM_PER_CHANNEL: usize = 4;
pub(crate) const CHANNEL_MIN_IN_FLIGHT_MESSAGES: usize = 8;
pub(crate) const CHANNEL_MAX_IN_FLIGHT_MESSAGES: usize = 64;
pub(crate) const CHANNEL_TYPING_REFRESH_INTERVAL_SECS: u64 = 4;
pub(crate) const CHANNEL_HEALTH_HEARTBEAT_SECS: u64 = 30;
pub(crate) const MODEL_CACHE_FILE: &str = "models_cache.json";
pub(crate) const MODEL_CACHE_PREVIEW_LIMIT: usize = 10;
pub(crate) const MEMORY_CONTEXT_MAX_ENTRIES: usize = 4;
pub(crate) const MEMORY_CONTEXT_ENTRY_MAX_CHARS: usize = 800;
pub(crate) const MEMORY_CONTEXT_MAX_CHARS: usize = 4_000;
pub(crate) const CHANNEL_HISTORY_COMPACT_KEEP_MESSAGES: usize = 12;
pub(crate) const CHANNEL_HISTORY_COMPACT_CONTENT_CHARS: usize = 600;
/// Guardrail for hook-modified outbound channel content.
pub(crate) const CHANNEL_HOOK_MAX_OUTBOUND_CHARS: usize = 20_000;

pub(crate) const TRIAGE_PROMPT: &str = r#"You are monitoring a Slack thread you previously participated in.
A new message arrived. Decide whether you should respond.

Respond YES if:
- You are directly addressed by name or role
- Someone asks a question you can answer
- The conversation needs your input to move forward
- You're being asked to take an action

Respond NO if:
- People are talking to each other
- The message is an acknowledgment (ok, thanks, got it)
- Your input would not add value
- The conversation is proceeding fine without you

Respond with exactly YES or NO."#;

pub(crate) type ProviderCacheMap = Arc<Mutex<HashMap<String, Arc<dyn Provider>>>>;
pub(crate) type RouteSelectionMap = Arc<Mutex<HashMap<String, ChannelRouteSelection>>>;

pub(crate) const SYSTEMD_STATUS_ARGS: [&str; 3] = ["--user", "is-active", "zeroclaw.service"];
pub(crate) const SYSTEMD_RESTART_ARGS: [&str; 3] = ["--user", "restart", "zeroclaw.service"];
pub(crate) const OPENRC_STATUS_ARGS: [&str; 2] = ["zeroclaw", "status"];
pub(crate) const OPENRC_RESTART_ARGS: [&str; 2] = ["zeroclaw", "restart"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelRouteSelection {
    pub(crate) provider: String,
    pub(crate) model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChannelRuntimeCommand {
    ShowProviders,
    SetProvider(String),
    ShowModel,
    SetModel(String),
    NewSession,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ModelCacheState {
    pub(crate) entries: Vec<ModelCacheEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ModelCacheEntry {
    pub(crate) provider: String,
    pub(crate) models: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelRuntimeDefaults {
    pub(crate) default_provider: String,
    pub(crate) model: String,
    pub(crate) temperature: f64,
    pub(crate) api_key: Option<String>,
    pub(crate) api_url: Option<String>,
    pub(crate) reliability: crate::config::ReliabilityConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigFileStamp {
    pub(crate) modified: SystemTime,
    pub(crate) len: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfigState {
    pub(crate) defaults: ChannelRuntimeDefaults,
    pub(crate) last_applied_stamp: Option<ConfigFileStamp>,
}

#[derive(Clone)]
pub(crate) struct ChannelRuntimeContext {
    pub(crate) channels_by_name: Arc<HashMap<String, Arc<dyn Channel>>>,
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) default_provider: Arc<String>,
    pub(crate) memory: Arc<dyn Memory>,
    pub(crate) tools_registry: Arc<Vec<Box<dyn Tool>>>,
    pub(crate) observer: Arc<dyn Observer>,
    pub(crate) system_prompt: Arc<String>,
    pub(crate) model: Arc<String>,
    pub(crate) temperature: f64,
    pub(crate) auto_save_memory: bool,
    pub(crate) max_tool_iterations: usize,
    pub(crate) max_executor_action_iterations: usize,
    pub(crate) min_relevance_score: f64,
    pub(crate) conversation_histories: ConversationHistoryMap,
    pub(crate) provider_cache: ProviderCacheMap,
    pub(crate) route_overrides: RouteSelectionMap,
    pub(crate) api_key: Option<String>,
    pub(crate) api_url: Option<String>,
    pub(crate) reliability: Arc<crate::config::ReliabilityConfig>,
    pub(crate) provider_runtime_options: providers::ProviderRuntimeOptions,
    pub(crate) workspace_dir: Arc<PathBuf>,
    pub(crate) message_timeout_secs: u64,
    pub(crate) interrupt_on_new_message: bool,
    pub(crate) multimodal: crate::config::MultimodalConfig,
    pub(crate) hooks: Option<Arc<crate::hooks::HookRunner>>,
    pub(crate) non_cli_excluded_tools: Arc<Vec<String>>,
    pub(crate) triage_model: Option<String>,
    pub(crate) planner_model: Option<String>,
    pub(crate) classification_config: crate::config::QueryClassificationConfig,
    pub(crate) integration_tool_names: std::collections::HashMap<String, Vec<String>>,
    pub(crate) integration_catalog: String,
    pub(crate) classifier_model: Option<String>,
}

#[derive(Clone)]
pub(crate) struct InFlightSenderTaskState {
    pub(crate) task_id: u64,
    pub(crate) cancellation: CancellationToken,
    pub(crate) completion: Arc<InFlightTaskCompletion>,
}

pub(crate) struct InFlightTaskCompletion {
    pub(crate) done: AtomicBool,
    pub(crate) notify: tokio::sync::Notify,
}

impl InFlightTaskCompletion {
    pub(crate) fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn mark_done(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(crate) async fn wait(&self) {
        if self.done.load(Ordering::Acquire) {
            return;
        }
        self.notify.notified().await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelHealthState {
    Healthy,
    Unhealthy,
    Timeout,
}

pub(crate) struct ConfiguredChannel {
    pub(crate) display_name: &'static str,
    pub(crate) channel: Arc<dyn Channel>,
}
