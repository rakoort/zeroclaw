use crate::config::Config;
use crate::providers::{
    canonical_china_provider_name, is_glm_alias, is_glm_cn_alias, is_minimax_alias,
    is_moonshot_alias, is_qianfan_alias, is_qwen_alias, is_qwen_oauth_alias, is_zai_alias,
    is_zai_cn_alias,
};
use anyhow::{bail, Context, Result};
use console::style;
use dialoguer::{Confirm, Input, Select};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

use super::wizard::print_bullet;

// ── Constants ────────────────────────────────────────────────────

const LIVE_MODEL_MAX_OPTIONS: usize = 120;
const MODEL_PREVIEW_LIMIT: usize = 20;
const MODEL_CACHE_FILE: &str = "models_cache.json";
pub(crate) const MODEL_CACHE_TTL_SECS: u64 = 12 * 60 * 60;
const CUSTOM_MODEL_SENTINEL: &str = "__custom_model__";

// ── Provider name normalization ──────────────────────────────────

pub(crate) fn canonical_provider_name(provider_name: &str) -> &str {
    if is_qwen_oauth_alias(provider_name) {
        return "qwen-code";
    }

    if let Some(canonical) = canonical_china_provider_name(provider_name) {
        return canonical;
    }

    match provider_name {
        "grok" => "xai",
        "together" => "together-ai",
        "google" | "google-gemini" => "gemini",
        "github-copilot" => "copilot",
        "openai_codex" | "codex" => "openai-codex",
        "kimi_coding" | "kimi_for_coding" => "kimi-code",
        "nvidia-nim" | "build.nvidia.com" => "nvidia",
        "aws-bedrock" => "bedrock",
        "llama.cpp" => "llamacpp",
        _ => provider_name,
    }
}

pub(crate) fn allows_unauthenticated_model_fetch(provider_name: &str) -> bool {
    matches!(
        canonical_provider_name(provider_name),
        "openrouter"
            | "ollama"
            | "llamacpp"
            | "sglang"
            | "vllm"
            | "osaurus"
            | "venice"
            | "astrai"
            | "nvidia"
    )
}

// ── Default models ───────────────────────────────────────────────

/// Pick a sensible default model for the given provider.
const MINIMAX_ONBOARD_MODELS: [(&str, &str); 5] = [
    ("MiniMax-M2.5", "MiniMax M2.5 (latest, recommended)"),
    ("MiniMax-M2.5-highspeed", "MiniMax M2.5 High-Speed (faster)"),
    ("MiniMax-M2.1", "MiniMax M2.1 (stable)"),
    ("MiniMax-M2.1-highspeed", "MiniMax M2.1 High-Speed (faster)"),
    ("MiniMax-M2", "MiniMax M2 (legacy)"),
];

pub(crate) fn default_model_for_provider(provider: &str) -> String {
    match canonical_provider_name(provider) {
        "anthropic" => "claude-sonnet-4-5-20250929".into(),
        "openai" => "gpt-5.2".into(),
        "openai-codex" => "gpt-5-codex".into(),
        "venice" => "zai-org-glm-5".into(),
        "groq" => "llama-3.3-70b-versatile".into(),
        "mistral" => "mistral-large-latest".into(),
        "deepseek" => "deepseek-chat".into(),
        "xai" => "grok-4-1-fast-reasoning".into(),
        "perplexity" => "sonar-pro".into(),
        "fireworks" => "accounts/fireworks/models/llama-v3p3-70b-instruct".into(),
        "novita" => "minimax/minimax-m2.5".into(),
        "together-ai" => "meta-llama/Llama-3.3-70B-Instruct-Turbo".into(),
        "cohere" => "command-a-03-2025".into(),
        "moonshot" => "kimi-k2.5".into(),
        "glm" | "zai" => "glm-5".into(),
        "minimax" => "MiniMax-M2.5".into(),
        "qwen" => "qwen-plus".into(),
        "qwen-code" => "qwen3-coder-plus".into(),
        "ollama" => "llama3.2".into(),
        "llamacpp" => "ggml-org/gpt-oss-20b-GGUF".into(),
        "sglang" | "vllm" | "osaurus" => "default".into(),
        "gemini" => "gemini-2.5-pro".into(),
        "kimi-code" => "kimi-for-coding".into(),
        "bedrock" => "anthropic.claude-sonnet-4-5-20250929-v1:0".into(),
        "nvidia" => "meta/llama-3.3-70b-instruct".into(),
        _ => "anthropic/claude-sonnet-4.6".into(),
    }
}

pub(crate) fn apply_provider_update(
    config: &mut Config,
    provider: String,
    api_key: String,
    model: String,
    provider_api_url: Option<String>,
) {
    config.default_provider = Some(provider);
    config.default_model = Some(model);
    config.api_url = provider_api_url;
    config.api_key = if api_key.trim().is_empty() {
        None
    } else {
        Some(api_key)
    };
}

// ── Curated model catalogs ───────────────────────────────────────

