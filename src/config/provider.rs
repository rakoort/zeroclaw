use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ── Provider Profiles ────────────────────────────────────────────

/// Named provider profile definition compatible with Codex app-server style config.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ModelProviderConfig {
    /// Optional provider type/name override (e.g. "openai", "openai-codex", or custom profile id).
    #[serde(default)]
    pub name: Option<String>,
    /// Optional base URL for OpenAI-compatible endpoints.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Provider protocol variant ("responses" or "chat_completions").
    #[serde(default)]
    pub wire_api: Option<String>,
    /// If true, load OpenAI auth material (OPENAI_API_KEY or ~/.codex/auth.json).
    #[serde(default)]
    pub requires_openai_auth: bool,
}

// ── Delegate Agents ──────────────────────────────────────────────

/// Configuration for a delegate sub-agent used by the `delegate` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DelegateAgentConfig {
    /// Provider name (e.g. "ollama", "openrouter", "anthropic")
    pub provider: String,
    /// Model name
    pub model: String,
    /// Optional system prompt for the sub-agent
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional API key override
    #[serde(default)]
    pub api_key: Option<String>,
    /// Temperature override
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Max recursion depth for nested delegation
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Enable agentic sub-agent mode (multi-turn tool-call loop).
    #[serde(default)]
    pub agentic: bool,
    /// Allowlist of tool names available to the sub-agent in agentic mode.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Maximum tool-call iterations in agentic mode.
    #[serde(default = "default_max_tool_iterations")]
    pub max_iterations: usize,
}

fn default_max_depth() -> u32 {
    3
}

fn default_max_tool_iterations() -> usize {
    10
}

// ── Transcription ────────────────────────────────────────────────

fn default_transcription_api_url() -> String {
    "https://api.groq.com/openai/v1/audio/transcriptions".into()
}

fn default_transcription_model() -> String {
    "whisper-large-v3-turbo".into()
}

fn default_transcription_max_duration_secs() -> u64 {
    120
}

/// Voice transcription configuration (Whisper API via Groq).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptionConfig {
    /// Enable voice transcription for channels that support it.
    #[serde(default)]
    pub enabled: bool,
    /// Whisper API endpoint URL.
    #[serde(default = "default_transcription_api_url")]
    pub api_url: String,
    /// Whisper model name.
    #[serde(default = "default_transcription_model")]
    pub model: String,
    /// Optional language hint (ISO-639-1, e.g. "en", "ru").
    #[serde(default)]
    pub language: Option<String>,
    /// Maximum voice duration in seconds (messages longer than this are skipped).
    #[serde(default = "default_transcription_max_duration_secs")]
    pub max_duration_secs: u64,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_url: default_transcription_api_url(),
            model: default_transcription_model(),
            language: None,
            max_duration_secs: default_transcription_max_duration_secs(),
        }
    }
}

// ── Reliability / supervision ────────────────────────────────────

/// A cross-provider fallback entry: try this model on this provider when the primary fails.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProviderFallbackEntry {
    /// Provider name (must match a key in provider config, e.g. "openrouter").
    pub provider: String,
    /// Model identifier for this provider.
    pub model: String,
}

/// Reliability and supervision configuration (`[reliability]` section).
///
/// Controls provider retries, fallback chains, API key rotation, and channel restart backoff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReliabilityConfig {
    /// Retries per provider before failing over.
    #[serde(default = "default_provider_retries")]
    pub provider_retries: u32,
    /// Base backoff (ms) for provider retry delay.
    #[serde(default = "default_provider_backoff_ms")]
    pub provider_backoff_ms: u64,
    /// Fallback provider chain (e.g. `["anthropic", "openai"]`).
    #[serde(default)]
    pub fallback_providers: Vec<String>,
    /// Additional API keys for round-robin rotation on rate-limit (429) errors.
    /// The primary `api_key` is always tried first; these are extras.
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Per-model fallback chains. When a model fails, try these alternatives in order.
    /// Example: `{ "claude-opus-4-20250514" = ["claude-sonnet-4-20250514", "gpt-4o"] }`
    #[serde(default)]
    pub model_fallbacks: std::collections::HashMap<String, Vec<String>>,
    /// Cross-provider fallback chains: when retries exhaust on the primary provider,
    /// try these (provider, model) pairs in order.
    /// Key = primary model name, Value = list of fallback entries.
    #[serde(default)]
    pub provider_fallbacks: std::collections::HashMap<String, Vec<ProviderFallbackEntry>>,
    /// Initial backoff for channel/daemon restarts.
    #[serde(default = "default_channel_backoff_secs")]
    pub channel_initial_backoff_secs: u64,
    /// Max backoff for channel/daemon restarts.
    #[serde(default = "default_channel_backoff_max_secs")]
    pub channel_max_backoff_secs: u64,
    /// Scheduler polling cadence in seconds.
    #[serde(default = "default_scheduler_poll_secs")]
    pub scheduler_poll_secs: u64,
    /// Max retries for cron job execution attempts.
    #[serde(default = "default_scheduler_retries")]
    pub scheduler_retries: u32,
}