pub(crate) fn curated_models_for_provider(provider_name: &str) -> Vec<(String, String)> {
    match canonical_provider_name(provider_name) {
        "openrouter" => vec![
            (
                "anthropic/claude-sonnet-4.6".to_string(),
                "Claude Sonnet 4.6 (balanced, recommended)".to_string(),
            ),
            (
                "openai/gpt-5.2".to_string(),
                "GPT-5.2 (latest flagship)".to_string(),
            ),
            (
                "openai/gpt-5-mini".to_string(),
                "GPT-5 mini (fast, cost-efficient)".to_string(),
            ),
            (
                "google/gemini-3-pro-preview".to_string(),
                "Gemini 3 Pro Preview (frontier reasoning)".to_string(),
            ),
            (
                "x-ai/grok-4.1-fast".to_string(),
                "Grok 4.1 Fast (reasoning + speed)".to_string(),
            ),
            (
                "deepseek/deepseek-v3.2".to_string(),
                "DeepSeek V3.2 (agentic + affordable)".to_string(),
            ),
            (
                "meta-llama/llama-4-maverick".to_string(),
                "Llama 4 Maverick (open model)".to_string(),
            ),
        ],
        "anthropic" => vec![
            (
                "claude-sonnet-4-5-20250929".to_string(),
                "Claude Sonnet 4.5 (balanced, recommended)".to_string(),
            ),
            (
                "claude-opus-4-6".to_string(),
                "Claude Opus 4.6 (best quality)".to_string(),
            ),
            (
                "claude-haiku-4-5-20251001".to_string(),
                "Claude Haiku 4.5 (fastest, cheapest)".to_string(),
            ),
        ],
        "openai" => vec![
            (
                "gpt-5.2".to_string(),
                "GPT-5.2 (latest coding/agentic flagship)".to_string(),
            ),
            (
                "gpt-5-mini".to_string(),
                "GPT-5 mini (faster, cheaper)".to_string(),
            ),
            (
                "gpt-5-nano".to_string(),
                "GPT-5 nano (lowest latency/cost)".to_string(),
            ),
            (
                "gpt-5.2-codex".to_string(),
                "GPT-5.2 Codex (agentic coding)".to_string(),
            ),
        ],
        "openai-codex" => vec![
            (
                "gpt-5-codex".to_string(),
                "GPT-5 Codex (recommended)".to_string(),
            ),
            (
                "gpt-5.2-codex".to_string(),
                "GPT-5.2 Codex (agentic coding)".to_string(),
            ),
            ("o4-mini".to_string(), "o4-mini (fallback)".to_string()),
        ],
        "venice" => vec![
            (
                "zai-org-glm-5".to_string(),
                "GLM-5 via Venice (agentic flagship)".to_string(),
            ),
            (
                "claude-sonnet-4-6".to_string(),
                "Claude Sonnet 4.6 via Venice (best quality)".to_string(),
            ),
            (
                "deepseek-v3.2".to_string(),
                "DeepSeek V3.2 via Venice (strong value)".to_string(),
            ),
            (
                "grok-41-fast".to_string(),
                "Grok 4.1 Fast via Venice (low latency)".to_string(),
            ),
        ],
        "groq" => vec![
            (
                "llama-3.3-70b-versatile".to_string(),
                "Llama 3.3 70B (fast, recommended)".to_string(),
            ),
            (
                "openai/gpt-oss-120b".to_string(),
                "GPT-OSS 120B (strong open-weight)".to_string(),
            ),
            (
                "openai/gpt-oss-20b".to_string(),
                "GPT-OSS 20B (cost-efficient open-weight)".to_string(),
            ),
        ],
        "mistral" => vec![
            (
                "mistral-large-latest".to_string(),
                "Mistral Large (latest flagship)".to_string(),
            ),
            (
                "mistral-medium-latest".to_string(),
                "Mistral Medium (balanced)".to_string(),
            ),
            (
                "codestral-latest".to_string(),
                "Codestral (code-focused)".to_string(),
            ),
            (
                "devstral-latest".to_string(),
                "Devstral (software engineering specialist)".to_string(),
            ),
        ],
        "deepseek" => vec![
            (
                "deepseek-chat".to_string(),
                "DeepSeek Chat (mapped to V3.2 non-thinking)".to_string(),
            ),
            (
                "deepseek-reasoner".to_string(),
                "DeepSeek Reasoner (mapped to V3.2 thinking)".to_string(),
            ),
        ],
        "xai" => vec![
            (
                "grok-4-1-fast-reasoning".to_string(),
                "Grok 4.1 Fast Reasoning (recommended)".to_string(),
            ),
            (
                "grok-4-1-fast-non-reasoning".to_string(),
                "Grok 4.1 Fast Non-Reasoning (low latency)".to_string(),
            ),
            (
                "grok-code-fast-1".to_string(),
                "Grok Code Fast 1 (coding specialist)".to_string(),
            ),
            ("grok-4".to_string(), "Grok 4 (max quality)".to_string()),
        ],
        "perplexity" => vec![
            (
                "sonar-pro".to_string(),
                "Sonar Pro (flagship web-grounded model)".to_string(),
            ),
            (
                "sonar-reasoning-pro".to_string(),
                "Sonar Reasoning Pro (complex multi-step reasoning)".to_string(),
            ),
            (
                "sonar-deep-research".to_string(),
                "Sonar Deep Research (long-form research)".to_string(),
            ),
            ("sonar".to_string(), "Sonar (search, fast)".to_string()),
        ],
        "fireworks" => vec![
            (
                "accounts/fireworks/models/llama-v3p3-70b-instruct".to_string(),
                "Llama 3.3 70B".to_string(),
            ),
            (
                "accounts/fireworks/models/mixtral-8x22b-instruct".to_string(),
                "Mixtral 8x22B".to_string(),
            ),
        ],
        "novita" => vec![(
            "minimax/minimax-m2.5".to_string(),
            "MiniMax M2.5".to_string(),
        )],
        "together-ai" => vec![
            (
                "meta-llama/Llama-3.3-70B-Instruct-Turbo".to_string(),
                "Llama 3.3 70B Instruct Turbo (recommended)".to_string(),
            ),
            (
                "moonshotai/Kimi-K2.5".to_string(),
                "Kimi K2.5 (reasoning + coding)".to_string(),
            ),
            (
                "deepseek-ai/DeepSeek-V3.1".to_string(),
                "DeepSeek V3.1 (strong value)".to_string(),
            ),
        ],
        "cohere" => vec![
            (
                "command-a-03-2025".to_string(),
                "Command A (flagship enterprise model)".to_string(),
            ),
            (
                "command-a-reasoning-08-2025".to_string(),
                "Command A Reasoning (agentic reasoning)".to_string(),
            ),
            (
                "command-r-08-2024".to_string(),
                "Command R (stable fast baseline)".to_string(),
            ),
        ],
        "kimi-code" => vec![
            (
                "kimi-for-coding".to_string(),
                "Kimi for Coding (official coding-agent model)".to_string(),
            ),
            (
                "kimi-k2.5".to_string(),
                "Kimi K2.5 (general coding endpoint model)".to_string(),
            ),
        ],
        "moonshot" => vec![
            (
                "kimi-k2.5".to_string(),
                "Kimi K2.5 (latest flagship, recommended)".to_string(),
            ),
            (
                "kimi-k2-thinking".to_string(),
                "Kimi K2 Thinking (deep reasoning + tool use)".to_string(),
            ),
            (
                "kimi-k2-0905-preview".to_string(),
                "Kimi K2 0905 Preview (strong coding)".to_string(),
            ),
        ],
        "glm" | "zai" => vec![
            ("glm-5".to_string(), "GLM-5 (high reasoning)".to_string()),
            (
                "glm-4.7".to_string(),
                "GLM-4.7 (strong general-purpose quality)".to_string(),
            ),
            (
                "glm-4.5-air".to_string(),
                "GLM-4.5 Air (lower latency)".to_string(),
            ),
        ],
        "minimax" => vec![
            (
                "MiniMax-M2.5".to_string(),
                "MiniMax M2.5 (latest flagship)".to_string(),
            ),
            (
                "MiniMax-M2.5-highspeed".to_string(),
                "MiniMax M2.5 High-Speed (fast)".to_string(),
            ),
            (
                "MiniMax-M2.1".to_string(),
                "MiniMax M2.1 (strong coding/reasoning)".to_string(),
            ),
        ],
        "qwen" => vec![
            (
                "qwen-max".to_string(),
                "Qwen Max (highest quality)".to_string(),
            ),
            (
                "qwen-plus".to_string(),
                "Qwen Plus (balanced default)".to_string(),
            ),
            (
                "qwen-turbo".to_string(),
                "Qwen Turbo (fast and cost-efficient)".to_string(),
            ),
        ],
        "qwen-code" => vec![
            (
                "qwen3-coder-plus".to_string(),
                "Qwen3 Coder Plus (recommended for coding workflows)".to_string(),
            ),
            (
                "qwen3.5-plus".to_string(),
                "Qwen3.5 Plus (reasoning + coding)".to_string(),
            ),
            (
                "qwen3-max-2026-01-23".to_string(),
                "Qwen3 Max (high-capability coding model)".to_string(),
            ),
        ],
        "nvidia" => vec![
            (
                "meta/llama-3.3-70b-instruct".to_string(),
                "Llama 3.3 70B Instruct (balanced default)".to_string(),
            ),
            (
                "deepseek-ai/deepseek-v3.2".to_string(),
                "DeepSeek V3.2 (advanced reasoning + coding)".to_string(),
            ),
            (
                "nvidia/llama-3.3-nemotron-super-49b-v1.5".to_string(),
                "Llama 3.3 Nemotron Super 49B v1.5 (NVIDIA-tuned)".to_string(),
            ),
            (
                "nvidia/llama-3.1-nemotron-ultra-253b-v1".to_string(),
                "Llama 3.1 Nemotron Ultra 253B v1 (max quality)".to_string(),
            ),
        ],
        "astrai" => vec![
            (
                "anthropic/claude-sonnet-4.6".to_string(),
                "Claude Sonnet 4.6 (balanced default)".to_string(),
            ),
            (
                "openai/gpt-5.2".to_string(),
                "GPT-5.2 (latest flagship)".to_string(),
            ),
            (
                "deepseek/deepseek-v3.2".to_string(),
                "DeepSeek V3.2 (agentic + affordable)".to_string(),
            ),
            (
                "z-ai/glm-5".to_string(),
                "GLM-5 (high reasoning)".to_string(),
            ),
        ],
        "ollama" => vec![
            (
                "llama3.2".to_string(),
                "Llama 3.2 (recommended local)".to_string(),
            ),
            ("mistral".to_string(), "Mistral 7B".to_string()),
            ("codellama".to_string(), "Code Llama".to_string()),
            ("phi3".to_string(), "Phi-3 (small, fast)".to_string()),
        ],
        "llamacpp" => vec![
            (
                "ggml-org/gpt-oss-20b-GGUF".to_string(),
                "GPT-OSS 20B GGUF (llama.cpp server example)".to_string(),
            ),
            (
                "bartowski/Llama-3.3-70B-Instruct-GGUF".to_string(),
                "Llama 3.3 70B GGUF (high quality)".to_string(),
            ),
            (
                "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF".to_string(),
                "Qwen2.5 Coder 7B GGUF (coding-focused)".to_string(),
            ),
        ],
        "sglang" | "vllm" => vec![
            (
                "meta-llama/Llama-3.1-8B-Instruct".to_string(),
                "Llama 3.1 8B Instruct (popular, fast)".to_string(),
            ),
            (
                "meta-llama/Llama-3.1-70B-Instruct".to_string(),
                "Llama 3.1 70B Instruct (high quality)".to_string(),
            ),
            (
                "Qwen/Qwen2.5-Coder-7B-Instruct".to_string(),
                "Qwen2.5 Coder 7B Instruct (coding-focused)".to_string(),
            ),
        ],
        "osaurus" => vec![
            (
                "qwen3-30b-a3b-8bit".to_string(),
                "Qwen3 30B A3B (local, balanced)".to_string(),
            ),
            (
                "gemma-3n-e4b-it-lm-4bit".to_string(),
                "Gemma 3N E4B (local, efficient)".to_string(),
            ),
            (
                "phi-4-mini-reasoning-mlx-4bit".to_string(),
                "Phi-4 Mini Reasoning (local, fast reasoning)".to_string(),
            ),
        ],
        "bedrock" => vec![
            (
                "anthropic.claude-sonnet-4-6".to_string(),
                "Claude Sonnet 4.6 (latest, recommended)".to_string(),
            ),
            (
                "anthropic.claude-opus-4-6-v1".to_string(),
                "Claude Opus 4.6 (strongest)".to_string(),
            ),
            (
                "anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
                "Claude Haiku 4.5 (fastest, cheapest)".to_string(),
            ),
            (
                "anthropic.claude-sonnet-4-5-20250929-v1:0".to_string(),
                "Claude Sonnet 4.5".to_string(),
            ),
        ],
        "gemini" => vec![
            (
                "gemini-3-pro-preview".to_string(),
                "Gemini 3 Pro Preview (latest frontier reasoning)".to_string(),
            ),
            (
                "gemini-2.5-pro".to_string(),
                "Gemini 2.5 Pro (stable reasoning)".to_string(),
            ),
            (
                "gemini-2.5-flash".to_string(),
                "Gemini 2.5 Flash (best price/performance)".to_string(),
            ),
            (
                "gemini-2.5-flash-lite".to_string(),
                "Gemini 2.5 Flash-Lite (lowest cost)".to_string(),
            ),
        ],
        _ => vec![("default".to_string(), "Default model".to_string())],
    }
}

// ── Live model discovery ─────────────────────────────────────────

pub(crate) fn supports_live_model_fetch(provider_name: &str) -> bool {
    if provider_name.trim().starts_with("custom:") {
        return true;
    }

    matches!(
        canonical_provider_name(provider_name),
        "openrouter"
            | "openai-codex"
            | "openai"
            | "anthropic"
            | "groq"
            | "mistral"
            | "deepseek"
            | "xai"
            | "together-ai"
            | "gemini"
            | "ollama"
            | "llamacpp"
            | "sglang"
            | "vllm"
            | "osaurus"
            | "astrai"
            | "venice"
            | "fireworks"
            | "novita"
            | "cohere"
            | "moonshot"
            | "glm"
            | "zai"
            | "qwen"
            | "nvidia"
    )
}

pub(crate) fn models_endpoint_for_provider(provider_name: &str) -> Option<&'static str> {
    match provider_name {
        "qwen-intl" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models"),
        "dashscope-us" => Some("https://dashscope-us.aliyuncs.com/compatible-mode/v1/models"),
        "moonshot-cn" | "kimi-cn" => Some("https://api.moonshot.cn/v1/models"),
        "glm-cn" | "bigmodel" => Some("https://open.bigmodel.cn/api/paas/v4/models"),
        "zai-cn" | "z.ai-cn" => Some("https://open.bigmodel.cn/api/coding/paas/v4/models"),
        _ => match canonical_provider_name(provider_name) {
            "openai-codex" | "openai" => Some("https://api.openai.com/v1/models"),
            "venice" => Some("https://api.venice.ai/api/v1/models"),
            "groq" => Some("https://api.groq.com/openai/v1/models"),
            "mistral" => Some("https://api.mistral.ai/v1/models"),
            "deepseek" => Some("https://api.deepseek.com/v1/models"),
            "xai" => Some("https://api.x.ai/v1/models"),
            "together-ai" => Some("https://api.together.xyz/v1/models"),
            "fireworks" => Some("https://api.fireworks.ai/inference/v1/models"),
            "novita" => Some("https://api.novita.ai/openai/v1/models"),
            "cohere" => Some("https://api.cohere.com/compatibility/v1/models"),
            "moonshot" => Some("https://api.moonshot.ai/v1/models"),
            "glm" => Some("https://api.z.ai/api/paas/v4/models"),
            "zai" => Some("https://api.z.ai/api/coding/paas/v4/models"),
            "qwen" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1/models"),
            "nvidia" => Some("https://integrate.api.nvidia.com/v1/models"),
            "astrai" => Some("https://as-trai.com/v1/models"),
            "llamacpp" => Some("http://localhost:8080/v1/models"),
            "sglang" => Some("http://localhost:30000/v1/models"),
            "vllm" => Some("http://localhost:8000/v1/models"),
            "osaurus" => Some("http://localhost:1337/v1/models"),
            _ => None,
        },
    }
}

fn build_model_fetch_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(4))
        .build()
        .context("failed to build model-fetch HTTP client")
}

fn normalize_model_ids(ids: Vec<String>) -> Vec<String> {
    let mut unique = BTreeMap::new();
    for id in ids {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            unique
                .entry(trimmed.to_ascii_lowercase())
                .or_insert_with(|| trimmed.to_string());
        }
    }
    unique.into_values().collect()
}

fn parse_openai_compatible_model_ids(payload: &Value) -> Vec<String> {
    let mut models = Vec::new();

    if let Some(data) = payload.get("data").and_then(Value::as_array) {
        for model in data {
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                models.push(id.to_string());
            }
        }
    } else if let Some(data) = payload.as_array() {
        for model in data {
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                models.push(id.to_string());
            }
        }
    }

    normalize_model_ids(models)
}

fn parse_gemini_model_ids(payload: &Value) -> Vec<String> {
    let Some(models) = payload.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut ids = Vec::new();
    for model in models {
        let supports_generate_content = model
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .is_none_or(|methods| {
                methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
            });

        if !supports_generate_content {
            continue;
        }

        if let Some(name) = model.get("name").and_then(Value::as_str) {
            ids.push(name.trim_start_matches("models/").to_string());
        }
    }

    normalize_model_ids(ids)
}