fn default_provider_retries() -> u32 {
    2
}

fn default_provider_backoff_ms() -> u64 {
    500
}

fn default_channel_backoff_secs() -> u64 {
    2
}

fn default_channel_backoff_max_secs() -> u64 {
    60
}

fn default_scheduler_poll_secs() -> u64 {
    15
}

fn default_scheduler_retries() -> u32 {
    2
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            provider_retries: default_provider_retries(),
            provider_backoff_ms: default_provider_backoff_ms(),
            fallback_providers: Vec::new(),
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            provider_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: default_channel_backoff_secs(),
            channel_max_backoff_secs: default_channel_backoff_max_secs(),
            scheduler_poll_secs: default_scheduler_poll_secs(),
            scheduler_retries: default_scheduler_retries(),
        }
    }
}

// ── Model routing ────────────────────────────────────────────────

/// Route a task hint to a specific provider + model.
///
/// ```toml
/// [[model_routes]]
/// hint = "reasoning"
/// provider = "openrouter"
/// model = "anthropic/claude-opus-4-20250514"
///
/// [[model_routes]]
/// hint = "fast"
/// provider = "groq"
/// model = "llama-3.3-70b-versatile"
/// ```
///
/// Usage: pass `hint:reasoning` as the model parameter to route the request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModelRouteConfig {
    /// Task hint name (e.g. "reasoning", "fast", "code", "summarize")
    pub hint: String,
    /// Provider to route to (must match a known provider name)
    pub provider: String,
    /// Model to use with that provider
    pub model: String,
    /// Optional API key override for this route's provider
    #[serde(default)]
    pub api_key: Option<String>,
    /// Ordered fallback models tried when primary fails.
    #[serde(default)]
    pub fallbacks: Vec<FallbackModelConfig>,
    /// Context window size in tokens. Used for filtering.
    #[serde(default)]
    pub context_window: Option<usize>,
}

/// A fallback model entry within a route.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FallbackModelConfig {
    /// Provider to use for this fallback.
    pub provider: String,
    /// Model name for this fallback.
    pub model: String,
    /// Context window size in tokens.
    #[serde(default)]
    pub context_window: Option<usize>,
}

// ── Embedding routing ───────────────────────────────────────────

/// Route an embedding hint to a specific provider + model.
///
/// ```toml
/// [[embedding_routes]]
/// hint = "semantic"
/// provider = "openai"
/// model = "text-embedding-3-small"
/// dimensions = 1536
///
/// [memory]
/// embedding_model = "hint:semantic"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EmbeddingRouteConfig {
    /// Route hint name (e.g. "semantic", "archive", "faq")
    pub hint: String,
    /// Embedding provider (`none`, `openai`, or `custom:<url>`)
    pub provider: String,
    /// Embedding model to use with that provider
    pub model: String,
    /// Optional embedding dimension override for this route
    #[serde(default)]
    pub dimensions: Option<usize>,
    /// Optional API key override for this route's provider
    #[serde(default)]
    pub api_key: Option<String>,
}

// ── Query Classification ─────────────────────────────────────────

/// Classification mode: rule-based (default, backward-compatible) or weighted scoring.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClassificationMode {
    #[default]
    Rules,
    Weighted,
}

/// Tier-to-hint mapping for weighted classification.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClassificationTiers {
    #[serde(default)]
    pub simple: Option<String>,
    #[serde(default)]
    pub medium: Option<String>,
    #[serde(default)]
    pub complex: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Dimension weights for weighted classification scoring.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClassificationWeights {
    #[serde(default = "default_weight_length")]
    pub length: f64,
    #[serde(default = "default_weight_code_density")]
    pub code_density: f64,
    #[serde(default = "default_weight_question_complexity")]
    pub question_complexity: f64,
    #[serde(default = "default_weight_conversation_depth")]
    pub conversation_depth: f64,
    #[serde(default = "default_weight_tool_hint")]
    pub tool_hint: f64,
    #[serde(default = "default_weight_domain_specificity")]
    pub domain_specificity: f64,
}

fn default_weight_length() -> f64 {
    0.20
}
fn default_weight_code_density() -> f64 {
    0.25
}
fn default_weight_question_complexity() -> f64 {
    0.20
}
fn default_weight_conversation_depth() -> f64 {
    0.10
}
fn default_weight_tool_hint() -> f64 {
    0.10
}
fn default_weight_domain_specificity() -> f64 {
    0.15
}

impl Default for ClassificationWeights {
    fn default() -> Self {
        Self {
            length: default_weight_length(),
            code_density: default_weight_code_density(),
            question_complexity: default_weight_question_complexity(),
            conversation_depth: default_weight_conversation_depth(),
            tool_hint: default_weight_tool_hint(),
            domain_specificity: default_weight_domain_specificity(),
        }
    }
}

// ── 14-Dimension Scoring ─────────────────────────────────────────

/// Complexity tier for query classification scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Simple,
    #[default]
    Medium,
    Complex,
    Reasoning,
}

/// Score boundaries between adjacent tiers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TierBoundaries {
    #[serde(default = "default_tier_simple_medium")]
    pub simple_medium: f64,
    #[serde(default = "default_tier_medium_complex")]
    pub medium_complex: f64,
    #[serde(default = "default_tier_complex_reasoning")]
    pub complex_reasoning: f64,
}

fn default_tier_simple_medium() -> f64 {
    0.0
}
fn default_tier_medium_complex() -> f64 {
    0.3
}
fn default_tier_complex_reasoning() -> f64 {
    0.5
}

impl Default for TierBoundaries {
    fn default() -> Self {
        Self {
            simple_medium: default_tier_simple_medium(),
            medium_complex: default_tier_medium_complex(),
            complex_reasoning: default_tier_complex_reasoning(),
        }
    }
}

/// Hard-coded overrides that bypass scoring.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoringOverrides {
    /// Force complex tier when token count exceeds this. Default: `100_000`.
    #[serde(default = "default_max_tokens_force_complex")]
    pub max_tokens_force_complex: usize,
    /// Minimum tier for structured-output requests. Default: `medium`.
    #[serde(default)]
    pub structured_output_min_tier: Tier,
    /// Default tier when classification is ambiguous. Default: `medium`.
    #[serde(default)]
    pub ambiguous_default_tier: Tier,
}

fn default_max_tokens_force_complex() -> usize {
    100_000
}

impl Default for ScoringOverrides {
    fn default() -> Self {
        Self {
            max_tokens_force_complex: default_max_tokens_force_complex(),
            structured_output_min_tier: Tier::default(),
            ambiguous_default_tier: Tier::default(),
        }
    }
}

/// 14-dimension weights for scoring-based classification. Sum must equal 1.0.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DimensionWeights {
    #[serde(default = "default_dw_token_count")]
    pub token_count: f64,
    #[serde(default = "default_dw_code_presence")]
    pub code_presence: f64,
    #[serde(default = "default_dw_reasoning_markers")]
    pub reasoning_markers: f64,
    #[serde(default = "default_dw_technical_terms")]
    pub technical_terms: f64,
    #[serde(default = "default_dw_creative_markers")]
    pub creative_markers: f64,
    #[serde(default = "default_dw_simple_indicators")]
    pub simple_indicators: f64,
    #[serde(default = "default_dw_multi_step_patterns")]
    pub multi_step_patterns: f64,
    #[serde(default = "default_dw_question_complexity")]
    pub question_complexity: f64,
    #[serde(default = "default_dw_imperative_verbs")]
    pub imperative_verbs: f64,
    #[serde(default = "default_dw_constraint_count")]
    pub constraint_count: f64,
    #[serde(default = "default_dw_output_format")]
    pub output_format: f64,
    #[serde(default = "default_dw_reference_complexity")]
    pub reference_complexity: f64,
    #[serde(default = "default_dw_negation_complexity")]
    pub negation_complexity: f64,
    #[serde(default = "default_dw_domain_specificity")]
    pub domain_specificity: f64,
}