fn parse_ollama_model_ids(payload: &Value) -> Vec<String> {
    let Some(models) = payload.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut ids = Vec::new();
    for model in models {
        if let Some(name) = model.get("name").and_then(Value::as_str) {
            ids.push(name.to_string());
        }
    }

    normalize_model_ids(ids)
}

fn fetch_openai_compatible_models(
    endpoint: &str,
    api_key: Option<&str>,
    allow_unauthenticated: bool,
) -> Result<Vec<String>> {
    let client = build_model_fetch_client()?;
    let mut request = client.get(endpoint);

    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    } else if !allow_unauthenticated {
        bail!("model fetch requires API key for endpoint {endpoint}");
    }

    let payload: Value = request
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .with_context(|| format!("model fetch failed: GET {endpoint}"))?
        .json()
        .context("failed to parse model list response")?;

    Ok(parse_openai_compatible_model_ids(&payload))
}

fn fetch_openrouter_models(api_key: Option<&str>) -> Result<Vec<String>> {
    let client = build_model_fetch_client()?;
    let mut request = client.get("https://openrouter.ai/api/v1/models");
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }

    let payload: Value = request
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .context("model fetch failed: GET https://openrouter.ai/api/v1/models")?
        .json()
        .context("failed to parse OpenRouter model list response")?;

    Ok(parse_openai_compatible_model_ids(&payload))
}

fn fetch_anthropic_models(api_key: Option<&str>) -> Result<Vec<String>> {
    let Some(api_key) = api_key else {
        bail!("Anthropic model fetch requires API key or OAuth token");
    };

    let client = build_model_fetch_client()?;
    let mut request = client
        .get("https://api.anthropic.com/v1/models")
        .header("anthropic-version", "2023-06-01");

    if api_key.starts_with("sk-ant-oat01-") {
        request = request
            .header("Authorization", format!("Bearer {api_key}"))
            .header("anthropic-beta", "oauth-2025-04-20");
    } else {
        request = request.header("x-api-key", api_key);
    }

    let response = request
        .send()
        .context("model fetch failed: GET https://api.anthropic.com/v1/models")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!("Anthropic model list request failed (HTTP {status}): {body}");
    }

    let payload: Value = response
        .json()
        .context("failed to parse Anthropic model list response")?;

    Ok(parse_openai_compatible_model_ids(&payload))
}

fn fetch_gemini_models(api_key: Option<&str>) -> Result<Vec<String>> {
    let Some(api_key) = api_key else {
        bail!("Gemini model fetch requires API key");
    };

    let client = build_model_fetch_client()?;
    let payload: Value = client
        .get("https://generativelanguage.googleapis.com/v1beta/models")
        .query(&[("key", api_key), ("pageSize", "200")])
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .context("model fetch failed: GET Gemini models")?
        .json()
        .context("failed to parse Gemini model list response")?;

    Ok(parse_gemini_model_ids(&payload))
}

fn fetch_ollama_models() -> Result<Vec<String>> {
    let client = build_model_fetch_client()?;
    let payload: Value = client
        .get("http://localhost:11434/api/tags")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .context("model fetch failed: GET http://localhost:11434/api/tags")?
        .json()
        .context("failed to parse Ollama model list response")?;

    Ok(parse_ollama_model_ids(&payload))
}

fn normalize_ollama_endpoint_url(raw_url: &str) -> String {
    let trimmed = raw_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .strip_suffix("/api")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

fn ollama_endpoint_is_local(endpoint_url: &str) -> bool {
    reqwest::Url::parse(endpoint_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0"))
}

fn ollama_uses_remote_endpoint(provider_api_url: Option<&str>) -> bool {
    let Some(endpoint) = provider_api_url else {
        return false;
    };

    let normalized = normalize_ollama_endpoint_url(endpoint);
    if normalized.is_empty() {
        return false;
    }

    !ollama_endpoint_is_local(&normalized)
}

fn resolve_live_models_endpoint(
    provider_name: &str,
    provider_api_url: Option<&str>,
) -> Option<String> {
    if let Some(raw_base) = provider_name.strip_prefix("custom:") {
        let normalized = raw_base.trim().trim_end_matches('/');
        if normalized.is_empty() {
            return None;
        }
        if normalized.ends_with("/models") {
            return Some(normalized.to_string());
        }
        return Some(format!("{normalized}/models"));
    }

    if matches!(
        canonical_provider_name(provider_name),
        "llamacpp" | "sglang" | "vllm" | "osaurus"
    ) {
        if let Some(url) = provider_api_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            let normalized = url.trim_end_matches('/');
            if normalized.ends_with("/models") {
                return Some(normalized.to_string());
            }
            return Some(format!("{normalized}/models"));
        }
    }

    if canonical_provider_name(provider_name) == "openai-codex" {
        if let Some(url) = provider_api_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            let normalized = url.trim_end_matches('/');
            if normalized.ends_with("/models") {
                return Some(normalized.to_string());
            }
            return Some(format!("{normalized}/models"));
        }
    }

    models_endpoint_for_provider(provider_name).map(str::to_string)
}

fn fetch_live_models_for_provider(
    provider_name: &str,
    api_key: &str,
    provider_api_url: Option<&str>,
) -> Result<Vec<String>> {
    let requested_provider_name = provider_name;
    let provider_name = canonical_provider_name(provider_name);
    let ollama_remote = provider_name == "ollama" && ollama_uses_remote_endpoint(provider_api_url);
    let api_key = if api_key.trim().is_empty() {
        if provider_name == "ollama" && !ollama_remote {
            None
        } else {
            std::env::var(provider_env_var(provider_name))
                .ok()
                .or_else(|| {
                    // Anthropic also accepts OAuth setup-tokens via ANTHROPIC_OAUTH_TOKEN
                    if provider_name == "anthropic" {
                        std::env::var("ANTHROPIC_OAUTH_TOKEN").ok()
                    } else if provider_name == "minimax" {
                        std::env::var("MINIMAX_OAUTH_TOKEN").ok()
                    } else {
                        None
                    }
                })
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }
    } else {
        Some(api_key.trim().to_string())
    };

    let models = match provider_name {
        "openrouter" => fetch_openrouter_models(api_key.as_deref())?,
        "anthropic" => fetch_anthropic_models(api_key.as_deref())?,
        "gemini" => fetch_gemini_models(api_key.as_deref())?,
        "ollama" => {
            if ollama_remote {
                // Remote Ollama endpoints can serve cloud-routed models.
                // Keep this curated list aligned with current Ollama cloud catalog.
                vec![
                    "glm-5:cloud".to_string(),
                    "glm-4.7:cloud".to_string(),
                    "gpt-oss:20b:cloud".to_string(),
                    "gpt-oss:120b:cloud".to_string(),
                    "gemini-3-flash-preview:cloud".to_string(),
                    "qwen3-coder-next:cloud".to_string(),
                    "qwen3-coder:480b:cloud".to_string(),
                    "kimi-k2.5:cloud".to_string(),
                    "minimax-m2.5:cloud".to_string(),
                    "deepseek-v3.1:671b:cloud".to_string(),
                ]
            } else {
                // Local endpoints should not surface cloud-only suffixes.
                fetch_ollama_models()?
                    .into_iter()
                    .filter(|model_id| !model_id.ends_with(":cloud"))
                    .collect()
            }
        }
        _ => {
            if let Some(endpoint) =
                resolve_live_models_endpoint(requested_provider_name, provider_api_url)
            {
                let allow_unauthenticated =
                    allows_unauthenticated_model_fetch(requested_provider_name);
                fetch_openai_compatible_models(
                    &endpoint,
                    api_key.as_deref(),
                    allow_unauthenticated,
                )?
            } else {
                Vec::new()
            }
        }
    };

    Ok(models)
}

// ── Model cache types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelCacheEntry {
    provider: String,
    fetched_at_unix: u64,
    models: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelCacheState {
    entries: Vec<ModelCacheEntry>,
}

#[derive(Debug, Clone)]
struct CachedModels {
    models: Vec<String>,
    age_secs: u64,
}

fn model_cache_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join(MODEL_CACHE_FILE)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

async fn load_model_cache_state(workspace_dir: &Path) -> Result<ModelCacheState> {
    let path = model_cache_path(workspace_dir);
    if !path.exists() {
        return Ok(ModelCacheState::default());
    }

    let raw = fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read model cache at {}", path.display()))?;

    match serde_json::from_str::<ModelCacheState>(&raw) {
        Ok(state) => Ok(state),
        Err(_) => Ok(ModelCacheState::default()),
    }
}

async fn save_model_cache_state(workspace_dir: &Path, state: &ModelCacheState) -> Result<()> {
    let path = model_cache_path(workspace_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to create model cache directory {}",
                parent.display()
            )
        })?;
    }

    let json = serde_json::to_vec_pretty(state).context("failed to serialize model cache")?;
    fs::write(&path, json)
        .await
        .with_context(|| format!("failed to write model cache at {}", path.display()))?;

    Ok(())
}

async fn cache_live_models_for_provider(
    workspace_dir: &Path,
    provider_name: &str,
    models: &[String],
) -> Result<()> {
    let normalized_models = normalize_model_ids(models.to_vec());
    if normalized_models.is_empty() {
        return Ok(());
    }

    let mut state = load_model_cache_state(workspace_dir).await?;
    let now = now_unix_secs();

    if let Some(entry) = state
        .entries
        .iter_mut()
        .find(|entry| entry.provider == provider_name)
    {
        entry.fetched_at_unix = now;
        entry.models = normalized_models;
    } else {
        state.entries.push(ModelCacheEntry {
            provider: provider_name.to_string(),
            fetched_at_unix: now,
            models: normalized_models,
        });
    }

    save_model_cache_state(workspace_dir, &state).await
}

async fn load_cached_models_for_provider_internal(
    workspace_dir: &Path,
    provider_name: &str,
    ttl_secs: Option<u64>,
) -> Result<Option<CachedModels>> {
    let state = load_model_cache_state(workspace_dir).await?;
    let now = now_unix_secs();

    let Some(entry) = state
        .entries
        .into_iter()
        .find(|entry| entry.provider == provider_name)
    else {
        return Ok(None);
    };

    if entry.models.is_empty() {
        return Ok(None);
    }

    let age_secs = now.saturating_sub(entry.fetched_at_unix);
    if ttl_secs.is_some_and(|ttl| age_secs > ttl) {
        return Ok(None);
    }

    Ok(Some(CachedModels {
        models: entry.models,
        age_secs,
    }))
}

async fn load_cached_models_for_provider(
    workspace_dir: &Path,
    provider_name: &str,
    ttl_secs: u64,
) -> Result<Option<CachedModels>> {
    load_cached_models_for_provider_internal(workspace_dir, provider_name, Some(ttl_secs)).await
}

async fn load_any_cached_models_for_provider(
    workspace_dir: &Path,
    provider_name: &str,
) -> Result<Option<CachedModels>> {
    load_cached_models_for_provider_internal(workspace_dir, provider_name, None).await
}

fn humanize_age(age_secs: u64) -> String {
    if age_secs < 60 {
        format!("{age_secs}s")
    } else if age_secs < 60 * 60 {
        format!("{}m", age_secs / 60)
    } else {
        format!("{}h", age_secs / (60 * 60))
    }
}