fn default_dw_token_count() -> f64 {
    0.08
}
fn default_dw_code_presence() -> f64 {
    0.15
}
fn default_dw_reasoning_markers() -> f64 {
    0.22
}
fn default_dw_technical_terms() -> f64 {
    0.10
}
fn default_dw_creative_markers() -> f64 {
    0.05
}
fn default_dw_simple_indicators() -> f64 {
    0.02
}
fn default_dw_multi_step_patterns() -> f64 {
    0.18
}
fn default_dw_question_complexity() -> f64 {
    0.05
}
fn default_dw_imperative_verbs() -> f64 {
    0.03
}
fn default_dw_constraint_count() -> f64 {
    0.04
}
fn default_dw_output_format() -> f64 {
    0.03
}
fn default_dw_reference_complexity() -> f64 {
    0.02
}
fn default_dw_negation_complexity() -> f64 {
    0.01
}
fn default_dw_domain_specificity() -> f64 {
    0.02
}

impl Default for DimensionWeights {
    fn default() -> Self {
        Self {
            token_count: default_dw_token_count(),
            code_presence: default_dw_code_presence(),
            reasoning_markers: default_dw_reasoning_markers(),
            technical_terms: default_dw_technical_terms(),
            creative_markers: default_dw_creative_markers(),
            simple_indicators: default_dw_simple_indicators(),
            multi_step_patterns: default_dw_multi_step_patterns(),
            question_complexity: default_dw_question_complexity(),
            imperative_verbs: default_dw_imperative_verbs(),
            constraint_count: default_dw_constraint_count(),
            output_format: default_dw_output_format(),
            reference_complexity: default_dw_reference_complexity(),
            negation_complexity: default_dw_negation_complexity(),
            domain_specificity: default_dw_domain_specificity(),
        }
    }
}

/// Top-level scoring configuration for 14-dimension classification.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoringConfig {
    #[serde(default)]
    pub dimension_weights: DimensionWeights,
    #[serde(default)]
    pub tier_boundaries: TierBoundaries,
    #[serde(default)]
    pub overrides: ScoringOverrides,
    /// Steepness of the sigmoid confidence curve. Default: `12.0`.
    #[serde(default = "default_confidence_steepness")]
    pub confidence_steepness: f64,
    /// Confidence threshold below which the result is treated as uncertain. Default: `0.7`.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
}

fn default_confidence_steepness() -> f64 {
    12.0
}
fn default_confidence_threshold() -> f64 {
    0.7
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            dimension_weights: DimensionWeights::default(),
            tier_boundaries: TierBoundaries::default(),
            overrides: ScoringOverrides::default(),
            confidence_steepness: default_confidence_steepness(),
            confidence_threshold: default_confidence_threshold(),
        }
    }
}

/// Automatic query classification — classifies user messages by keyword/pattern
/// and routes to the appropriate model hint. Disabled by default.
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct QueryClassificationConfig {
    /// Enable automatic query classification. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Classification mode: "rules" (default) or "weighted".
    #[serde(default)]
    pub mode: ClassificationMode,
    /// Classification rules evaluated in priority order.
    #[serde(default)]
    pub rules: Vec<ClassificationRule>,
    /// Tier-to-hint mapping (only used in weighted mode).
    #[serde(default)]
    pub tiers: ClassificationTiers,
    /// Dimension weights (only used in weighted mode).
    #[serde(default)]
    pub weights: ClassificationWeights,
    /// 14-dimension scoring configuration.
    #[serde(default)]
    pub scoring: ScoringConfig,
}

/// A single classification rule mapping message patterns to a model hint.
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct ClassificationRule {
    /// Must match a `[[model_routes]]` hint value.
    pub hint: String,
    /// Case-insensitive substring matches.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Case-sensitive literal matches (for "```", "fn ", etc.).
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Only match if message length >= N chars.
    #[serde(default)]
    pub min_length: Option<usize>,
    /// Only match if message length <= N chars.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Higher priority rules are checked first.
    #[serde(default)]
    pub priority: i32,
}