fn build_model_options(model_ids: Vec<String>, source: &str) -> Vec<(String, String)> {
    model_ids
        .into_iter()
        .map(|model_id| {
            let label = format!("{model_id} ({source})");
            (model_id, label)
        })
        .collect()
}

fn print_model_preview(models: &[String]) {
    for model in models.iter().take(MODEL_PREVIEW_LIMIT) {
        println!("  {} {model}", style("-"));
    }

    if models.len() > MODEL_PREVIEW_LIMIT {
        println!(
            "  {} ... and {} more",
            style("-"),
            models.len() - MODEL_PREVIEW_LIMIT
        );
    }
}

// ── Public model commands ────────────────────────────────────────

pub async fn run_models_refresh(
    config: &Config,
    provider_override: Option<&str>,
    force: bool,
) -> Result<()> {
    let provider_name = provider_override
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter")
        .trim()
        .to_string();

    if provider_name.is_empty() {
        anyhow::bail!("Provider name cannot be empty");
    }

    if !supports_live_model_fetch(&provider_name) {
        anyhow::bail!("Provider '{provider_name}' does not support live model discovery yet");
    }

    if !force {
        if let Some(cached) = load_cached_models_for_provider(
            &config.workspace_dir,
            &provider_name,
            MODEL_CACHE_TTL_SECS,
        )
        .await?
        {
            println!(
                "Using cached model list for '{}' (updated {} ago):",
                provider_name,
                humanize_age(cached.age_secs)
            );
            print_model_preview(&cached.models);
            println!();
            println!(
                "Tip: run `zeroclaw models refresh --force --provider {}` to fetch latest now.",
                provider_name
            );
            return Ok(());
        }
    }

    let api_key = config.api_key.clone().unwrap_or_default();

    match fetch_live_models_for_provider(&provider_name, &api_key, config.api_url.as_deref()) {
        Ok(models) if !models.is_empty() => {
            cache_live_models_for_provider(&config.workspace_dir, &provider_name, &models).await?;
            println!(
                "Refreshed '{}' model cache with {} models.",
                provider_name,
                models.len()
            );
            print_model_preview(&models);
            Ok(())
        }
        Ok(_) => {
            if let Some(stale_cache) =
                load_any_cached_models_for_provider(&config.workspace_dir, &provider_name).await?
            {
                println!(
                    "Provider returned no models; using stale cache (updated {} ago):",
                    humanize_age(stale_cache.age_secs)
                );
                print_model_preview(&stale_cache.models);
                return Ok(());
            }

            anyhow::bail!("Provider '{}' returned an empty model list", provider_name)
        }
        Err(error) => {
            if let Some(stale_cache) =
                load_any_cached_models_for_provider(&config.workspace_dir, &provider_name).await?
            {
                println!(
                    "Live refresh failed ({}). Falling back to stale cache (updated {} ago):",
                    error,
                    humanize_age(stale_cache.age_secs)
                );
                print_model_preview(&stale_cache.models);
                return Ok(());
            }

            Err(error)
                .with_context(|| format!("failed to refresh models for provider '{provider_name}'"))
        }
    }
}

pub async fn run_models_list(config: &Config, provider_override: Option<&str>) -> Result<()> {
    let provider_name = provider_override
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter");

    let cached = load_any_cached_models_for_provider(&config.workspace_dir, provider_name).await?;

    let Some(cached) = cached else {
        println!();
        println!(
            "  No cached models for '{provider_name}'. Run: zeroclaw models refresh --provider {provider_name}"
        );
        println!();
        return Ok(());
    };

    println!();
    println!(
        "  {} models for '{}' (cached {} ago):",
        cached.models.len(),
        provider_name,
        humanize_age(cached.age_secs)
    );
    println!();
    for model in &cached.models {
        let marker = if config.default_model.as_deref() == Some(model.as_str()) {
            "* "
        } else {
            "  "
        };
        println!("  {marker}{model}");
    }
    println!();
    Ok(())
}

pub async fn run_models_set(config: &Config, model: &str) -> Result<()> {
    let model = model.trim();
    if model.is_empty() {
        anyhow::bail!("Model name cannot be empty");
    }

    let mut updated = config.clone();
    updated.default_model = Some(model.to_string());
    updated.save().await?;

    println!();
    println!("  Default model set to '{}'.", style(model).green().bold());
    println!();
    Ok(())
}

pub async fn run_models_status(config: &Config) -> Result<()> {
    let provider = config.default_provider.as_deref().unwrap_or("openrouter");
    let model = config.default_model.as_deref().unwrap_or("(not set)");

    println!();
    println!("  Provider:  {}", style(provider).cyan());
    println!("  Model:     {}", style(model).cyan());
    println!(
        "  Temp:      {}",
        style(format!("{:.1}", config.default_temperature)).cyan()
    );

    match load_any_cached_models_for_provider(&config.workspace_dir, provider).await? {
        Some(cached) => {
            println!(
                "  Cache:     {} models (updated {} ago)",
                cached.models.len(),
                humanize_age(cached.age_secs)
            );
            let fresh = cached.age_secs < MODEL_CACHE_TTL_SECS;
            if fresh {
                println!("  Freshness: {}", style("fresh").green());
            } else {
                println!("  Freshness: {}", style("stale").yellow());
            }
        }
        None => {
            println!("  Cache:     {}", style("none").yellow());
        }
    }

    println!();
    Ok(())
}

pub async fn cached_model_catalog_stats(
    config: &Config,
    provider_name: &str,
) -> Result<Option<(usize, u64)>> {
    let Some(cached) =
        load_any_cached_models_for_provider(&config.workspace_dir, provider_name).await?
    else {
        return Ok(None);
    };
    Ok(Some((cached.models.len(), cached.age_secs)))
}

pub async fn run_models_refresh_all(config: &Config, force: bool) -> Result<()> {
    let mut targets: Vec<String> = crate::providers::list_providers()
        .into_iter()
        .map(|provider| provider.name.to_string())
        .filter(|name| supports_live_model_fetch(name))
        .collect();

    targets.sort();
    targets.dedup();

    if targets.is_empty() {
        anyhow::bail!("No providers support live model discovery");
    }

    println!(
        "Refreshing model catalogs for {} providers (force: {})",
        targets.len(),
        if force { "yes" } else { "no" }
    );
    println!();

    let mut ok_count = 0usize;
    let mut fail_count = 0usize;

    for provider_name in &targets {
        println!("== {} ==", provider_name);
        match run_models_refresh(config, Some(provider_name), force).await {
            Ok(()) => {
                ok_count += 1;
            }
            Err(error) => {
                fail_count += 1;
                println!("  failed: {error}");
            }
        }
        println!();
    }

    println!("Summary: {} succeeded, {} failed", ok_count, fail_count);

    if ok_count == 0 {
        anyhow::bail!("Model refresh failed for all providers")
    }
    Ok(())
}

// ── Provider helpers ─────────────────────────────────────────────

pub(crate) fn local_provider_choices() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ollama", "Ollama — local models (Llama, Mistral, Phi)"),
        (
            "llamacpp",
            "llama.cpp server — local OpenAI-compatible endpoint",
        ),
        (
            "sglang",
            "SGLang — high-performance local serving framework",
        ),
        ("vllm", "vLLM — high-performance local inference engine"),
        (
            "osaurus",
            "Osaurus — unified AI edge runtime (local MLX + cloud proxy + MCP)",
        ),
    ]
}

/// Map provider name to its conventional env var
pub(crate) fn provider_env_var(name: &str) -> &'static str {
    if canonical_provider_name(name) == "qwen-code" {
        return "QWEN_OAUTH_TOKEN";
    }

    match canonical_provider_name(name) {
        "openrouter" => "OPENROUTER_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai-codex" | "openai" => "OPENAI_API_KEY",
        "ollama" => "OLLAMA_API_KEY",
        "llamacpp" => "LLAMACPP_API_KEY",
        "sglang" => "SGLANG_API_KEY",
        "vllm" => "VLLM_API_KEY",
        "osaurus" => "OSAURUS_API_KEY",
        "venice" => "VENICE_API_KEY",
        "groq" => "GROQ_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "xai" => "XAI_API_KEY",
        "together-ai" => "TOGETHER_API_KEY",
        "fireworks" | "fireworks-ai" => "FIREWORKS_API_KEY",
        "novita" => "NOVITA_API_KEY",
        "perplexity" => "PERPLEXITY_API_KEY",
        "cohere" => "COHERE_API_KEY",
        "kimi-code" => "KIMI_CODE_API_KEY",
        "moonshot" => "MOONSHOT_API_KEY",
        "glm" => "GLM_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "qwen" => "DASHSCOPE_API_KEY",
        "qianfan" => "QIANFAN_API_KEY",
        "zai" => "ZAI_API_KEY",
        "synthetic" => "SYNTHETIC_API_KEY",
        "opencode" | "opencode-zen" => "OPENCODE_API_KEY",
        "vercel" | "vercel-ai" => "VERCEL_API_KEY",
        "cloudflare" | "cloudflare-ai" => "CLOUDFLARE_API_KEY",
        "bedrock" | "aws-bedrock" => "AWS_ACCESS_KEY_ID",
        "gemini" => "GEMINI_API_KEY",
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => "NVIDIA_API_KEY",
        "astrai" => "ASTRAI_API_KEY",
        _ => "API_KEY",
    }
}

pub(crate) fn provider_supports_keyless_local_usage(provider_name: &str) -> bool {
    matches!(
        canonical_provider_name(provider_name),
        "ollama" | "llamacpp" | "sglang" | "vllm" | "osaurus"
    )
}

pub(crate) fn provider_supports_device_flow(provider_name: &str) -> bool {
    matches!(
        canonical_provider_name(provider_name),
        "copilot" | "gemini" | "openai-codex"
    )
}

// ── Interactive provider setup ───────────────────────────────────

#[allow(clippy::too_many_lines)]
pub(crate) async fn setup_provider(
    workspace_dir: &Path,
) -> Result<(String, String, String, Option<String>)> {
    // ── Tier selection ──
    let tiers = vec![
        "⭐ Recommended (OpenRouter, Venice, Anthropic, OpenAI, Gemini)",
        "⚡ Fast inference (Groq, Fireworks, Together AI, NVIDIA NIM)",
        "🌐 Gateway / proxy (Vercel AI, Cloudflare AI, Amazon Bedrock)",
        "🔬 Specialized (Moonshot/Kimi, GLM/Zhipu, MiniMax, Qwen/DashScope, Qianfan, Z.AI, Synthetic, OpenCode Zen, Cohere)",
        "🏠 Local / private (Ollama, llama.cpp server, vLLM — no API key needed)",
        "🔧 Custom — bring your own OpenAI-compatible API",
    ];

    let tier_idx = Select::new()
        .with_prompt("  Select provider category")
        .items(&tiers)
        .default(0)
        .interact()?;

    let providers: Vec<(&str, &str)> = match tier_idx {
        0 => vec![
            (
                "openrouter",
                "OpenRouter — 200+ models, 1 API key (recommended)",
            ),
            ("venice", "Venice AI — privacy-first (Llama, Opus)"),
            ("anthropic", "Anthropic — Claude Sonnet & Opus (direct)"),
            ("openai", "OpenAI — GPT-4o, o1, GPT-5 (direct)"),
            (
                "openai-codex",
                "OpenAI Codex (ChatGPT subscription OAuth, no API key)",
            ),
            ("deepseek", "DeepSeek — V3 & R1 (affordable)"),
            ("mistral", "Mistral — Large & Codestral"),
            ("xai", "xAI — Grok 3 & 4"),
            ("perplexity", "Perplexity — search-augmented AI"),
            (
                "gemini",
                "Google Gemini — Gemini 2.0 Flash & Pro (supports CLI auth)",
            ),
        ],
        1 => vec![
            ("groq", "Groq — ultra-fast LPU inference"),
            ("fireworks", "Fireworks AI — fast open-source inference"),
            ("novita", "Novita AI — affordable open-source inference"),
            ("together-ai", "Together AI — open-source model hosting"),
            ("nvidia", "NVIDIA NIM — DeepSeek, Llama, & more"),
        ],
        2 => vec![
            ("vercel", "Vercel AI Gateway"),
            ("cloudflare", "Cloudflare AI Gateway"),
            (
                "astrai",
                "Astrai — compliant AI routing (PII stripping, cost optimization)",
            ),
            ("bedrock", "Amazon Bedrock — AWS managed models"),
        ],
        3 => vec![
            (
                "kimi-code",
                "Kimi Code — coding-optimized Kimi API (KimiCLI)",
            ),
            (
                "qwen-code",
                "Qwen Code — OAuth tokens reused from ~/.qwen/oauth_creds.json",
            ),
            ("moonshot", "Moonshot — Kimi API (China endpoint)"),
            (
                "moonshot-intl",
                "Moonshot — Kimi API (international endpoint)",
            ),
            ("glm", "GLM — ChatGLM / Zhipu (international endpoint)"),
            ("glm-cn", "GLM — ChatGLM / Zhipu (China endpoint)"),
            (
                "minimax",
                "MiniMax — international endpoint (api.minimax.io)",
            ),
            ("minimax-cn", "MiniMax — China endpoint (api.minimaxi.com)"),
            ("qwen", "Qwen — DashScope China endpoint"),
            ("qwen-intl", "Qwen — DashScope international endpoint"),
            ("qwen-us", "Qwen — DashScope US endpoint"),
            ("qianfan", "Qianfan — Baidu AI models (China endpoint)"),
            ("zai", "Z.AI — global coding endpoint"),
            ("zai-cn", "Z.AI — China coding endpoint (open.bigmodel.cn)"),
            ("synthetic", "Synthetic — Synthetic AI models"),
            ("opencode", "OpenCode Zen — code-focused AI"),
            ("cohere", "Cohere — Command R+ & embeddings"),
        ],
        4 => local_provider_choices(),
        _ => vec![], // Custom — handled below
    };

    // ── Custom / BYOP flow ──
    if providers.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("Custom Provider Setup").white().bold(),
            style("— any OpenAI-compatible API").dim()
        );
        print_bullet("ZeroClaw works with ANY API that speaks the OpenAI chat completions format.");
        print_bullet("Examples: LiteLLM, LocalAI, vLLM, text-generation-webui, LM Studio, etc.");
        println!();

        let base_url: String = Input::new()
            .with_prompt("  API base URL (e.g. http://localhost:1234 or https://my-api.com)")
            .interact_text()?;

        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            anyhow::bail!("Custom provider requires a base URL.");
        }

        let api_key: String = Input::new()
            .with_prompt("  API key (or Enter to skip if not needed)")
            .allow_empty(true)
            .interact_text()?;

        let model: String = Input::new()
            .with_prompt("  Model name (e.g. llama3, gpt-4o, mistral)")
            .default("default".into())
            .interact_text()?;

        let provider_name = format!("custom:{base_url}");

        println!(
            "  {} Provider: {} | Model: {}",
            style("✓").green().bold(),
            style(&provider_name).green(),
            style(&model).green()
        );

        return Ok((provider_name, api_key, model, None));
    }

    let provider_labels: Vec<&str> = providers.iter().map(|(_, label)| *label).collect();

    let provider_idx = Select::new()
        .with_prompt("  Select your AI provider")
        .items(&provider_labels)
        .default(0)
        .interact()?;

    let provider_name = providers[provider_idx].0;

    // ── API key / endpoint ──
    let mut provider_api_url: Option<String> = None;
    let api_key = if provider_name == "ollama" {
        let use_remote_ollama = Confirm::new()
            .with_prompt("  Use a remote Ollama endpoint (for example Ollama Cloud)?")
            .default(false)
            .interact()?;

        if use_remote_ollama {
            let raw_url: String = Input::new()
                .with_prompt("  Remote Ollama endpoint URL")
                .default("https://ollama.com".into())
                .interact_text()?;

            let normalized_url = normalize_ollama_endpoint_url(&raw_url);
            if normalized_url.is_empty() {
                anyhow::bail!("Remote Ollama endpoint URL cannot be empty.");
            }
            let parsed = reqwest::Url::parse(&normalized_url)
                .context("Remote Ollama endpoint URL must be a valid URL")?;
            if !matches!(parsed.scheme(), "http" | "https") {
                anyhow::bail!("Remote Ollama endpoint URL must use http:// or https://");
            }

            provider_api_url = Some(normalized_url.clone());

            print_bullet(&format!(
                "Remote endpoint configured: {}",
                style(&normalized_url).cyan()
            ));
            if raw_url.trim().trim_end_matches('/') != normalized_url {
                print_bullet("Normalized endpoint to base URL (removed trailing /api).");
            }
            print_bullet(&format!(
                "If you use cloud-only models, append {} to the model ID.",
                style(":cloud").yellow()
            ));

            let key: String = Input::new()
                .with_prompt("  API key for remote Ollama endpoint (or Enter to skip)")
                .allow_empty(true)
                .interact_text()?;

            if key.trim().is_empty() {
                print_bullet(&format!(
                    "No API key provided. Set {} later if required by your endpoint.",
                    style("OLLAMA_API_KEY").yellow()
                ));
            }

            key
        } else {
            print_bullet("Using local Ollama at http://localhost:11434 (no API key needed).");
            String::new()
        }
    } else if matches!(provider_name, "llamacpp" | "llama.cpp") {
        let raw_url: String = Input::new()
            .with_prompt("  llama.cpp server endpoint URL")
            .default("http://localhost:8080/v1".into())
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("llama.cpp endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using llama.cpp server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your llama.cpp server is started with --api-key.");

        let key: String = Input::new()
            .with_prompt("  API key for llama.cpp server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("LLAMACPP_API_KEY").yellow()
            ));
        }

        key
    } else if provider_name == "sglang" {
        let raw_url: String = Input::new()
            .with_prompt("  SGLang server endpoint URL")
            .default("http://localhost:30000/v1".into())
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("SGLang endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using SGLang server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your SGLang server requires authentication.");

        let key: String = Input::new()
            .with_prompt("  API key for SGLang server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("SGLANG_API_KEY").yellow()
            ));
        }

        key
    } else if provider_name == "vllm" {
        let raw_url: String = Input::new()
            .with_prompt("  vLLM server endpoint URL")
            .default("http://localhost:8000/v1".into())
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("vLLM endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using vLLM server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your vLLM server requires authentication.");

        let key: String = Input::new()
            .with_prompt("  API key for vLLM server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("VLLM_API_KEY").yellow()
            ));
        }

        key
    } else if provider_name == "osaurus" {
        let raw_url: String = Input::new()
            .with_prompt("  Osaurus server endpoint URL")
            .default("http://localhost:1337/v1".into())
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("Osaurus endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using Osaurus server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your Osaurus server requires authentication.");

        let key: String = Input::new()
            .with_prompt("  API key for Osaurus server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("OSAURUS_API_KEY").yellow()
            ));
        }

        key
    } else if canonical_provider_name(provider_name) == "gemini" {
        // Special handling for Gemini: check for CLI auth first
        if crate::providers::gemini::GeminiProvider::has_cli_credentials() {
            print_bullet(&format!(
                "{} Gemini CLI credentials detected! You can skip the API key.",
                style("✓").green().bold()
            ));
            print_bullet("ZeroClaw will reuse your existing Gemini CLI authentication.");
            println!();

            let use_cli: bool = dialoguer::Confirm::new()
                .with_prompt("  Use existing Gemini CLI authentication?")
                .default(true)
                .interact()?;

            if use_cli {
                println!(
                    "  {} Using Gemini CLI OAuth tokens",
                    style("✓").green().bold()
                );
                String::new() // Empty key = will use CLI tokens
            } else {
                print_bullet("Get your API key at: https://aistudio.google.com/app/apikey");
                Input::new()
                    .with_prompt("  Paste your Gemini API key")
                    .allow_empty(true)
                    .interact_text()?
            }
        } else if std::env::var("GEMINI_API_KEY").is_ok() {
            print_bullet(&format!(
                "{} GEMINI_API_KEY environment variable detected!",
                style("✓").green().bold()
            ));
            String::new()
        } else {
            print_bullet("Get your API key at: https://aistudio.google.com/app/apikey");
            print_bullet("Or run `gemini` CLI to authenticate (tokens will be reused).");
            println!();

            Input::new()
                .with_prompt("  Paste your Gemini API key (or press Enter to skip)")
                .allow_empty(true)
                .interact_text()?
        }
    } else if canonical_provider_name(provider_name) == "anthropic" {
        if std::env::var("ANTHROPIC_OAUTH_TOKEN").is_ok() {
            print_bullet(&format!(
                "{} ANTHROPIC_OAUTH_TOKEN environment variable detected!",
                style("✓").green().bold()
            ));
            String::new()
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            print_bullet(&format!(
                "{} ANTHROPIC_API_KEY environment variable detected!",
                style("✓").green().bold()
            ));
            String::new()
        } else {
            print_bullet(&format!(
                "Get your API key at: {}",
                style("https://console.anthropic.com/settings/keys")
                    .cyan()
                    .underlined()
            ));
            print_bullet("Or run `claude setup-token` to get an OAuth setup-token.");
            println!();

            let key: String = Input::new()
                .with_prompt("  Paste your API key or setup-token (or press Enter to skip)")
                .allow_empty(true)
                .interact_text()?;

            if key.is_empty() {
                print_bullet(&format!(
                    "Skipped. Set {} or {} or edit config.toml later.",
                    style("ANTHROPIC_API_KEY").yellow(),
                    style("ANTHROPIC_OAUTH_TOKEN").yellow()
                ));
            }

            key
        }
    } else if canonical_provider_name(provider_name) == "qwen-code" {
        if std::env::var("QWEN_OAUTH_TOKEN").is_ok() {
            print_bullet(&format!(
                "{} QWEN_OAUTH_TOKEN environment variable detected!",
                style("✓").green().bold()
            ));
            "qwen-oauth".to_string()
        } else {
            print_bullet(
                "Qwen Code OAuth credentials are usually stored in ~/.qwen/oauth_creds.json.",
            );
            print_bullet(
                "Run `qwen` once and complete OAuth login to populate cached credentials.",
            );
            print_bullet("You can also set QWEN_OAUTH_TOKEN directly.");
            println!();

            let key: String = Input::new()
                .with_prompt(
                    "  Paste your Qwen OAuth token (or press Enter to auto-detect cached OAuth)",
                )
                .allow_empty(true)
                .interact_text()?;

            if key.trim().is_empty() {
                print_bullet(&format!(
                    "Using OAuth auto-detection. Set {} and optional {} if needed.",
                    style("QWEN_OAUTH_TOKEN").yellow(),
                    style("QWEN_OAUTH_RESOURCE_URL").yellow()
                ));
                "qwen-oauth".to_string()
            } else {
                key
            }
        }
    } else {
        let key_url = if is_moonshot_alias(provider_name)
            || canonical_provider_name(provider_name) == "kimi-code"
        {
            "https://platform.moonshot.cn/console/api-keys"
        } else if canonical_provider_name(provider_name) == "qwen-code" {
            "https://qwen.readthedocs.io/en/latest/getting_started/installation.html"
        } else if is_glm_cn_alias(provider_name) || is_zai_cn_alias(provider_name) {
            "https://open.bigmodel.cn/usercenter/proj-mgmt/apikeys"
        } else if is_glm_alias(provider_name) || is_zai_alias(provider_name) {
            "https://platform.z.ai/"
        } else if is_minimax_alias(provider_name) {
            "https://www.minimaxi.com/user-center/basic-information"
        } else if is_qwen_alias(provider_name) {
            "https://help.aliyun.com/zh/model-studio/developer-reference/get-api-key"
        } else if is_qianfan_alias(provider_name) {
            "https://cloud.baidu.com/doc/WENXINWORKSHOP/s/7lm0vxo78"
        } else {
            match provider_name {
                "openrouter" => "https://openrouter.ai/keys",
                "openai" => "https://platform.openai.com/api-keys",
                "venice" => "https://venice.ai/settings/api",
                "groq" => "https://console.groq.com/keys",
                "mistral" => "https://console.mistral.ai/api-keys",
                "deepseek" => "https://platform.deepseek.com/api_keys",
                "together-ai" => "https://api.together.xyz/settings/api-keys",
                "fireworks" => "https://fireworks.ai/account/api-keys",
                "novita" => "https://novita.ai/settings/key-management",
                "perplexity" => "https://www.perplexity.ai/settings/api",
                "xai" => "https://console.x.ai",
                "cohere" => "https://dashboard.cohere.com/api-keys",
                "vercel" => "https://vercel.com/account/tokens",
                "cloudflare" => "https://dash.cloudflare.com/profile/api-tokens",
                "nvidia" | "nvidia-nim" | "build.nvidia.com" => "https://build.nvidia.com/",
                "bedrock" => "https://console.aws.amazon.com/iam",
                "gemini" => "https://aistudio.google.com/app/apikey",
                "astrai" => "https://as-trai.com",
                _ => "",
            }
        };

        println!();
        if matches!(provider_name, "bedrock" | "aws-bedrock") {
            // Bedrock uses AWS AKSK, not a single API key.
            print_bullet("Bedrock uses AWS credentials (not a single API key).");
            print_bullet(&format!(
                "Set {} and {} environment variables.",
                style("AWS_ACCESS_KEY_ID").yellow(),
                style("AWS_SECRET_ACCESS_KEY").yellow(),
            ));
            print_bullet(&format!(
                "Optionally set {} for the region (default: us-east-1).",
                style("AWS_REGION").yellow(),
            ));
            if !key_url.is_empty() {
                print_bullet(&format!(
                    "Manage IAM credentials at: {}",
                    style(key_url).cyan().underlined()
                ));
            }
            println!();
            String::new()
        } else {
            if !key_url.is_empty() {
                print_bullet(&format!(
                    "Get your API key at: {}",
                    style(key_url).cyan().underlined()
                ));
            }
            print_bullet("You can also set it later via env var or config file.");
            println!();

            let key: String = Input::new()
                .with_prompt("  Paste your API key (or press Enter to skip)")
                .allow_empty(true)
                .interact_text()?;

            if key.is_empty() {
                let env_var = provider_env_var(provider_name);
                print_bullet(&format!(
                    "Skipped. Set {} or edit config.toml later.",
                    style(env_var).yellow()
                ));
            }

            key
        }
    };

    // ── Model selection ──
    let canonical_provider = canonical_provider_name(provider_name);
    let mut model_options: Vec<(String, String)> = curated_models_for_provider(canonical_provider);

    let mut live_options: Option<Vec<(String, String)>> = None;

    if supports_live_model_fetch(provider_name) {
        let ollama_remote = canonical_provider == "ollama"
            && ollama_uses_remote_endpoint(provider_api_url.as_deref());
        let can_fetch_without_key =
            allows_unauthenticated_model_fetch(provider_name) && !ollama_remote;
        let has_api_key = !api_key.trim().is_empty()
            || ((canonical_provider != "ollama" || ollama_remote)
                && std::env::var(provider_env_var(provider_name))
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty()))
            || (provider_name == "minimax"
                && std::env::var("MINIMAX_OAUTH_TOKEN")
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty()));

        if canonical_provider == "ollama" && ollama_remote && !has_api_key {
            print_bullet(&format!(
                "Remote Ollama live-model refresh needs an API key ({}); using curated models.",
                style("OLLAMA_API_KEY").yellow()
            ));
        }

        if can_fetch_without_key || has_api_key {
            if let Some(cached) =
                load_cached_models_for_provider(workspace_dir, provider_name, MODEL_CACHE_TTL_SECS)
                    .await?
            {
                let shown_count = cached.models.len().min(LIVE_MODEL_MAX_OPTIONS);
                print_bullet(&format!(
                    "Found cached models ({shown_count}) updated {} ago.",
                    humanize_age(cached.age_secs)
                ));

                live_options = Some(build_model_options(
                    cached
                        .models
                        .into_iter()
                        .take(LIVE_MODEL_MAX_OPTIONS)
                        .collect(),
                    "cached",
                ));
            }

            let should_fetch_now = Confirm::new()
                .with_prompt(if live_options.is_some() {
                    "  Refresh models from provider now?"
                } else {
                    "  Fetch latest models from provider now?"
                })
                .default(live_options.is_none())
                .interact()?;

            if should_fetch_now {
                match fetch_live_models_for_provider(
                    provider_name,
                    &api_key,
                    provider_api_url.as_deref(),
                ) {
                    Ok(live_model_ids) if !live_model_ids.is_empty() => {
                        cache_live_models_for_provider(
                            workspace_dir,
                            provider_name,
                            &live_model_ids,
                        )
                        .await?;

                        let fetched_count = live_model_ids.len();
                        let shown_count = fetched_count.min(LIVE_MODEL_MAX_OPTIONS);
                        let shown_models: Vec<String> = live_model_ids
                            .into_iter()
                            .take(LIVE_MODEL_MAX_OPTIONS)
                            .collect();

                        if shown_count < fetched_count {
                            print_bullet(&format!(
                                "Fetched {fetched_count} models. Showing first {shown_count}."
                            ));
                        } else {
                            print_bullet(&format!("Fetched {shown_count} live models."));
                        }

                        live_options = Some(build_model_options(shown_models, "live"));
                    }
                    Ok(_) => {
                        print_bullet("Provider returned no models; using curated list.");
                    }
                    Err(error) => {
                        print_bullet(&format!(
                            "Live fetch failed ({}); using cached/curated list.",
                            style(error.to_string()).yellow()
                        ));

                        if live_options.is_none() {
                            if let Some(stale) =
                                load_any_cached_models_for_provider(workspace_dir, provider_name)
                                    .await?
                            {
                                print_bullet(&format!(
                                    "Loaded stale cache from {} ago.",
                                    humanize_age(stale.age_secs)
                                ));

                                live_options = Some(build_model_options(
                                    stale
                                        .models
                                        .into_iter()
                                        .take(LIVE_MODEL_MAX_OPTIONS)
                                        .collect(),
                                    "stale-cache",
                                ));
                            }
                        }
                    }
                }
            }
        } else {
            print_bullet("No API key detected, so using curated model list.");
            print_bullet("Tip: add an API key and rerun onboarding to fetch live models.");
        }
    }

    if let Some(live_model_options) = live_options {
        let source_options = vec![
            format!("Provider model list ({})", live_model_options.len()),
            format!("Curated starter list ({})", model_options.len()),
        ];

        let source_idx = Select::new()
            .with_prompt("  Model source")
            .items(&source_options)
            .default(0)
            .interact()?;

        if source_idx == 0 {
            model_options = live_model_options;
        }
    }

    if model_options.is_empty() {
        model_options.push((
            default_model_for_provider(provider_name),
            "Provider default model".to_string(),
        ));
    }

    model_options.push((
        CUSTOM_MODEL_SENTINEL.to_string(),
        "Custom model ID (type manually)".to_string(),
    ));

    let model_labels: Vec<String> = model_options
        .iter()
        .map(|(model_id, label)| format!("{label} — {}", style(model_id).dim()))
        .collect();

    let model_idx = Select::new()
        .with_prompt("  Select your default model")
        .items(&model_labels)
        .default(0)
        .interact()?;

    let selected_model = model_options[model_idx].0.clone();
    let model = if selected_model == CUSTOM_MODEL_SENTINEL {
        Input::new()
            .with_prompt("  Enter custom model ID")
            .default(default_model_for_provider(provider_name))
            .interact_text()?
    } else {
        selected_model
    };

    println!(
        "  {} Provider: {} | Model: {}",
        style("✓").green().bold(),
        style(provider_name).green(),
        style(&model).green()
    );

    Ok((provider_name.to_string(), api_key, model, provider_api_url))
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn canonical_provider_name_normalizes_regional_aliases() {
        assert_eq!(canonical_provider_name("qwen-intl"), "qwen");
        assert_eq!(canonical_provider_name("dashscope-us"), "qwen");
        assert_eq!(canonical_provider_name("qwen-code"), "qwen-code");
        assert_eq!(canonical_provider_name("qwen-oauth"), "qwen-code");
        assert_eq!(canonical_provider_name("codex"), "openai-codex");
        assert_eq!(canonical_provider_name("openai_codex"), "openai-codex");
        assert_eq!(canonical_provider_name("moonshot-intl"), "moonshot");
        assert_eq!(canonical_provider_name("kimi-cn"), "moonshot");
        assert_eq!(canonical_provider_name("kimi_coding"), "kimi-code");
        assert_eq!(canonical_provider_name("kimi_for_coding"), "kimi-code");
        assert_eq!(canonical_provider_name("glm-cn"), "glm");
        assert_eq!(canonical_provider_name("bigmodel"), "glm");
        assert_eq!(canonical_provider_name("minimax-cn"), "minimax");
        assert_eq!(canonical_provider_name("zai-cn"), "zai");
        assert_eq!(canonical_provider_name("z.ai-global"), "zai");
        assert_eq!(canonical_provider_name("nvidia-nim"), "nvidia");
        assert_eq!(canonical_provider_name("aws-bedrock"), "bedrock");
        assert_eq!(canonical_provider_name("build.nvidia.com"), "nvidia");
        assert_eq!(canonical_provider_name("llama.cpp"), "llamacpp");
    }

    #[test]
    fn default_model_for_provider_uses_latest_defaults() {
        assert_eq!(
            default_model_for_provider("openrouter"),
            "anthropic/claude-sonnet-4.6"
        );
        assert_eq!(default_model_for_provider("openai"), "gpt-5.2");
        assert_eq!(default_model_for_provider("openai-codex"), "gpt-5-codex");
        assert_eq!(
            default_model_for_provider("anthropic"),
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(default_model_for_provider("qwen"), "qwen-plus");
        assert_eq!(default_model_for_provider("qwen-intl"), "qwen-plus");
        assert_eq!(default_model_for_provider("qwen-code"), "qwen3-coder-plus");
        assert_eq!(default_model_for_provider("glm-cn"), "glm-5");
        assert_eq!(default_model_for_provider("minimax-cn"), "MiniMax-M2.5");
        assert_eq!(default_model_for_provider("zai-cn"), "glm-5");
        assert_eq!(default_model_for_provider("gemini"), "gemini-2.5-pro");
        assert_eq!(default_model_for_provider("google"), "gemini-2.5-pro");
        assert_eq!(default_model_for_provider("kimi-code"), "kimi-for-coding");
        assert_eq!(
            default_model_for_provider("bedrock"),
            "anthropic.claude-sonnet-4-5-20250929-v1:0"
        );
        assert_eq!(
            default_model_for_provider("google-gemini"),
            "gemini-2.5-pro"
        );
        assert_eq!(default_model_for_provider("venice"), "zai-org-glm-5");
        assert_eq!(default_model_for_provider("moonshot"), "kimi-k2.5");
        assert_eq!(
            default_model_for_provider("nvidia"),
            "meta/llama-3.3-70b-instruct"
        );
        assert_eq!(
            default_model_for_provider("nvidia-nim"),
            "meta/llama-3.3-70b-instruct"
        );
        assert_eq!(
            default_model_for_provider("llamacpp"),
            "ggml-org/gpt-oss-20b-GGUF"
        );
        assert_eq!(default_model_for_provider("sglang"), "default");
        assert_eq!(default_model_for_provider("vllm"), "default");
        assert_eq!(
            default_model_for_provider("astrai"),
            "anthropic/claude-sonnet-4.6"
        );
    }

    #[test]
    fn apply_provider_update_preserves_non_provider_settings() {
        let mut config = Config::default();
        config.default_temperature = 1.23;
        config.memory.backend = "markdown".to_string();
        config.skills.open_skills_enabled = true;
        config.channels_config.cli = false;

        apply_provider_update(
            &mut config,
            "openrouter".to_string(),
            "sk-updated".to_string(),
            "openai/gpt-5.2".to_string(),
            Some("https://openrouter.ai/api/v1".to_string()),
        );

        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt-5.2"));
        assert_eq!(config.api_key.as_deref(), Some("sk-updated"));
        assert_eq!(
            config.api_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(config.default_temperature, 1.23);
        assert_eq!(config.memory.backend, "markdown");
        assert!(config.skills.open_skills_enabled);
        assert!(!config.channels_config.cli);
    }

    #[test]
    fn apply_provider_update_clears_api_key_when_empty() {
        let mut config = Config::default();
        config.api_key = Some("sk-old".to_string());

        apply_provider_update(
            &mut config,
            "anthropic".to_string(),
            String::new(),
            "claude-sonnet-4-5-20250929".to_string(),
            None,
        );

        assert_eq!(config.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(
            config.default_model.as_deref(),
            Some("claude-sonnet-4-5-20250929")
        );
        assert!(config.api_key.is_none());
        assert!(config.api_url.is_none());
    }

    #[test]
    fn curated_models_for_openai_include_latest_choices() {
        let ids: Vec<String> = curated_models_for_provider("openai")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"gpt-5.2".to_string()));
        assert!(ids.contains(&"gpt-5-mini".to_string()));
    }

    #[test]
    fn curated_models_for_glm_removes_deprecated_flash_plus_aliases() {
        let ids: Vec<String> = curated_models_for_provider("glm")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"glm-5".to_string()));
        assert!(ids.contains(&"glm-4.7".to_string()));
        assert!(ids.contains(&"glm-4.5-air".to_string()));
        assert!(!ids.contains(&"glm-4-plus".to_string()));
        assert!(!ids.contains(&"glm-4-flash".to_string()));
    }

    #[test]
    fn curated_models_for_openai_codex_include_codex_family() {
        let ids: Vec<String> = curated_models_for_provider("openai-codex")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"gpt-5-codex".to_string()));
        assert!(ids.contains(&"gpt-5.2-codex".to_string()));
    }

    #[test]
    fn curated_models_for_openrouter_use_valid_anthropic_id() {
        let ids: Vec<String> = curated_models_for_provider("openrouter")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"anthropic/claude-sonnet-4.6".to_string()));
    }

    #[test]
    fn curated_models_for_bedrock_include_verified_model_ids() {
        let ids: Vec<String> = curated_models_for_provider("bedrock")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"anthropic.claude-sonnet-4-6".to_string()));
        assert!(ids.contains(&"anthropic.claude-opus-4-6-v1".to_string()));
        assert!(ids.contains(&"anthropic.claude-haiku-4-5-20251001-v1:0".to_string()));
        assert!(ids.contains(&"anthropic.claude-sonnet-4-5-20250929-v1:0".to_string()));
    }

    #[test]
    fn curated_models_for_moonshot_drop_deprecated_aliases() {
        let ids: Vec<String> = curated_models_for_provider("moonshot")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"kimi-k2.5".to_string()));
        assert!(ids.contains(&"kimi-k2-thinking".to_string()));
        assert!(!ids.contains(&"kimi-latest".to_string()));
        assert!(!ids.contains(&"kimi-thinking-preview".to_string()));
    }

    #[test]
    fn allows_unauthenticated_model_fetch_for_public_catalogs() {
        assert!(allows_unauthenticated_model_fetch("openrouter"));
        assert!(allows_unauthenticated_model_fetch("venice"));
        assert!(allows_unauthenticated_model_fetch("nvidia"));
        assert!(allows_unauthenticated_model_fetch("nvidia-nim"));
        assert!(allows_unauthenticated_model_fetch("build.nvidia.com"));
        assert!(allows_unauthenticated_model_fetch("astrai"));
        assert!(allows_unauthenticated_model_fetch("ollama"));
        assert!(allows_unauthenticated_model_fetch("llamacpp"));
        assert!(allows_unauthenticated_model_fetch("llama.cpp"));
        assert!(allows_unauthenticated_model_fetch("sglang"));
        assert!(allows_unauthenticated_model_fetch("vllm"));
        assert!(!allows_unauthenticated_model_fetch("openai"));
        assert!(!allows_unauthenticated_model_fetch("deepseek"));
    }

    #[test]
    fn curated_models_for_kimi_code_include_official_agent_model() {
        let ids: Vec<String> = curated_models_for_provider("kimi-code")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"kimi-for-coding".to_string()));
        assert!(ids.contains(&"kimi-k2.5".to_string()));
    }

    #[test]
    fn curated_models_for_qwen_code_include_coding_plan_models() {
        let ids: Vec<String> = curated_models_for_provider("qwen-code")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"qwen3-coder-plus".to_string()));
        assert!(ids.contains(&"qwen3.5-plus".to_string()));
        assert!(ids.contains(&"qwen3-max-2026-01-23".to_string()));
    }

    #[test]
    fn supports_live_model_fetch_for_supported_and_unsupported_providers() {
        assert!(supports_live_model_fetch("openai"));
        assert!(supports_live_model_fetch("anthropic"));
        assert!(supports_live_model_fetch("gemini"));
        assert!(supports_live_model_fetch("google"));
        assert!(supports_live_model_fetch("grok"));
        assert!(supports_live_model_fetch("together"));
        assert!(supports_live_model_fetch("nvidia"));
        assert!(supports_live_model_fetch("nvidia-nim"));
        assert!(supports_live_model_fetch("build.nvidia.com"));
        assert!(supports_live_model_fetch("ollama"));
        assert!(supports_live_model_fetch("llamacpp"));
        assert!(supports_live_model_fetch("llama.cpp"));
        assert!(supports_live_model_fetch("sglang"));
        assert!(supports_live_model_fetch("vllm"));
        assert!(supports_live_model_fetch("astrai"));
        assert!(supports_live_model_fetch("venice"));
        assert!(supports_live_model_fetch("glm-cn"));
        assert!(supports_live_model_fetch("qwen-intl"));
        assert!(!supports_live_model_fetch("minimax-cn"));
        assert!(!supports_live_model_fetch("unknown-provider"));
    }

    #[test]
    fn curated_models_provider_aliases_share_same_catalog() {
        assert_eq!(
            curated_models_for_provider("xai"),
            curated_models_for_provider("grok")
        );
        assert_eq!(
            curated_models_for_provider("together-ai"),
            curated_models_for_provider("together")
        );
        assert_eq!(
            curated_models_for_provider("gemini"),
            curated_models_for_provider("google")
        );
        assert_eq!(
            curated_models_for_provider("gemini"),
            curated_models_for_provider("google-gemini")
        );
        assert_eq!(
            curated_models_for_provider("qwen"),
            curated_models_for_provider("qwen-intl")
        );
        assert_eq!(
            curated_models_for_provider("qwen"),
            curated_models_for_provider("dashscope-us")
        );
        assert_eq!(
            curated_models_for_provider("minimax"),
            curated_models_for_provider("minimax-cn")
        );
        assert_eq!(
            curated_models_for_provider("zai"),
            curated_models_for_provider("zai-cn")
        );
        assert_eq!(
            curated_models_for_provider("nvidia"),
            curated_models_for_provider("nvidia-nim")
        );
        assert_eq!(
            curated_models_for_provider("nvidia"),
            curated_models_for_provider("build.nvidia.com")
        );
        assert_eq!(
            curated_models_for_provider("llamacpp"),
            curated_models_for_provider("llama.cpp")
        );
        assert_eq!(
            curated_models_for_provider("bedrock"),
            curated_models_for_provider("aws-bedrock")
        );
    }

    #[test]
    fn curated_models_for_nvidia_include_nim_catalog_entries() {
        let ids: Vec<String> = curated_models_for_provider("nvidia")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"meta/llama-3.3-70b-instruct".to_string()));
        assert!(ids.contains(&"deepseek-ai/deepseek-v3.2".to_string()));
        assert!(ids.contains(&"nvidia/llama-3.3-nemotron-super-49b-v1.5".to_string()));
    }

    #[test]
    fn models_endpoint_for_provider_handles_region_aliases() {
        assert_eq!(
            models_endpoint_for_provider("glm-cn"),
            Some("https://open.bigmodel.cn/api/paas/v4/models")
        );
        assert_eq!(
            models_endpoint_for_provider("zai-cn"),
            Some("https://open.bigmodel.cn/api/coding/paas/v4/models")
        );
        assert_eq!(
            models_endpoint_for_provider("qwen-intl"),
            Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models")
        );
    }

    #[test]
    fn models_endpoint_for_provider_supports_additional_openai_compatible_providers() {
        assert_eq!(
            models_endpoint_for_provider("openai-codex"),
            Some("https://api.openai.com/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("venice"),
            Some("https://api.venice.ai/api/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("cohere"),
            Some("https://api.cohere.com/compatibility/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("moonshot"),
            Some("https://api.moonshot.ai/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("llamacpp"),
            Some("http://localhost:8080/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("llama.cpp"),
            Some("http://localhost:8080/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("sglang"),
            Some("http://localhost:30000/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("vllm"),
            Some("http://localhost:8000/v1/models")
        );
        assert_eq!(models_endpoint_for_provider("perplexity"), None);
        assert_eq!(models_endpoint_for_provider("unknown-provider"), None);
    }

    #[test]
    fn resolve_live_models_endpoint_prefers_llamacpp_custom_url() {
        assert_eq!(
            resolve_live_models_endpoint("llamacpp", Some("http://127.0.0.1:8033/v1")),
            Some("http://127.0.0.1:8033/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("llama.cpp", Some("http://127.0.0.1:8033/v1/")),
            Some("http://127.0.0.1:8033/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("llamacpp", Some("http://127.0.0.1:8033/v1/models")),
            Some("http://127.0.0.1:8033/v1/models".to_string())
        );
    }

    #[test]
    fn resolve_live_models_endpoint_falls_back_to_provider_defaults() {
        assert_eq!(
            resolve_live_models_endpoint("llamacpp", None),
            Some("http://localhost:8080/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("sglang", None),
            Some("http://localhost:30000/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("vllm", None),
            Some("http://localhost:8000/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("venice", Some("http://localhost:9999/v1")),
            Some("https://api.venice.ai/api/v1/models".to_string())
        );
        assert_eq!(resolve_live_models_endpoint("unknown-provider", None), None);
    }

    #[test]
    fn resolve_live_models_endpoint_supports_custom_provider_urls() {
        assert_eq!(
            resolve_live_models_endpoint("custom:https://proxy.example.com/v1", None),
            Some("https://proxy.example.com/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("custom:https://proxy.example.com/v1/models", None),
            Some("https://proxy.example.com/v1/models".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_url_strips_api_suffix_and_trailing_slash() {
        assert_eq!(
            normalize_ollama_endpoint_url(" https://ollama.com/api/ "),
            "https://ollama.com".to_string()
        );
        assert_eq!(
            normalize_ollama_endpoint_url("https://ollama.com/"),
            "https://ollama.com".to_string()
        );
        assert_eq!(normalize_ollama_endpoint_url(""), "");
    }

    #[test]
    fn ollama_uses_remote_endpoint_distinguishes_local_and_remote_urls() {
        assert!(!ollama_uses_remote_endpoint(None));
        assert!(!ollama_uses_remote_endpoint(Some("http://localhost:11434")));
        assert!(!ollama_uses_remote_endpoint(Some(
            "http://127.0.0.1:11434/api"
        )));
        assert!(ollama_uses_remote_endpoint(Some("https://ollama.com")));
        assert!(ollama_uses_remote_endpoint(Some("https://ollama.com/api")));
    }

    #[test]
    fn resolve_live_models_endpoint_prefers_vllm_custom_url() {
        assert_eq!(
            resolve_live_models_endpoint("vllm", Some("http://127.0.0.1:9000/v1")),
            Some("http://127.0.0.1:9000/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("vllm", Some("http://127.0.0.1:9000/v1/models")),
            Some("http://127.0.0.1:9000/v1/models".to_string())
        );
    }

    #[test]
    fn parse_openai_model_ids_supports_data_array_payload() {
        let payload = json!({
            "data": [
                {"id": "  gpt-5.1  "},
                {"id": "gpt-5-mini"},
                {"id": "gpt-5.1"},
                {"id": ""}
            ]
        });

        let ids = parse_openai_compatible_model_ids(&payload);
        assert_eq!(ids, vec!["gpt-5-mini".to_string(), "gpt-5.1".to_string()]);
    }

    #[test]
    fn parse_openai_model_ids_supports_root_array_payload() {
        let payload = json!([
            {"id": "alpha"},
            {"id": "beta"},
            {"id": "alpha"}
        ]);

        let ids = parse_openai_compatible_model_ids(&payload);
        assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn normalize_model_ids_deduplicates_case_insensitively() {
        let ids = normalize_model_ids(vec![
            "GPT-5".to_string(),
            "gpt-5".to_string(),
            "gpt-5-mini".to_string(),
            " GPT-5-MINI ".to_string(),
        ]);
        assert_eq!(ids, vec!["GPT-5".to_string(), "gpt-5-mini".to_string()]);
    }

    #[test]
    fn parse_gemini_model_ids_filters_for_generate_content() {
        let payload = json!({
            "models": [
                {
                    "name": "models/gemini-2.5-pro",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                {
                    "name": "models/gemini-2.5-flash",
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        });

        let ids = parse_gemini_model_ids(&payload);
        assert_eq!(
            ids,
            vec!["gemini-2.5-flash".to_string(), "gemini-2.5-pro".to_string()]
        );
    }

    #[test]
    fn parse_ollama_model_ids_extracts_and_deduplicates_names() {
        let payload = json!({
            "models": [
                {"name": "llama3.2:latest"},
                {"name": "mistral:latest"},
                {"name": "llama3.2:latest"}
            ]
        });

        let ids = parse_ollama_model_ids(&payload);
        assert_eq!(
            ids,
            vec!["llama3.2:latest".to_string(), "mistral:latest".to_string()]
        );
    }

    #[tokio::test]
    async fn model_cache_round_trip_returns_fresh_entry() {
        let tmp = TempDir::new().unwrap();
        let models = vec!["gpt-5.1".to_string(), "gpt-5-mini".to_string()];

        cache_live_models_for_provider(tmp.path(), "openai", &models)
            .await
            .unwrap();

        let cached = load_cached_models_for_provider(tmp.path(), "openai", MODEL_CACHE_TTL_SECS)
            .await
            .unwrap();
        let cached = cached.expect("expected fresh cached models");

        assert_eq!(cached.models.len(), 2);
        assert!(cached.models.contains(&"gpt-5.1".to_string()));
        assert!(cached.models.contains(&"gpt-5-mini".to_string()));
    }

    #[tokio::test]
    async fn model_cache_ttl_filters_stale_entries() {
        let tmp = TempDir::new().unwrap();
        let stale = ModelCacheState {
            entries: vec![ModelCacheEntry {
                provider: "openai".to_string(),
                fetched_at_unix: now_unix_secs().saturating_sub(MODEL_CACHE_TTL_SECS + 120),
                models: vec!["gpt-5.1".to_string()],
            }],
        };

        save_model_cache_state(tmp.path(), &stale).await.unwrap();

        let fresh = load_cached_models_for_provider(tmp.path(), "openai", MODEL_CACHE_TTL_SECS)
            .await
            .unwrap();
        assert!(fresh.is_none());

        let stale_any = load_any_cached_models_for_provider(tmp.path(), "openai")
            .await
            .unwrap();
        assert!(stale_any.is_some());
    }

    #[tokio::test]
    async fn run_models_refresh_uses_fresh_cache_without_network() {
        let tmp = TempDir::new().unwrap();

        cache_live_models_for_provider(tmp.path(), "openai", &["gpt-5.1".to_string()])
            .await
            .unwrap();

        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            default_provider: Some("openai".to_string()),
            ..Config::default()
        };

        run_models_refresh(&config, None, false).await.unwrap();
    }

    #[tokio::test]
    async fn run_models_refresh_rejects_unsupported_provider() {
        let tmp = TempDir::new().unwrap();

        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            // Use a non-provider channel key to keep this test deterministic and offline.
            default_provider: Some("imessage".to_string()),
            ..Config::default()
        };

        let err = run_models_refresh(&config, None, true).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("does not support live model discovery"));
    }

    #[test]
    fn provider_env_var_known_providers() {
        assert_eq!(provider_env_var("openrouter"), "OPENROUTER_API_KEY");
        assert_eq!(provider_env_var("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(provider_env_var("openai-codex"), "OPENAI_API_KEY");
        assert_eq!(provider_env_var("openai"), "OPENAI_API_KEY");
        assert_eq!(provider_env_var("ollama"), "OLLAMA_API_KEY");
        assert_eq!(provider_env_var("llamacpp"), "LLAMACPP_API_KEY");
        assert_eq!(provider_env_var("llama.cpp"), "LLAMACPP_API_KEY");
        assert_eq!(provider_env_var("sglang"), "SGLANG_API_KEY");
        assert_eq!(provider_env_var("vllm"), "VLLM_API_KEY");
        assert_eq!(provider_env_var("xai"), "XAI_API_KEY");
        assert_eq!(provider_env_var("grok"), "XAI_API_KEY");
        assert_eq!(provider_env_var("together"), "TOGETHER_API_KEY");
        assert_eq!(provider_env_var("together-ai"), "TOGETHER_API_KEY");
        assert_eq!(provider_env_var("google"), "GEMINI_API_KEY");
        assert_eq!(provider_env_var("google-gemini"), "GEMINI_API_KEY");
        assert_eq!(provider_env_var("gemini"), "GEMINI_API_KEY");
        assert_eq!(provider_env_var("qwen"), "DASHSCOPE_API_KEY");
        assert_eq!(provider_env_var("qwen-intl"), "DASHSCOPE_API_KEY");
        assert_eq!(provider_env_var("dashscope-us"), "DASHSCOPE_API_KEY");
        assert_eq!(provider_env_var("qwen-code"), "QWEN_OAUTH_TOKEN");
        assert_eq!(provider_env_var("qwen-oauth"), "QWEN_OAUTH_TOKEN");
        assert_eq!(provider_env_var("glm-cn"), "GLM_API_KEY");
        assert_eq!(provider_env_var("minimax-cn"), "MINIMAX_API_KEY");
        assert_eq!(provider_env_var("kimi-code"), "KIMI_CODE_API_KEY");
        assert_eq!(provider_env_var("kimi_coding"), "KIMI_CODE_API_KEY");
        assert_eq!(provider_env_var("kimi_for_coding"), "KIMI_CODE_API_KEY");
        assert_eq!(provider_env_var("minimax-oauth"), "MINIMAX_API_KEY");
        assert_eq!(provider_env_var("minimax-oauth-cn"), "MINIMAX_API_KEY");
        assert_eq!(provider_env_var("moonshot-intl"), "MOONSHOT_API_KEY");
        assert_eq!(provider_env_var("zai-cn"), "ZAI_API_KEY");
        assert_eq!(provider_env_var("nvidia"), "NVIDIA_API_KEY");
        assert_eq!(provider_env_var("nvidia-nim"), "NVIDIA_API_KEY");
        assert_eq!(provider_env_var("build.nvidia.com"), "NVIDIA_API_KEY");
        assert_eq!(provider_env_var("astrai"), "ASTRAI_API_KEY");
    }

    #[test]
    fn provider_supports_keyless_local_usage_for_local_providers() {
        assert!(provider_supports_keyless_local_usage("ollama"));
        assert!(provider_supports_keyless_local_usage("llamacpp"));
        assert!(provider_supports_keyless_local_usage("llama.cpp"));
        assert!(provider_supports_keyless_local_usage("sglang"));
        assert!(provider_supports_keyless_local_usage("vllm"));
        assert!(!provider_supports_keyless_local_usage("openai"));
    }

    #[test]
    fn provider_supports_device_flow_copilot() {
        assert!(provider_supports_device_flow("copilot"));
        assert!(provider_supports_device_flow("github-copilot"));
        assert!(provider_supports_device_flow("gemini"));
        assert!(provider_supports_device_flow("openai-codex"));
        assert!(!provider_supports_device_flow("openai"));
        assert!(!provider_supports_device_flow("openrouter"));
    }

    #[test]
    fn local_provider_choices_include_sglang() {
        let choices = local_provider_choices();
        assert!(choices.iter().any(|(provider, _)| *provider == "sglang"));
    }

    #[test]
    fn provider_env_var_unknown_falls_back() {
        assert_eq!(provider_env_var("some-new-provider"), "API_KEY");
    }
}
