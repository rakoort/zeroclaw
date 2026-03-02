//! Google Gemini provider with support for:
//! - Direct API key (`GEMINI_API_KEY` env var or config)
//! - Vertex AI service account (`GOOGLE_APPLICATION_CREDENTIALS` or `VERTEX_SERVICE_ACCOUNT_JSON`)
//! - Gemini CLI OAuth tokens (reuse existing ~/.gemini/ authentication)
//! - ZeroClaw auth-profiles OAuth tokens

use crate::auth::AuthService;
use crate::providers::gemini_sanitize::sanitize_transcript_for_gemini;
use crate::providers::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ProviderCapabilities, TokenUsage, ToolCall,
    ToolsPayload,
};
use async_trait::async_trait;
use base64::Engine;
use directories::UserDirs;
use reqwest::Client;
use ring::signature::RsaKeyPair;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Gemini provider supporting multiple authentication methods.
pub struct GeminiProvider {
    auth: Option<GeminiAuth>,
    oauth_project: Arc<tokio::sync::Mutex<Option<String>>>,
    oauth_cred_paths: Vec<PathBuf>,
    oauth_index: Arc<tokio::sync::Mutex<usize>>,
    /// AuthService for managed profiles (auth-profiles.json).
    auth_service: Option<AuthService>,
    /// Override profile name for managed auth.
    auth_profile_override: Option<String>,
}

/// Mutable OAuth token state — supports runtime refresh for long-lived processes.
struct OAuthTokenState {
    access_token: String,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    /// Expiry as unix millis. `None` means unknown (treat as potentially expired).
    expiry_millis: Option<i64>,
}

/// Parsed GCP service account credentials for Vertex AI.
struct VertexServiceAccountCreds {
    client_email: String,
    key_pair: Arc<RsaKeyPair>,
    project_id: String,
    region: String,
}

impl std::fmt::Debug for VertexServiceAccountCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexServiceAccountCreds")
            .field("client_email", &self.client_email)
            .field("project_id", &self.project_id)
            .field("region", &self.region)
            .field("key_pair", &"<redacted>")
            .finish()
    }
}

/// Cached Vertex AI access token with expiry tracking.
struct VertexTokenState {
    access_token: String,
    /// Expiry as unix millis.
    expiry_millis: i64,
}

/// Resolved credential — the variant determines both the HTTP auth method
/// and the diagnostic label returned by `auth_source()`.
enum GeminiAuth {
    /// Explicit API key from config: sent as `?key=` query parameter.
    ExplicitKey(String),
    /// API key from `GEMINI_API_KEY` env var: sent as `?key=`.
    EnvGeminiKey(String),
    /// API key from `GOOGLE_API_KEY` env var: sent as `?key=`.
    EnvGoogleKey(String),
    /// OAuth access token from Gemini CLI: sent as `Authorization: Bearer`.
    /// Wrapped in a Mutex to allow runtime token refresh.
    OAuthToken(Arc<tokio::sync::Mutex<OAuthTokenState>>),
    /// OAuth token managed by AuthService (auth-profiles.json).
    /// Token refresh is handled by AuthService, not here.
    ManagedOAuth,
    /// Vertex AI via GCP service account JWT grant.
    /// Bearer token acquired via self-signed JWT exchange.
    VertexServiceAccount {
        creds: Arc<VertexServiceAccountCreds>,
        token_state: Arc<tokio::sync::Mutex<Option<VertexTokenState>>>,
    },
}

impl GeminiAuth {
    /// Whether this credential is an API key (sent as `?key=` query param).
    fn is_api_key(&self) -> bool {
        matches!(
            self,
            GeminiAuth::ExplicitKey(_) | GeminiAuth::EnvGeminiKey(_) | GeminiAuth::EnvGoogleKey(_)
        )
    }

    /// Whether this credential is an OAuth token (CLI or managed).
    fn is_oauth(&self) -> bool {
        matches!(self, GeminiAuth::OAuthToken(_) | GeminiAuth::ManagedOAuth)
    }

    /// The raw credential string (for API key variants only).
    fn api_key_credential(&self) -> &str {
        match self {
            GeminiAuth::ExplicitKey(s)
            | GeminiAuth::EnvGeminiKey(s)
            | GeminiAuth::EnvGoogleKey(s) => s,
            GeminiAuth::OAuthToken(_)
            | GeminiAuth::ManagedOAuth
            | GeminiAuth::VertexServiceAccount { .. } => "",
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// SCHEMA SANITIZER — convert JSON Schema to Gemini-compatible format
// ══════════════════════════════════════════════════════════════════════════════

/// Recursively sanitize a JSON Schema value for the Gemini API.
///
/// Gemini's proto-based Schema doesn't support:
/// - `"type": ["string", "null"]` (union types) — convert to scalar + `"nullable": true`
fn sanitize_schema_for_gemini(value: &mut serde_json::Value) {
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    // Fix union types: "type": ["string", "null"] → "type": "string", "nullable": true
    if let Some(type_val) = obj.get("type").cloned() {
        if let Some(arr) = type_val.as_array() {
            let mut non_null: Option<String> = None;
            let mut has_null = false;
            for v in arr {
                if let Some(s) = v.as_str() {
                    if s == "null" {
                        has_null = true;
                    } else if non_null.is_none() {
                        non_null = Some(s.to_string());
                    }
                }
            }
            if let Some(t) = non_null {
                obj.insert("type".to_string(), serde_json::Value::String(t));
                if has_null {
                    obj.insert("nullable".to_string(), serde_json::Value::Bool(true));
                }
            }
        }
    }

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties") {
        if let Some(props_obj) = props.as_object_mut() {
            for val in props_obj.values_mut() {
                sanitize_schema_for_gemini(val);
            }
        }
    }

    // Recurse into items (for array types)
    if let Some(items) = obj.get_mut("items") {
        sanitize_schema_for_gemini(items);
    }

    // Recurse into oneOf/anyOf entries
    for key in &["oneOf", "anyOf"] {
        if let Some(variants) = obj.get_mut(*key) {
            if let Some(arr) = variants.as_array_mut() {
                for item in arr.iter_mut() {
                    sanitize_schema_for_gemini(item);
                }
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// API REQUEST/RESPONSE TYPES
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, Clone)]
struct GenerateContentRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolDeclaration>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolConfig>,
}

#[derive(Debug, Serialize, Clone)]
struct GeminiToolDeclaration {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Clone)]
struct GeminiToolConfig {
    #[serde(rename = "functionCallingConfig")]
    function_calling_config: FunctionCallingConfigMode,
}

#[derive(Debug, Serialize, Clone)]
struct FunctionCallingConfigMode {
    mode: String,
}

/// Request envelope for the internal cloudcode-pa API.
/// OAuth tokens from Gemini CLI are scoped for this endpoint.
///
/// The internal API expects a nested structure:
/// ```json
/// {
///   "model": "models/gemini-...",
///   "project": "...",
///   "request": {
///     "contents": [...],
///     "systemInstruction": {...},
///     "generationConfig": {...}
///   }
/// }
/// ```
/// Ref: gemini-cli `packages/core/src/code_assist/converter.ts`
#[derive(Debug, Serialize)]
struct InternalGenerateContentEnvelope {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_prompt_id: Option<String>,
    request: InternalGenerateContentRequest,
}

/// Nested request payload for cloudcode-pa's code assist APIs.
#[derive(Debug, Serialize)]
struct InternalGenerateContentRequest {
    contents: Vec<Content>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolDeclaration>>,
    #[serde(rename = "toolConfig", skip_serializing_if = "Option::is_none")]
    tool_config: Option<GeminiToolConfig>,
}

#[derive(Debug, Serialize, Clone)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Thinking models: marks this part as internal reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
    /// Opaque signature for thinking context — must be replayed exactly as received.
    #[serde(
        default,
        rename = "thoughtSignature",
        skip_serializing_if = "Option::is_none"
    )]
    thought_signature: Option<String>,
    #[serde(
        default,
        rename = "functionCall",
        skip_serializing_if = "Option::is_none"
    )]
    function_call: Option<FunctionCallPart>,
    #[serde(
        default,
        rename = "functionResponse",
        skip_serializing_if = "Option::is_none"
    )]
    function_response: Option<FunctionResponsePart>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FunctionCallPart {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FunctionResponsePart {
    name: String,
    response: serde_json::Value,
}

#[derive(Debug, Serialize, Clone)]
struct GenerationConfig {
    temperature: f64,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<ApiError>,
    #[serde(default)]
    response: Option<Box<GenerateContentResponse>>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
struct GeminiUsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
}

/// Response envelope for the internal cloudcode-pa API.
/// The internal API nests the standard response under a `response` field.
#[derive(Debug, Deserialize)]
struct InternalGenerateContentResponse {
    response: GenerateContentResponse,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<CandidateContent>,
}

#[derive(Debug, Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

#[derive(Debug, Deserialize)]
struct ResponsePart {
    #[serde(default)]
    text: Option<String>,
    /// Thinking models (e.g. gemini-3-pro-preview) mark reasoning parts with `thought: true`.
    #[serde(default)]
    thought: bool,
    /// Opaque signature for thinking context — must be replayed exactly as received.
    #[serde(default, rename = "thoughtSignature")]
    thought_signature: Option<String>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FunctionCallResponse>,
}

#[derive(Debug, Deserialize)]
struct FunctionCallResponse {
    name: String,
    args: serde_json::Value,
}

impl CandidateContent {
    /// Extract effective text, skipping thinking/signature parts.
    ///
    /// Gemini thinking models (e.g. gemini-3-pro-preview) return parts like:
    /// - `{"thought": true, "text": "reasoning..."}` — internal reasoning
    /// - `{"text": "actual answer"}` — the real response
    /// - `{"thoughtSignature": "..."}` — opaque signature (no text field)
    ///
    /// Returns the non-thinking text, falling back to thinking text only when
    /// no non-thinking content is available.
    fn effective_text(self) -> Option<String> {
        let mut answer_parts: Vec<String> = Vec::new();
        let mut first_thinking: Option<String> = None;

        for part in self.parts {
            if let Some(text) = part.text {
                if text.is_empty() {
                    continue;
                }
                if !part.thought {
                    answer_parts.push(text);
                } else if first_thinking.is_none() {
                    first_thinking = Some(text);
                }
            }
        }

        if answer_parts.is_empty() {
            first_thinking
        } else {
            Some(answer_parts.join(""))
        }
    }

    fn extract_response(self) -> (Option<String>, Vec<ToolCall>, Vec<Part>) {
        let mut answer_parts: Vec<String> = Vec::new();
        let mut first_thinking: Option<String> = None;
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut all_parts: Vec<Part> = Vec::new();

        for part in self.parts {
            // Convert ResponsePart -> outbound Part (preserves all fields)
            all_parts.push(Part {
                text: part.text.clone(),
                thought: if part.thought { Some(true) } else { None },
                thought_signature: part.thought_signature.clone(),
                function_call: part.function_call.as_ref().map(|fc| FunctionCallPart {
                    name: fc.name.clone(),
                    args: fc.args.clone(),
                }),
                function_response: None,
            });

            if let Some(fc) = part.function_call {
                tool_calls.push(ToolCall {
                    id: format!("gemini_call_{}", tool_calls.len()),
                    name: fc.name,
                    arguments: fc.args.to_string(),
                    thought_signature: part.thought_signature.clone(),
                });
            }
            if let Some(text) = part.text {
                if text.is_empty() {
                    continue;
                }
                if !part.thought {
                    answer_parts.push(text);
                } else if first_thinking.is_none() {
                    first_thinking = Some(text);
                }
            }
        }

        let text = if answer_parts.is_empty() {
            first_thinking
        } else {
            Some(answer_parts.join(""))
        };

        (text, tool_calls, all_parts)
    }
}

struct GeminiResponse {
    text: Option<String>,
    tool_calls: Vec<ToolCall>,
    raw_parts: Vec<Part>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    message: String,
}

impl GenerateContentResponse {
    /// cloudcode-pa wraps the actual response under `response`.
    fn into_effective_response(self) -> Self {
        match self {
            Self {
                response: Some(inner),
                ..
            } => *inner,
            other => other,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// GEMINI CLI TOKEN STRUCTURES
// ══════════════════════════════════════════════════════════════════════════════

/// OAuth token stored by Gemini CLI in `~/.gemini/oauth_creds.json`
#[derive(Debug, Deserialize)]
struct GeminiCliOAuthCreds {
    access_token: Option<String>,
    #[serde(alias = "idToken")]
    id_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(alias = "clientId")]
    client_id: Option<String>,
    #[serde(alias = "clientSecret")]
    client_secret: Option<String>,
    /// Unix milliseconds expiry (used by newer Gemini CLI versions).
    #[serde(alias = "expiryDate")]
    expiry_date: Option<i64>,
    /// RFC 3339 expiry string (used by older Gemini CLI versions).
    expiry: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
// GEMINI CLI OAUTH CONSTANTS
// ══════════════════════════════════════════════════════════════════════════════

/// Google OAuth token endpoint.
const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Internal API endpoint used by Gemini CLI for OAuth users.
/// See: https://github.com/google-gemini/gemini-cli/issues/19200
const CLOUDCODE_PA_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal";

/// loadCodeAssist endpoint for resolving the project ID.
const LOAD_CODE_ASSIST_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";

/// Public API endpoint for API key users.
const PUBLIC_API_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

// ══════════════════════════════════════════════════════════════════════════════
// TOKEN REFRESH
// ══════════════════════════════════════════════════════════════════════════════

/// Result of a successful token refresh.
struct RefreshedToken {
    access_token: String,
    /// Expiry as unix millis (computed from `expires_in` seconds in the response).
    expiry_millis: Option<i64>,
}

/// Refresh an expired Gemini CLI OAuth token using the refresh_token grant.
///
/// Client credentials are optional and can be sourced from:
/// - `oauth_creds.json` if present
/// - `GEMINI_OAUTH_CLIENT_ID` / `GEMINI_OAUTH_CLIENT_SECRET` env vars
fn refresh_gemini_cli_token(
    refresh_token: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> anyhow::Result<RefreshedToken> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());

    let form = build_oauth_refresh_form(refresh_token, client_id, client_secret);

    let response = client
        .post(GOOGLE_TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .map_err(|error| anyhow::anyhow!("Gemini CLI OAuth refresh request failed: {error}"))?;

    let status = response.status();
    let body = response
        .text()
        .unwrap_or_else(|_| "<failed to read response body>".to_string());

    if !status.is_success() {
        anyhow::bail!("Gemini CLI OAuth refresh failed (HTTP {status}): {body}");
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: Option<String>,
        expires_in: Option<i64>,
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|_| anyhow::anyhow!("Gemini CLI OAuth refresh response is not valid JSON"))?;

    let access_token = parsed
        .access_token
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Gemini CLI OAuth refresh response missing access_token"))?;

    let expiry_millis = parsed.expires_in.and_then(|secs| {
        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())?;
        now_millis.checked_add(secs.checked_mul(1000)?)
    });

    Ok(RefreshedToken {
        access_token,
        expiry_millis,
    })
}

fn build_oauth_refresh_form(
    refresh_token: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
    ];
    if let Some(id) = client_id.and_then(GeminiProvider::normalize_non_empty) {
        form.push(("client_id", id));
    }
    if let Some(secret) = client_secret.and_then(GeminiProvider::normalize_non_empty) {
        form.push(("client_secret", secret));
    }
    form
}

fn extract_client_id_from_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;

    #[derive(Deserialize)]
    struct IdTokenClaims {
        aud: Option<String>,
        azp: Option<String>,
    }

    let claims: IdTokenClaims = serde_json::from_slice(&decoded).ok()?;
    claims
        .aud
        .as_deref()
        .and_then(GeminiProvider::normalize_non_empty)
        .or_else(|| {
            claims
                .azp
                .as_deref()
                .and_then(GeminiProvider::normalize_non_empty)
        })
}

/// Async version of token refresh for use during runtime (inside tokio context).
async fn refresh_gemini_cli_token_async(
    refresh_token: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> anyhow::Result<RefreshedToken> {
    let refresh_token = refresh_token.to_string();
    let client_id = client_id.map(str::to_string);
    let client_secret = client_secret.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        refresh_gemini_cli_token(
            &refresh_token,
            client_id.as_deref(),
            client_secret.as_deref(),
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("Token refresh task panicked: {e}"))?
}

impl GeminiProvider {
    /// Create a new Gemini provider.
    ///
    /// Authentication priority:
    /// 1. Explicit API key passed in
    /// 2. `GEMINI_API_KEY` environment variable
    /// 3. `GOOGLE_API_KEY` environment variable
    /// 4. Vertex AI service account (`VERTEX_SERVICE_ACCOUNT_JSON` or `GOOGLE_APPLICATION_CREDENTIALS`)
    /// 5. Gemini CLI OAuth tokens (`~/.gemini/oauth_creds.json`)
    pub fn new(api_key: Option<&str>) -> Self {
        let oauth_cred_paths = Self::discover_oauth_cred_paths();
        let resolved_auth = api_key
            .and_then(Self::normalize_non_empty)
            .map(GeminiAuth::ExplicitKey)
            .or_else(|| Self::load_non_empty_env("GEMINI_API_KEY").map(GeminiAuth::EnvGeminiKey))
            .or_else(|| Self::load_non_empty_env("GOOGLE_API_KEY").map(GeminiAuth::EnvGoogleKey))
            .or_else(Self::try_load_vertex_service_account)
            .or_else(|| {
                Self::try_load_gemini_cli_token(oauth_cred_paths.first())
                    .map(|state| GeminiAuth::OAuthToken(Arc::new(tokio::sync::Mutex::new(state))))
            });

        Self {
            auth: resolved_auth,
            oauth_project: Arc::new(tokio::sync::Mutex::new(None)),
            oauth_cred_paths,
            oauth_index: Arc::new(tokio::sync::Mutex::new(0)),
            auth_service: None,
            auth_profile_override: None,
        }
    }

    /// Create a new Gemini provider with managed OAuth from auth-profiles.json.
    ///
    /// Authentication priority:
    /// 1. Explicit API key passed in
    /// 2. `GEMINI_API_KEY` environment variable
    /// 3. `GOOGLE_API_KEY` environment variable
    /// 4. Vertex AI service account (`VERTEX_SERVICE_ACCOUNT_JSON` or `GOOGLE_APPLICATION_CREDENTIALS`)
    /// 5. Managed OAuth from auth-profiles.json (if auth_service provided)
    /// 6. Gemini CLI OAuth tokens (`~/.gemini/oauth_creds.json`)
    pub fn new_with_auth(
        api_key: Option<&str>,
        auth_service: AuthService,
        profile_override: Option<String>,
    ) -> Self {
        let oauth_cred_paths = Self::discover_oauth_cred_paths();

        // First check API keys, then Vertex service account
        let resolved_auth = api_key
            .and_then(Self::normalize_non_empty)
            .map(GeminiAuth::ExplicitKey)
            .or_else(|| Self::load_non_empty_env("GEMINI_API_KEY").map(GeminiAuth::EnvGeminiKey))
            .or_else(|| Self::load_non_empty_env("GOOGLE_API_KEY").map(GeminiAuth::EnvGoogleKey))
            .or_else(Self::try_load_vertex_service_account);

        // If no API key or Vertex SA, try managed OAuth (checked at runtime)
        // or fall back to CLI OAuth
        let (auth, use_managed) = if resolved_auth.is_some() {
            (resolved_auth, false)
        } else {
            // Check if we have a managed profile - this is a blocking check
            // but we need to know at construction time
            let has_managed = std::thread::scope(|s| {
                s.spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    rt.block_on(async {
                        auth_service
                            .get_gemini_profile(profile_override.as_deref())
                            .await
                            .ok()
                            .flatten()
                    })
                })
                .join()
                .ok()
                .flatten()
                .is_some()
            });

            if has_managed {
                (Some(GeminiAuth::ManagedOAuth), true)
            } else {
                // Fall back to CLI OAuth
                let cli_auth = Self::try_load_gemini_cli_token(oauth_cred_paths.first())
                    .map(|state| GeminiAuth::OAuthToken(Arc::new(tokio::sync::Mutex::new(state))));
                (cli_auth, false)
            }
        };

        Self {
            auth,
            oauth_project: Arc::new(tokio::sync::Mutex::new(None)),
            oauth_cred_paths,
            oauth_index: Arc::new(tokio::sync::Mutex::new(0)),
            auth_service: if use_managed {
                Some(auth_service)
            } else {
                None
            },
            auth_profile_override: profile_override,
        }
    }

    fn normalize_non_empty(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn load_non_empty_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .and_then(|value| Self::normalize_non_empty(&value))
    }

    fn load_gemini_cli_creds(creds_path: &PathBuf) -> Option<GeminiCliOAuthCreds> {
        if !creds_path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(creds_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Discover all OAuth credential files from known Gemini CLI installations.
    ///
    /// Looks in `~/.gemini/oauth_creds.json` (default) plus any
    /// `~/.gemini-*-home/.gemini/oauth_creds.json` siblings.
    fn discover_oauth_cred_paths() -> Vec<PathBuf> {
        let home = match UserDirs::new() {
            Some(u) => u.home_dir().to_path_buf(),
            None => return Vec::new(),
        };

        let mut paths = Vec::new();

        let primary = home.join(".gemini").join("oauth_creds.json");
        if primary.exists() {
            paths.push(primary);
        }

        if let Ok(entries) = std::fs::read_dir(&home) {
            let mut extras: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with(".gemini-") && name.ends_with("-home") {
                        let path = e.path().join(".gemini").join("oauth_creds.json");
                        if path.exists() {
                            return Some(path);
                        }
                    }
                    None
                })
                .collect();
            extras.sort();
            paths.extend(extras);
        }

        paths
    }

    /// Try to load OAuth credentials from Gemini CLI's cached credentials.
    /// Location: `~/.gemini/oauth_creds.json`
    ///
    /// Returns the full `OAuthTokenState` so the provider can refresh at runtime.
    fn try_load_gemini_cli_token(path: Option<&PathBuf>) -> Option<OAuthTokenState> {
        let creds = Self::load_gemini_cli_creds(path?)?;

        // Determine expiry in millis: prefer expiry_date over expiry (RFC 3339)
        let expiry_millis = creds.expiry_date.or_else(|| {
            creds.expiry.as_deref().and_then(|expiry| {
                chrono::DateTime::parse_from_rfc3339(expiry)
                    .ok()
                    .map(|dt| dt.timestamp_millis())
            })
        });

        let access_token = creds
            .access_token
            .and_then(|token| Self::normalize_non_empty(&token))?;

        let id_token_client_id = creds
            .id_token
            .as_deref()
            .and_then(extract_client_id_from_id_token);

        let client_id = Self::load_non_empty_env("GEMINI_OAUTH_CLIENT_ID")
            .or_else(|| {
                creds
                    .client_id
                    .as_deref()
                    .and_then(Self::normalize_non_empty)
            })
            .or(id_token_client_id);
        let client_secret = Self::load_non_empty_env("GEMINI_OAUTH_CLIENT_SECRET").or_else(|| {
            creds
                .client_secret
                .as_deref()
                .and_then(Self::normalize_non_empty)
        });

        Some(OAuthTokenState {
            access_token,
            refresh_token: creds.refresh_token,
            client_id,
            client_secret,
            expiry_millis,
        })
    }

    /// Get the Gemini CLI config directory (~/.gemini)
    fn gemini_cli_dir() -> Option<PathBuf> {
        UserDirs::new().map(|u| u.home_dir().join(".gemini"))
    }

    /// Check if Gemini CLI is configured and has valid credentials
    pub fn has_cli_credentials() -> bool {
        Self::discover_oauth_cred_paths().iter().any(|path| {
            Self::load_gemini_cli_creds(path)
                .and_then(|creds| {
                    creds
                        .access_token
                        .as_deref()
                        .and_then(Self::normalize_non_empty)
                })
                .is_some()
        })
    }

    /// Check if any Gemini authentication is available
    pub fn has_any_auth() -> bool {
        Self::load_non_empty_env("GEMINI_API_KEY").is_some()
            || Self::load_non_empty_env("GOOGLE_API_KEY").is_some()
            || Self::load_non_empty_env("VERTEX_SERVICE_ACCOUNT_JSON").is_some()
            || Self::load_non_empty_env("GOOGLE_APPLICATION_CREDENTIALS").is_some()
            || Self::has_cli_credentials()
    }

    /// Parse a service account JSON string into Vertex credentials.
    fn parse_vertex_service_account_json(
        json_str: &str,
        region: &str,
    ) -> anyhow::Result<VertexServiceAccountCreds> {
        #[derive(Deserialize)]
        struct ServiceAccountJson {
            r#type: Option<String>,
            project_id: Option<String>,
            private_key: Option<String>,
            client_email: Option<String>,
        }

        let sa: ServiceAccountJson = serde_json::from_str(json_str)
            .map_err(|e| anyhow::anyhow!("invalid service account JSON: {e}"))?;

        let sa_type = sa.r#type.unwrap_or_default();
        if sa_type != "service_account" {
            anyhow::bail!(
                "expected type \"service_account\" in credentials JSON, got \"{sa_type}\""
            );
        }

        let client_email = sa
            .client_email
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing client_email in service account JSON"))?;
        let project_id = sa
            .project_id
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing project_id in service account JSON"))?;
        let private_key_pem = sa
            .private_key
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing private_key in service account JSON"))?;

        let parsed_pem = pem::parse(&private_key_pem)
            .map_err(|e| anyhow::anyhow!("failed to decode PEM private key: {e}"))?;

        let key_pair = match parsed_pem.tag() {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(parsed_pem.contents()),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(parsed_pem.contents()),
            other => anyhow::bail!(
                "unsupported PEM tag \"{other}\", expected PRIVATE KEY or RSA PRIVATE KEY"
            ),
        }
        .map_err(|e| anyhow::anyhow!("failed to parse RSA private key: {e}"))?;

        Ok(VertexServiceAccountCreds {
            client_email,
            key_pair: Arc::new(key_pair),
            project_id,
            region: region.to_string(),
        })
    }

    /// Try to load Vertex AI service account credentials from environment.
    ///
    /// Checks (in order):
    /// 1. `VERTEX_SERVICE_ACCOUNT_JSON` — base64-encoded JSON (for containers)
    /// 2. `GOOGLE_APPLICATION_CREDENTIALS` — file path to JSON
    fn try_load_vertex_service_account() -> Option<GeminiAuth> {
        let region = std::env::var("VERTEX_REGION")
            .ok()
            .and_then(|v| Self::normalize_non_empty(&v))
            .unwrap_or_else(|| "europe-west1".to_string());

        let json_str = Self::load_non_empty_env("VERTEX_SERVICE_ACCOUNT_JSON")
            .and_then(|b64| {
                base64::engine::general_purpose::STANDARD
                    .decode(&b64)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            })
            .or_else(|| {
                let path = Self::load_non_empty_env("GOOGLE_APPLICATION_CREDENTIALS")?;
                std::fs::read_to_string(&path)
                    .map_err(|e| {
                        tracing::debug!("GOOGLE_APPLICATION_CREDENTIALS read failed: {e}");
                        e
                    })
                    .ok()
            })?;

        // Only proceed if it's actually a service_account type
        let check: serde_json::Value = serde_json::from_str(&json_str).ok()?;
        if check.get("type").and_then(|v| v.as_str()) != Some("service_account") {
            return None;
        }

        match Self::parse_vertex_service_account_json(&json_str, &region) {
            Ok(creds) => {
                tracing::info!(
                    project_id = %creds.project_id,
                    region = %creds.region,
                    "Gemini provider using Vertex AI service account"
                );
                Some(GeminiAuth::VertexServiceAccount {
                    creds: Arc::new(creds),
                    token_state: Arc::new(tokio::sync::Mutex::new(None)),
                })
            }
            Err(e) => {
                tracing::warn!("Failed to load Vertex AI service account: {e}");
                None
            }
        }
    }

    /// Get authentication source description for diagnostics.
    /// Uses the stored enum variant — no env var re-reading at call time.
    pub fn auth_source(&self) -> &'static str {
        match self.auth.as_ref() {
            Some(GeminiAuth::ExplicitKey(_)) => "config",
            Some(GeminiAuth::EnvGeminiKey(_)) => "GEMINI_API_KEY env var",
            Some(GeminiAuth::EnvGoogleKey(_)) => "GOOGLE_API_KEY env var",
            Some(GeminiAuth::OAuthToken(_)) => "Gemini CLI OAuth",
            Some(GeminiAuth::ManagedOAuth) => "auth-profiles",
            Some(GeminiAuth::VertexServiceAccount { .. }) => "Vertex AI service account",
            None => "none",
        }
    }

    /// Get a valid OAuth access token, refreshing if expired.
    /// Adds a 60-second buffer before actual expiry to avoid edge-case failures.
    async fn get_valid_oauth_token(
        state: &Arc<tokio::sync::Mutex<OAuthTokenState>>,
    ) -> anyhow::Result<String> {
        let mut guard = state.lock().await;

        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(i64::MAX);

        // Refresh if expiry is unknown, already expired, or within 60s of expiry.
        let needs_refresh = guard
            .expiry_millis
            .map_or(true, |exp| exp <= now_millis.saturating_add(60_000));

        if needs_refresh {
            if let Some(ref refresh_token) = guard.refresh_token {
                let refreshed = refresh_gemini_cli_token_async(
                    refresh_token,
                    guard.client_id.as_deref(),
                    guard.client_secret.as_deref(),
                )
                .await?;
                tracing::info!("Gemini CLI OAuth token refreshed successfully (runtime)");
                guard.access_token = refreshed.access_token;
                guard.expiry_millis = refreshed.expiry_millis;
            } else {
                anyhow::bail!(
                    "Gemini CLI OAuth token expired and no refresh_token available — re-run `gemini` to authenticate"
                );
            }
        }

        Ok(guard.access_token.clone())
    }

    /// Rotate to the next available OAuth credentials file and swap state.
    /// Returns `true` when rotation succeeded.
    async fn rotate_oauth_credential(
        &self,
        state: &Arc<tokio::sync::Mutex<OAuthTokenState>>,
    ) -> bool {
        if self.oauth_cred_paths.len() <= 1 {
            return false;
        }

        let mut idx = self.oauth_index.lock().await;
        let start = *idx;

        loop {
            let next = (*idx + 1) % self.oauth_cred_paths.len();
            *idx = next;

            if next == start {
                return false;
            }

            if let Some(next_state) =
                Self::try_load_gemini_cli_token(self.oauth_cred_paths.get(next))
            {
                {
                    let mut guard = state.lock().await;
                    *guard = next_state;
                }
                {
                    let mut cached_project = self.oauth_project.lock().await;
                    *cached_project = None;
                }
                tracing::warn!(
                    "Gemini OAuth: rotated credential to {}",
                    self.oauth_cred_paths[next].display()
                );
                return true;
            }
        }
    }

    fn format_model_name(model: &str) -> String {
        if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        }
    }

    fn format_internal_model_name(model: &str) -> String {
        model.strip_prefix("models/").unwrap_or(model).to_string()
    }

    /// Build the API URL based on auth type.
    ///
    /// - API key users → public `generativelanguage.googleapis.com/v1beta`
    /// - OAuth users → internal `cloudcode-pa.googleapis.com/v1internal`
    ///
    /// The Gemini CLI OAuth tokens are scoped for the internal Code Assist API,
    /// not the public API. Sending them to the public endpoint results in
    /// "400 Bad Request: API key not valid" errors.
    /// See: https://github.com/google-gemini/gemini-cli/issues/19200
    fn build_generate_content_url(model: &str, auth: &GeminiAuth) -> String {
        match auth {
            GeminiAuth::OAuthToken(_) | GeminiAuth::ManagedOAuth => {
                // OAuth tokens are scoped for the internal Code Assist API.
                // The model is passed in the request body, not the URL path.
                format!("{CLOUDCODE_PA_ENDPOINT}:generateContent")
            }
            GeminiAuth::VertexServiceAccount { ref creds, .. } => {
                let model_id = model.strip_prefix("models/").unwrap_or(model);
                if creds.region == "global" {
                    format!(
                        "https://aiplatform.googleapis.com/v1/projects/{project}/locations/global/publishers/google/models/{model}:generateContent",
                        project = creds.project_id,
                        model = model_id,
                    )
                } else {
                    format!(
                        "https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:generateContent",
                        region = creds.region,
                        project = creds.project_id,
                        model = model_id,
                    )
                }
            }
            _ => {
                let model_name = Self::format_model_name(model);
                let base_url = format!("{PUBLIC_API_ENDPOINT}/{model_name}:generateContent");

                if auth.is_api_key() {
                    format!("{base_url}?key={}", auth.api_key_credential())
                } else {
                    base_url
                }
            }
        }
    }

    fn http_client(&self) -> Client {
        crate::config::build_runtime_proxy_client_with_timeouts("provider.gemini", 120, 10)
    }

    /// Resolve the GCP project ID for OAuth by calling the loadCodeAssist endpoint.
    /// Caches the result for subsequent calls.
    async fn resolve_oauth_project(&self, token: &str) -> anyhow::Result<String> {
        let project_seed = Self::load_non_empty_env("GOOGLE_CLOUD_PROJECT")
            .or_else(|| Self::load_non_empty_env("GOOGLE_CLOUD_PROJECT_ID"));
        let project_seed_for_request = project_seed.clone();
        let duet_project_for_request = project_seed.clone();

        // Check cache first
        {
            let cached = self.oauth_project.lock().await;
            if let Some(ref project) = *cached {
                return Ok(project.clone());
            }
        }

        // Call loadCodeAssist
        let client = self.http_client();
        let response = client
            .post(LOAD_CODE_ASSIST_ENDPOINT)
            .bearer_auth(token)
            .json(&serde_json::json!({
                "cloudaicompanionProject": project_seed_for_request,
                "metadata": {
                    "ideType": "GEMINI_CLI",
                    "platform": "PLATFORM_UNSPECIFIED",
                    "pluginType": "GEMINI",
                    "duetProject": duet_project_for_request,
                }
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if let Some(seed) = project_seed {
                tracing::warn!(
                    "loadCodeAssist failed (HTTP {status}); using GOOGLE_CLOUD_PROJECT fallback"
                );
                return Ok(seed);
            }
            anyhow::bail!("loadCodeAssist failed (HTTP {status}): {body}");
        }

        #[derive(Deserialize)]
        struct LoadCodeAssistResponse {
            #[serde(rename = "cloudaicompanionProject")]
            cloudaicompanion_project: Option<String>,
        }

        let result: LoadCodeAssistResponse = response.json().await?;
        let project = result
            .cloudaicompanion_project
            .filter(|p| !p.trim().is_empty())
            .or(project_seed)
            .ok_or_else(|| anyhow::anyhow!("loadCodeAssist response missing project context"))?;

        // Cache for future calls
        {
            let mut cached = self.oauth_project.lock().await;
            *cached = Some(project.clone());
        }

        Ok(project)
    }

    /// Build the HTTP request for generateContent.
    ///
    /// For OAuth, pass the resolved `oauth_token` and `project`.
    /// For API key, both are `None`.
    fn build_generate_content_request(
        &self,
        auth: &GeminiAuth,
        url: &str,
        request: &GenerateContentRequest,
        model: &str,
        include_generation_config: bool,
        project: Option<&str>,
        oauth_token: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let req = self.http_client().post(url).json(request);
        match auth {
            GeminiAuth::OAuthToken(_) | GeminiAuth::ManagedOAuth => {
                let token = oauth_token.unwrap_or_default();
                // Internal Code Assist API uses a wrapped payload shape:
                // { model, project?, user_prompt_id?, request: { contents, systemInstruction?, generationConfig } }
                let internal_request = InternalGenerateContentEnvelope {
                    model: Self::format_internal_model_name(model),
                    project: project.map(|value| value.to_string()),
                    user_prompt_id: Some(uuid::Uuid::new_v4().to_string()),
                    request: InternalGenerateContentRequest {
                        contents: request.contents.clone(),
                        system_instruction: request.system_instruction.clone(),
                        generation_config: if include_generation_config {
                            Some(request.generation_config.clone())
                        } else {
                            None
                        },
                        tools: request.tools.clone(),
                        tool_config: request.tool_config.clone(),
                    },
                };
                self.http_client()
                    .post(url)
                    .json(&internal_request)
                    .bearer_auth(token)
            }
            GeminiAuth::VertexServiceAccount { .. } => {
                // Vertex AI uses the same flat request body as API key auth,
                // but authenticates via Bearer token instead of ?key= query param.
                let token = oauth_token.unwrap_or_default();
                self.http_client()
                    .post(url)
                    .json(request)
                    .bearer_auth(token)
            }
            _ => req,
        }
    }

    fn should_retry_oauth_without_generation_config(
        status: reqwest::StatusCode,
        error_text: &str,
    ) -> bool {
        if status != reqwest::StatusCode::BAD_REQUEST {
            return false;
        }

        error_text.contains("Unknown name \"generationConfig\"")
            || error_text.contains("Unknown name 'generationConfig'")
            || error_text.contains(r#"Unknown name \"generationConfig\""#)
    }

    fn should_rotate_oauth_on_error(status: reqwest::StatusCode, error_text: &str) -> bool {
        status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
            || status.is_server_error()
            || error_text.contains("RESOURCE_EXHAUSTED")
    }

    /// Build a self-signed JWT for Vertex AI service account token exchange.
    fn build_vertex_jwt(creds: &VertexServiceAccountCreds) -> anyhow::Result<String> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("system clock error: {e}"))?
            .as_secs() as i64;

        let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
        let claims = serde_json::json!({
            "iss": creds.client_email,
            "scope": "https://www.googleapis.com/auth/cloud-platform",
            "aud": GOOGLE_TOKEN_ENDPOINT,
            "iat": now_secs,
            "exp": now_secs + 3600,
        });

        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header_b64 = b64.encode(serde_json::to_vec(&header)?);
        let claims_b64 = b64.encode(serde_json::to_vec(&claims)?);
        let signing_input = format!("{header_b64}.{claims_b64}");

        let rng = ring::rand::SystemRandom::new();
        let mut signature = vec![0u8; creds.key_pair.public().modulus_len()];
        creds
            .key_pair
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &rng,
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| anyhow::anyhow!("RSA signing failed"))?;

        let signature_b64 = b64.encode(&signature);
        Ok(format!("{signing_input}.{signature_b64}"))
    }

    /// Get a valid Vertex AI access token, acquiring or refreshing via JWT grant as needed.
    async fn get_valid_vertex_token(
        creds: &VertexServiceAccountCreds,
        token_state: &Arc<tokio::sync::Mutex<Option<VertexTokenState>>>,
    ) -> anyhow::Result<String> {
        let mut guard = token_state.lock().await;

        let now_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(i64::MAX);

        // Reuse cached token if still valid (with 60s buffer)
        if let Some(ref state) = *guard {
            if state.expiry_millis > now_millis.saturating_add(60_000) {
                return Ok(state.access_token.clone());
            }
        }

        // Build and sign JWT
        let jwt = Self::build_vertex_jwt(creds)?;

        // Exchange JWT for access token (blocking HTTP in spawn_blocking)
        let token_result = tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new());

            let response = client
                .post(GOOGLE_TOKEN_ENDPOINT)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    ("assertion", &jwt),
                ])
                .send()
                .map_err(|e| anyhow::anyhow!("Vertex AI token request failed: {e}"))?;

            let status = response.status();
            let body = response.text().unwrap_or_default();

            if !status.is_success() {
                anyhow::bail!("Vertex AI token exchange failed (HTTP {status}): {body}");
            }

            #[derive(serde::Deserialize)]
            struct TokenResponse {
                access_token: Option<String>,
                expires_in: Option<i64>,
            }

            let parsed: TokenResponse = serde_json::from_str(&body)
                .map_err(|_| anyhow::anyhow!("Vertex AI token response is not valid JSON"))?;

            let access_token = parsed
                .access_token
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("Vertex AI token response missing access_token"))?;

            let expiry_millis = parsed.expires_in.and_then(|secs| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|d| i64::try_from(d.as_millis()).ok())?;
                now.checked_add(secs.checked_mul(1000)?)
            });

            Ok((access_token, expiry_millis))
        })
        .await
        .map_err(|e| anyhow::anyhow!("Vertex AI token task panicked: {e}"))??;

        let (access_token, expiry_millis) = token_result;

        *guard = Some(VertexTokenState {
            access_token: access_token.clone(),
            expiry_millis: expiry_millis.unwrap_or(now_millis + 3_600_000),
        });

        Ok(access_token)
    }
}

impl GeminiProvider {
    async fn send_generate_content(
        &self,
        contents: Vec<Content>,
        system_instruction: Option<Content>,
        model: &str,
        temperature: f64,
        tools: Option<Vec<GeminiToolDeclaration>>,
        tool_config: Option<GeminiToolConfig>,
    ) -> anyhow::Result<GeminiResponse> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Gemini API key not found. Options:\n\
                 1. Set GEMINI_API_KEY env var\n\
                 2. Set GOOGLE_APPLICATION_CREDENTIALS to a service account JSON file (Vertex AI)\n\
                 3. Run `gemini` CLI to authenticate (tokens will be reused)\n\
                 4. Run `zeroclaw auth login --provider gemini`\n\
                 5. Get an API key from https://aistudio.google.com/app/apikey\n\
                 6. Run `zeroclaw onboard` to configure"
            )
        })?;

        let oauth_state = match auth {
            GeminiAuth::OAuthToken(state) => Some(state.clone()),
            _ => None,
        };

        // For OAuth: get a valid (potentially refreshed) token and resolve project
        let (mut oauth_token, mut project) = match auth {
            GeminiAuth::OAuthToken(state) => {
                let token = Self::get_valid_oauth_token(state).await?;
                let proj = self.resolve_oauth_project(&token).await?;
                (Some(token), Some(proj))
            }
            GeminiAuth::ManagedOAuth => {
                let auth_service = self
                    .auth_service
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("ManagedOAuth requires auth_service"))?;
                let token = auth_service
                    .get_valid_gemini_access_token(self.auth_profile_override.as_deref())
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Gemini auth profile not found. Run `zeroclaw auth login --provider gemini`."
                        )
                    })?;
                let proj = self.resolve_oauth_project(&token).await?;
                (Some(token), Some(proj))
            }
            GeminiAuth::VertexServiceAccount {
                ref creds,
                ref token_state,
            } => {
                let token = Self::get_valid_vertex_token(creds, token_state).await?;
                (Some(token), None)
            }
            _ => (None, None),
        };

        let request = GenerateContentRequest {
            contents,
            system_instruction,
            generation_config: GenerationConfig {
                temperature,
                max_output_tokens: 8192,
            },
            tools,
            tool_config,
        };

        let url = Self::build_generate_content_url(model, auth);

        let mut response = self
            .build_generate_content_request(
                auth,
                &url,
                &request,
                model,
                true,
                project.as_deref(),
                oauth_token.as_deref(),
            )
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();

            if auth.is_oauth() && Self::should_rotate_oauth_on_error(status, &error_text) {
                // For CLI OAuth: rotate credentials
                // For ManagedOAuth: AuthService handles refresh, just retry
                let can_retry = match auth {
                    GeminiAuth::OAuthToken(_) => {
                        if let Some(state) = oauth_state.as_ref() {
                            self.rotate_oauth_credential(state).await
                        } else {
                            false
                        }
                    }
                    GeminiAuth::ManagedOAuth => true, // AuthService refreshes automatically
                    _ => false,
                };

                if can_retry {
                    // Re-fetch token (may be refreshed)
                    let (new_token, new_project) = match auth {
                        GeminiAuth::OAuthToken(state) => {
                            let token = Self::get_valid_oauth_token(state).await?;
                            let proj = self.resolve_oauth_project(&token).await?;
                            (token, proj)
                        }
                        GeminiAuth::ManagedOAuth => {
                            let auth_service = self.auth_service.as_ref().unwrap();
                            let token = auth_service
                                .get_valid_gemini_access_token(
                                    self.auth_profile_override.as_deref(),
                                )
                                .await?
                                .ok_or_else(|| anyhow::anyhow!("Gemini auth profile not found"))?;
                            let proj = self.resolve_oauth_project(&token).await?;
                            (token, proj)
                        }
                        _ => unreachable!(),
                    };
                    oauth_token = Some(new_token);
                    project = Some(new_project);
                    response = self
                        .build_generate_content_request(
                            auth,
                            &url,
                            &request,
                            model,
                            true,
                            project.as_deref(),
                            oauth_token.as_deref(),
                        )
                        .send()
                        .await?;
                } else {
                    anyhow::bail!("Gemini API error ({status}): {error_text}");
                }
            } else if auth.is_oauth()
                && Self::should_retry_oauth_without_generation_config(status, &error_text)
            {
                tracing::warn!(
                    "Gemini OAuth internal endpoint rejected generationConfig; retrying without generationConfig"
                );
                response = self
                    .build_generate_content_request(
                        auth,
                        &url,
                        &request,
                        model,
                        false,
                        project.as_deref(),
                        oauth_token.as_deref(),
                    )
                    .send()
                    .await?;
            } else {
                anyhow::bail!("Gemini API error ({status}): {error_text}");
            }
        }

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            if auth.is_oauth()
                && Self::should_retry_oauth_without_generation_config(status, &error_text)
            {
                tracing::warn!(
                    "Gemini OAuth internal endpoint rejected generationConfig; retrying without generationConfig"
                );
                response = self
                    .build_generate_content_request(
                        auth,
                        &url,
                        &request,
                        model,
                        false,
                        project.as_deref(),
                        oauth_token.as_deref(),
                    )
                    .send()
                    .await?;
            } else {
                anyhow::bail!("Gemini API error ({status}): {error_text}");
            }
        }

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error ({status}): {error_text}");
        }

        let body_text = response.text().await?;
        let result: GenerateContentResponse = match serde_json::from_str(&body_text) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    body = &body_text[..body_text.len().min(500)],
                    "Gemini response deserialization failed"
                );
                anyhow::bail!("error decoding response body: {e}");
            }
        };
        if let Some(err) = &result.error {
            anyhow::bail!("Gemini API error: {}", err.message);
        }
        let result = result.into_effective_response();
        if let Some(err) = result.error {
            anyhow::bail!("Gemini API error: {}", err.message);
        }

        let usage = result.usage_metadata.map(|u| TokenUsage {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
        });

        let content = result
            .candidates
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.content);

        let (text, tool_calls, raw_parts) = match content {
            Some(c) => c.extract_response(),
            None => (None, Vec::new(), Vec::new()),
        };

        // When no tool calls and no text, report as error (mirrors old behavior)
        if text.is_none() && tool_calls.is_empty() {
            tracing::warn!(
                body = &body_text[..body_text.len().min(1000)],
                "Gemini returned no extractable text or tool calls"
            );
            anyhow::bail!("No response from Gemini");
        }

        Ok(GeminiResponse {
            text,
            tool_calls,
            raw_parts,
            usage,
        })
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
        }
    }

    fn convert_tools(&self, tools: &[crate::tools::ToolSpec]) -> ToolsPayload {
        ToolsPayload::Gemini {
            function_declarations: tools
                .iter()
                .map(|t| {
                    let mut params = t.parameters.clone();
                    sanitize_schema_for_gemini(&mut params);
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": params,
                    })
                })
                .collect(),
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let system_instruction = system_prompt.map(|sys| Content {
            role: None,
            parts: vec![Part {
                text: Some(sys.to_string()),
                ..Default::default()
            }],
        });

        let contents = vec![Content {
            role: Some("user".to_string()),
            parts: vec![Part {
                text: Some(message.to_string()),
                ..Default::default()
            }],
        }];

        let resp = self
            .send_generate_content(contents, system_instruction, model, temperature, None, None)
            .await?;
        Ok(resp.text.unwrap_or_default())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let messages = sanitize_transcript_for_gemini(messages);
        let mut system_parts: Vec<&str> = Vec::new();
        let mut contents: Vec<Content> = Vec::new();

        for msg in &messages {
            match msg.role.as_str() {
                "system" => {
                    system_parts.push(&msg.content);
                }
                "user" => {
                    contents.push(Content {
                        role: Some("user".to_string()),
                        parts: vec![Part {
                            text: Some(msg.content.clone()),
                            ..Default::default()
                        }],
                    });
                }
                "assistant" => {
                    // Gemini API uses "model" role instead of "assistant"
                    contents.push(Content {
                        role: Some("model".to_string()),
                        parts: vec![Part {
                            text: Some(msg.content.clone()),
                            ..Default::default()
                        }],
                    });
                }
                _ => {}
            }
        }

        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            Some(Content {
                role: None,
                parts: vec![Part {
                    text: Some(system_parts.join("\n\n")),
                    ..Default::default()
                }],
            })
        };

        let resp = self
            .send_generate_content(contents, system_instruction, model, temperature, None, None)
            .await?;
        Ok(resp.text.unwrap_or_default())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let sanitized_messages = sanitize_transcript_for_gemini(request.messages);
        let mut system_parts: Vec<&str> = Vec::new();
        let mut contents: Vec<Content> = Vec::new();
        // Map tool_call_id → function name so tool results use the correct name
        let mut tool_id_to_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for msg in &sanitized_messages {
            match msg.role.as_str() {
                "system" => system_parts.push(&msg.content),
                "user" => contents.push(Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: Some(msg.content.clone()),
                        ..Default::default()
                    }],
                }),
                "assistant" => {
                    // Check if this is a tool-call message (JSON with tool_calls field)
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                        // Use raw_model_parts for faithful replay when available
                        if let Some(raw_parts) =
                            parsed.get("raw_model_parts").and_then(|v| v.as_array())
                        {
                            let parts: Vec<Part> = raw_parts
                                .iter()
                                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                                .collect();
                            if !parts.is_empty() {
                                // Populate tool_id_to_name from tool_calls for result matching
                                if let Some(tcs) =
                                    parsed.get("tool_calls").and_then(|v| v.as_array())
                                {
                                    for tc in tcs {
                                        if let (Some(id), Some(name)) = (
                                            tc.get("id").and_then(|v| v.as_str()),
                                            tc.get("name").and_then(|v| v.as_str()),
                                        ) {
                                            tool_id_to_name
                                                .insert(id.to_string(), name.to_string());
                                        }
                                    }
                                }
                                contents.push(Content {
                                    role: Some("model".into()),
                                    parts,
                                });
                                continue;
                            }
                        }

                        // Fallback: reconstruct from tool_calls (non-thinking models, legacy)
                        if let Some(tool_calls) =
                            parsed.get("tool_calls").and_then(|tc| tc.as_array())
                        {
                            let mut parts: Vec<Part> = Vec::new();
                            // Preserve assistant text alongside tool calls
                            if let Some(text) = parsed.get("content").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    parts.push(Part {
                                        text: Some(text.to_string()),
                                        ..Default::default()
                                    });
                                }
                            }
                            for tc in tool_calls {
                                if let (Some(name), Some(args_str)) = (
                                    tc.get("name").and_then(|v| v.as_str()),
                                    tc.get("arguments").and_then(|v| v.as_str()),
                                ) {
                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                        tool_id_to_name.insert(id.to_string(), name.to_string());
                                    }
                                    if let Ok(args) = serde_json::from_str(args_str) {
                                        parts.push(Part {
                                            function_call: Some(FunctionCallPart {
                                                name: name.to_string(),
                                                args,
                                            }),
                                            // thoughtSignature goes on the part; thought is only for text parts
                                            thought_signature: tc
                                                .get("thought_signature")
                                                .and_then(|v| v.as_str())
                                                .map(String::from),
                                            ..Default::default()
                                        });
                                    }
                                }
                            }
                            if !parts.is_empty() {
                                contents.push(Content {
                                    role: Some("model".into()),
                                    parts,
                                });
                                continue;
                            }
                        }
                    }
                    // Fallback: plain text assistant message
                    contents.push(Content {
                        role: Some("model".to_string()),
                        parts: vec![Part {
                            text: Some(msg.content.clone()),
                            ..Default::default()
                        }],
                    });
                }
                "tool" => {
                    // Convert tool result to Gemini functionResponse
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                        let tool_call_id = parsed
                            .get("tool_call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let fn_name = tool_id_to_name
                            .get(tool_call_id)
                            .cloned()
                            .unwrap_or_else(|| tool_call_id.to_string());
                        // Try to parse content as JSON; fall back to string wrapper
                        let response_value = match parsed.get("content") {
                            Some(v) if v.is_object() => v.clone(),
                            Some(v) => match v
                                .as_str()
                                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                            {
                                Some(obj) if obj.is_object() => obj,
                                _ => serde_json::json!({"output": v}),
                            },
                            None => serde_json::json!({"output": ""}),
                        };
                        let part = Part {
                            function_response: Some(FunctionResponsePart {
                                name: fn_name,
                                response: response_value,
                            }),
                            ..Default::default()
                        };
                        // Merge consecutive tool results into one Content to maintain
                        // strict role alternation (Gemini requires user/model/user/model).
                        if let Some(last) = contents.last_mut() {
                            if last.role.as_deref() == Some("user")
                                && last.parts.iter().all(|p| p.function_response.is_some())
                            {
                                last.parts.push(part);
                                continue;
                            }
                        }
                        contents.push(Content {
                            role: Some("user".into()),
                            parts: vec![part],
                        });
                    } else {
                        let preview: String = msg.content.chars().take(200).collect();
                        tracing::warn!(
                            content_len = msg.content.len(),
                            content_preview = %preview,
                            "Failed to parse tool message as JSON -- tool result will be dropped"
                        );
                    }
                }
                _ => {}
            }
        }

        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            Some(Content {
                role: None,
                parts: vec![Part {
                    text: Some(system_parts.join("\n\n")),
                    ..Default::default()
                }],
            })
        };

        // Convert request.tools to Gemini functionDeclarations
        let gemini_tools = request.tools.and_then(|specs| {
            if specs.is_empty() {
                return None;
            }
            let decls: Vec<serde_json::Value> = specs
                .iter()
                .map(|t| {
                    let mut params = t.parameters.clone();
                    sanitize_schema_for_gemini(&mut params);
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": params,
                    })
                })
                .collect();
            Some(vec![GeminiToolDeclaration {
                function_declarations: decls,
            }])
        });

        let tool_config = gemini_tools.as_ref().map(|_| GeminiToolConfig {
            function_calling_config: FunctionCallingConfigMode {
                mode: "AUTO".into(),
            },
        });

        let resp = self
            .send_generate_content(
                contents,
                system_instruction,
                model,
                temperature,
                gemini_tools,
                tool_config,
            )
            .await?;

        Ok(ChatResponse {
            text: resp.text,
            tool_calls: resp.tool_calls,
            usage: resp.usage,
            reasoning_content: None,
            provider_parts: if resp.raw_parts.is_empty() {
                None
            } else {
                Some(
                    resp.raw_parts
                        .iter()
                        .filter_map(|p| serde_json::to_value(p).ok())
                        .collect(),
                )
            },
        })
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let messages = sanitize_transcript_for_gemini(messages);
        let mut system_parts: Vec<&str> = Vec::new();
        let mut contents: Vec<Content> = Vec::new();
        // Map tool_call_id → function name so tool results use the correct name
        let mut tool_id_to_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for msg in &messages {
            match msg.role.as_str() {
                "system" => system_parts.push(&msg.content),
                "user" => contents.push(Content {
                    role: Some("user".into()),
                    parts: vec![Part {
                        text: Some(msg.content.clone()),
                        ..Default::default()
                    }],
                }),
                "assistant" => {
                    // Check if this is a tool-call message (JSON with tool_calls field)
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                        // Use raw_model_parts for faithful replay when available
                        if let Some(raw_parts) =
                            parsed.get("raw_model_parts").and_then(|v| v.as_array())
                        {
                            let parts: Vec<Part> = raw_parts
                                .iter()
                                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                                .collect();
                            if !parts.is_empty() {
                                // Populate tool_id_to_name from tool_calls for result matching
                                if let Some(tcs) =
                                    parsed.get("tool_calls").and_then(|v| v.as_array())
                                {
                                    for tc in tcs {
                                        if let (Some(id), Some(name)) = (
                                            tc.get("id").and_then(|v| v.as_str()),
                                            tc.get("name").and_then(|v| v.as_str()),
                                        ) {
                                            tool_id_to_name
                                                .insert(id.to_string(), name.to_string());
                                        }
                                    }
                                }
                                contents.push(Content {
                                    role: Some("model".into()),
                                    parts,
                                });
                                continue;
                            }
                        }

                        // Fallback: reconstruct from tool_calls (non-thinking models, legacy)
                        if let Some(tool_calls) =
                            parsed.get("tool_calls").and_then(|tc| tc.as_array())
                        {
                            let mut parts: Vec<Part> = Vec::new();
                            // Preserve assistant text alongside tool calls
                            if let Some(text) = parsed.get("content").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    parts.push(Part {
                                        text: Some(text.to_string()),
                                        ..Default::default()
                                    });
                                }
                            }
                            for tc in tool_calls {
                                if let (Some(name), Some(args_str)) = (
                                    tc.get("name").and_then(|v| v.as_str()),
                                    tc.get("arguments").and_then(|v| v.as_str()),
                                ) {
                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                        tool_id_to_name.insert(id.to_string(), name.to_string());
                                    }
                                    if let Ok(args) = serde_json::from_str(args_str) {
                                        parts.push(Part {
                                            function_call: Some(FunctionCallPart {
                                                name: name.to_string(),
                                                args,
                                            }),
                                            // thoughtSignature goes on the part; thought is only for text parts
                                            thought_signature: tc
                                                .get("thought_signature")
                                                .and_then(|v| v.as_str())
                                                .map(String::from),
                                            ..Default::default()
                                        });
                                    }
                                }
                            }
                            if !parts.is_empty() {
                                contents.push(Content {
                                    role: Some("model".into()),
                                    parts,
                                });
                                continue;
                            }
                        }
                    }
                    contents.push(Content {
                        role: Some("model".into()),
                        parts: vec![Part {
                            text: Some(msg.content.clone()),
                            ..Default::default()
                        }],
                    });
                }
                "tool" => {
                    // Parse tool result and convert to functionResponse
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                        let tool_call_id = parsed
                            .get("tool_call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let fn_name = tool_id_to_name
                            .get(tool_call_id)
                            .cloned()
                            .unwrap_or_else(|| tool_call_id.to_string());
                        // Try to parse content as JSON; fall back to string wrapper
                        let response_value = match parsed.get("content") {
                            Some(v) if v.is_object() => v.clone(),
                            Some(v) => match v
                                .as_str()
                                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                            {
                                Some(obj) if obj.is_object() => obj,
                                _ => serde_json::json!({"output": v}),
                            },
                            None => serde_json::json!({"output": ""}),
                        };
                        let part = Part {
                            function_response: Some(FunctionResponsePart {
                                name: fn_name,
                                response: response_value,
                            }),
                            ..Default::default()
                        };
                        // Merge consecutive tool results into one Content to maintain
                        // strict role alternation (Gemini requires user/model/user/model).
                        if let Some(last) = contents.last_mut() {
                            if last.role.as_deref() == Some("user")
                                && last.parts.iter().all(|p| p.function_response.is_some())
                            {
                                last.parts.push(part);
                                continue;
                            }
                        }
                        contents.push(Content {
                            role: Some("user".into()),
                            parts: vec![part],
                        });
                    }
                }
                _ => {}
            }
        }

        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            Some(Content {
                role: None,
                parts: vec![Part {
                    text: Some(system_parts.join("\n\n")),
                    ..Default::default()
                }],
            })
        };

        let gemini_tools = if tools.is_empty() {
            None
        } else {
            Some(vec![GeminiToolDeclaration {
                function_declarations: tools.to_vec(),
            }])
        };

        let tool_config = if gemini_tools.is_some() {
            Some(GeminiToolConfig {
                function_calling_config: FunctionCallingConfigMode {
                    mode: "AUTO".into(),
                },
            })
        } else {
            None
        };

        let resp = self
            .send_generate_content(
                contents,
                system_instruction,
                model,
                temperature,
                gemini_tools,
                tool_config,
            )
            .await?;

        Ok(ChatResponse {
            text: resp.text,
            tool_calls: resp.tool_calls,
            usage: resp.usage,
            reasoning_content: None,
            provider_parts: if resp.raw_parts.is_empty() {
                None
            } else {
                Some(
                    resp.raw_parts
                        .iter()
                        .filter_map(|p| serde_json::to_value(p).ok())
                        .collect(),
                )
            },
        })
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        if let Some(auth) = self.auth.as_ref() {
            match auth {
                GeminiAuth::ManagedOAuth => {
                    // For ManagedOAuth, verify and refresh the token if needed.
                    // This ensures fallback works even if tokens expired during daemon uptime.
                    let auth_service = self
                        .auth_service
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("ManagedOAuth requires auth_service"))?;

                    let _token = auth_service
                        .get_valid_gemini_access_token(self.auth_profile_override.as_deref())
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Gemini auth profile not found or expired. Run: zeroclaw auth login --provider gemini"
                            )
                        })?;

                    // Token refresh happens in get_valid_gemini_access_token().
                    // We don't call resolve_oauth_project() here to keep warmup fast.
                    // OAuth project will be resolved lazily on first real request.
                }
                GeminiAuth::OAuthToken(_) => {
                    // CLI OAuth — cloudcode-pa does not expose a lightweight model-list probe.
                    // Token will be validated on first real request.
                }
                GeminiAuth::VertexServiceAccount { ref creds, .. } => {
                    tracing::info!(
                        project_id = %creds.project_id,
                        region = %creds.region,
                        "Gemini provider: Vertex AI service account ready"
                    );
                }
                _ => {
                    // API key path — verify with public API models endpoint.
                    let url = if auth.is_api_key() {
                        format!(
                            "https://generativelanguage.googleapis.com/v1beta/models?key={}",
                            auth.api_key_credential()
                        )
                    } else {
                        "https://generativelanguage.googleapis.com/v1beta/models".to_string()
                    };

                    self.http_client()
                        .get(&url)
                        .send()
                        .await?
                        .error_for_status()?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{header::AUTHORIZATION, StatusCode};

    /// Helper to create a test Vertex auth variant with a test RSA key.
    fn test_vertex_auth() -> GeminiAuth {
        let pem_bytes = include_bytes!("../../tests/fixtures/test_rsa_private_key.pem");
        let parsed = pem::parse(pem_bytes).expect("test PEM parse");
        let key_pair = match parsed.tag() {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(parsed.contents()),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(parsed.contents()),
            other => panic!("unexpected PEM tag: {other}"),
        }
        .expect("test RSA key parse");
        GeminiAuth::VertexServiceAccount {
            creds: Arc::new(VertexServiceAccountCreds {
                client_email: "test@test-project.iam.gserviceaccount.com".into(),
                key_pair: Arc::new(key_pair),
                project_id: "test-project".into(),
                region: "europe-west1".into(),
            }),
            token_state: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Helper to create a test OAuth auth variant.
    fn test_oauth_auth(token: &str) -> GeminiAuth {
        GeminiAuth::OAuthToken(Arc::new(tokio::sync::Mutex::new(OAuthTokenState {
            access_token: token.to_string(),
            refresh_token: None,
            client_id: None,
            client_secret: None,
            expiry_millis: None,
        })))
    }

    fn test_provider(auth: Option<GeminiAuth>) -> GeminiProvider {
        GeminiProvider {
            auth,
            oauth_project: Arc::new(tokio::sync::Mutex::new(None)),
            oauth_cred_paths: Vec::new(),
            oauth_index: Arc::new(tokio::sync::Mutex::new(0)),
            auth_service: None,
            auth_profile_override: None,
        }
    }

    #[test]
    fn normalize_non_empty_trims_and_filters() {
        assert_eq!(
            GeminiProvider::normalize_non_empty(" value "),
            Some("value".into())
        );
        assert_eq!(GeminiProvider::normalize_non_empty(""), None);
        assert_eq!(GeminiProvider::normalize_non_empty(" \t\n"), None);
    }

    #[test]
    fn oauth_refresh_form_uses_provided_client_credentials() {
        let form = build_oauth_refresh_form("refresh-token", Some("client-id"), Some("secret"));
        let map: std::collections::HashMap<_, _> = form.into_iter().collect();
        assert_eq!(map.get("grant_type"), Some(&"refresh_token".to_string()));
        assert_eq!(map.get("refresh_token"), Some(&"refresh-token".to_string()));
        assert_eq!(map.get("client_id"), Some(&"client-id".to_string()));
        assert_eq!(map.get("client_secret"), Some(&"secret".to_string()));
    }

    #[test]
    fn oauth_refresh_form_omits_client_credentials_when_missing() {
        let form = build_oauth_refresh_form("refresh-token", None, None);
        let map: std::collections::HashMap<_, _> = form.into_iter().collect();
        assert!(!map.contains_key("client_id"));
        assert!(!map.contains_key("client_secret"));
    }

    #[test]
    fn extract_client_id_from_id_token_prefers_aud_claim() {
        let payload = serde_json::json!({
            "aud": "aud-client-id",
            "azp": "azp-client-id"
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let token = format!("header.{payload_b64}.sig");

        assert_eq!(
            extract_client_id_from_id_token(&token),
            Some("aud-client-id".to_string())
        );
    }

    #[test]
    fn extract_client_id_from_id_token_uses_azp_when_aud_missing() {
        let payload = serde_json::json!({
            "azp": "azp-client-id"
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let token = format!("header.{payload_b64}.sig");

        assert_eq!(
            extract_client_id_from_id_token(&token),
            Some("azp-client-id".to_string())
        );
    }

    #[test]
    fn extract_client_id_from_id_token_returns_none_for_invalid_tokens() {
        assert_eq!(extract_client_id_from_id_token("invalid"), None);
        assert_eq!(extract_client_id_from_id_token("a.b.c"), None);
    }

    #[test]
    fn try_load_cli_token_derives_client_id_from_id_token_when_missing() {
        let payload = serde_json::json!({ "aud": "derived-client-id" });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let id_token = format!("header.{payload_b64}.sig");

        let file = tempfile::NamedTempFile::new().unwrap();
        let json = format!(
            r#"{{
                "access_token": "ya29.test-access",
                "refresh_token": "1//test-refresh",
                "id_token": "{id_token}"
            }}"#
        );
        std::fs::write(file.path(), json).unwrap();

        let path = file.path().to_path_buf();
        let state = GeminiProvider::try_load_gemini_cli_token(Some(&path)).unwrap();
        assert_eq!(state.client_id.as_deref(), Some("derived-client-id"));
        assert_eq!(state.client_secret, None);
    }

    #[test]
    fn provider_creates_without_key() {
        let provider = GeminiProvider::new(None);
        // May pick up env vars; just verify it doesn't panic
        let _ = provider.auth_source();
    }

    #[test]
    fn provider_creates_with_key() {
        let provider = GeminiProvider::new(Some("test-api-key"));
        assert!(matches!(
            provider.auth,
            Some(GeminiAuth::ExplicitKey(ref key)) if key == "test-api-key"
        ));
    }

    #[test]
    fn provider_rejects_empty_key() {
        let provider = GeminiProvider::new(Some(""));
        assert!(!matches!(provider.auth, Some(GeminiAuth::ExplicitKey(_))));
    }

    #[test]
    fn gemini_cli_dir_returns_path() {
        let dir = GeminiProvider::gemini_cli_dir();
        // Should return Some on systems with home dir
        if UserDirs::new().is_some() {
            assert!(dir.is_some());
            assert!(dir.unwrap().ends_with(".gemini"));
        }
    }

    #[test]
    fn auth_source_explicit_key() {
        let provider = test_provider(Some(GeminiAuth::ExplicitKey("key".into())));
        assert_eq!(provider.auth_source(), "config");
    }

    #[test]
    fn auth_source_none_without_credentials() {
        let provider = test_provider(None);
        assert_eq!(provider.auth_source(), "none");
    }

    #[test]
    fn auth_source_oauth() {
        let provider = test_provider(Some(test_oauth_auth("ya29.mock")));
        assert_eq!(provider.auth_source(), "Gemini CLI OAuth");
    }

    #[test]
    fn auth_source_vertex() {
        let provider = test_provider(Some(test_vertex_auth()));
        assert_eq!(provider.auth_source(), "Vertex AI service account");
    }

    #[test]
    fn vertex_auth_is_not_api_key() {
        let auth = test_vertex_auth();
        assert!(!auth.is_api_key());
    }

    #[test]
    fn vertex_auth_is_not_oauth() {
        let auth = test_vertex_auth();
        assert!(!auth.is_oauth());
    }

    #[test]
    fn explicit_key_takes_priority_over_vertex_env() {
        // Even if GOOGLE_APPLICATION_CREDENTIALS is set,
        // an explicit key should win
        let provider = GeminiProvider::new(Some("explicit-key"));
        assert!(matches!(
            provider.auth,
            Some(GeminiAuth::ExplicitKey(ref key)) if key == "explicit-key"
        ));
    }

    #[test]
    fn model_name_formatting() {
        assert_eq!(
            GeminiProvider::format_model_name("gemini-2.0-flash"),
            "models/gemini-2.0-flash"
        );
        assert_eq!(
            GeminiProvider::format_model_name("models/gemini-1.5-pro"),
            "models/gemini-1.5-pro"
        );
        assert_eq!(
            GeminiProvider::format_internal_model_name("models/gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
        assert_eq!(
            GeminiProvider::format_internal_model_name("gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
    }

    #[test]
    fn api_key_url_includes_key_query_param() {
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.contains(":generateContent?key=api-key-123"));
    }

    #[test]
    fn oauth_url_uses_internal_endpoint() {
        let auth = test_oauth_auth("ya29.test-token");
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.starts_with("https://cloudcode-pa.googleapis.com/v1internal"));
        assert!(url.ends_with(":generateContent"));
        assert!(!url.contains("generativelanguage.googleapis.com"));
        assert!(!url.contains("?key="));
    }

    #[test]
    fn api_key_url_uses_public_endpoint() {
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        assert!(url.contains("generativelanguage.googleapis.com/v1beta"));
        assert!(url.contains("models/gemini-2.0-flash"));
    }

    #[test]
    fn oauth_request_uses_bearer_auth_header() {
        let provider = test_provider(Some(test_oauth_auth("ya29.mock-token")));
        let auth = test_oauth_auth("ya29.mock-token");
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: Some("hello".into()),
                    ..Default::default()
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
            tools: None,
            tool_config: None,
        };

        let request = provider
            .build_generate_content_request(
                &auth,
                &url,
                &body,
                "gemini-2.0-flash",
                true,
                Some("test-project"),
                Some("ya29.mock-token"),
            )
            .build()
            .unwrap();

        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|h| h.to_str().ok()),
            Some("Bearer ya29.mock-token")
        );
    }

    #[test]
    fn oauth_request_wraps_payload_in_request_envelope() {
        let provider = test_provider(Some(test_oauth_auth("ya29.mock-token")));
        let auth = test_oauth_auth("ya29.mock-token");
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: Some("hello".into()),
                    ..Default::default()
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
            tools: None,
            tool_config: None,
        };

        let request = provider
            .build_generate_content_request(
                &auth,
                &url,
                &body,
                "models/gemini-2.0-flash",
                true,
                Some("test-project"),
                Some("ya29.mock-token"),
            )
            .build()
            .unwrap();

        let payload = request
            .body()
            .and_then(|b| b.as_bytes())
            .expect("json request body should be bytes");
        let json: serde_json::Value = serde_json::from_slice(payload).unwrap();

        assert_eq!(json["model"], "gemini-2.0-flash");
        assert!(json.get("generationConfig").is_none());
        assert!(json.get("request").is_some());
        assert!(json["request"].get("generationConfig").is_some());
    }

    #[test]
    fn api_key_request_does_not_set_bearer_header() {
        let provider = test_provider(Some(GeminiAuth::ExplicitKey("api-key-123".into())));
        let auth = GeminiAuth::ExplicitKey("api-key-123".into());
        let url = GeminiProvider::build_generate_content_url("gemini-2.0-flash", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: Some("hello".into()),
                    ..Default::default()
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
            tools: None,
            tool_config: None,
        };

        let request = provider
            .build_generate_content_request(
                &auth,
                &url,
                &body,
                "gemini-2.0-flash",
                true,
                None,
                None,
            )
            .build()
            .unwrap();

        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn request_serialization() {
        let request = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".to_string()),
                parts: vec![Part {
                    text: Some("Hello".to_string()),
                    ..Default::default()
                }],
            }],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part {
                    text: Some("You are helpful".to_string()),
                    ..Default::default()
                }],
            }),
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
            tools: None,
            tool_config: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"text\":\"Hello\""));
        assert!(json.contains("\"systemInstruction\""));
        assert!(!json.contains("\"system_instruction\""));
        assert!(json.contains("\"temperature\":0.7"));
        assert!(json.contains("\"maxOutputTokens\":8192"));
    }

    #[test]
    fn internal_request_includes_model() {
        let request = InternalGenerateContentEnvelope {
            model: "gemini-3-pro-preview".to_string(),
            project: Some("test-project".to_string()),
            user_prompt_id: Some("prompt-123".to_string()),
            request: InternalGenerateContentRequest {
                contents: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: Some("Hello".to_string()),
                        ..Default::default()
                    }],
                }],
                system_instruction: None,
                generation_config: Some(GenerationConfig {
                    temperature: 0.7,
                    max_output_tokens: 8192,
                }),
                tools: None,
                tool_config: None,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"model\":\"gemini-3-pro-preview\""));
        assert!(json.contains("\"request\""));
        assert!(json.contains("\"generationConfig\""));
        assert!(json.contains("\"maxOutputTokens\":8192"));
        assert!(json.contains("\"user_prompt_id\":\"prompt-123\""));
        assert!(json.contains("\"project\":\"test-project\""));
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"temperature\":0.7"));
    }

    #[test]
    fn internal_request_omits_generation_config_when_none() {
        let request = InternalGenerateContentEnvelope {
            model: "gemini-3-pro-preview".to_string(),
            project: Some("test-project".to_string()),
            user_prompt_id: None,
            request: InternalGenerateContentRequest {
                contents: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: Some("Hello".to_string()),
                        ..Default::default()
                    }],
                }],
                system_instruction: None,
                generation_config: None,
                tools: None,
                tool_config: None,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("generationConfig"));
        assert!(json.contains("\"model\":\"gemini-3-pro-preview\""));
    }

    #[test]
    fn internal_request_includes_project() {
        let request = InternalGenerateContentEnvelope {
            model: "gemini-2.5-flash".to_string(),
            project: Some("my-gcp-project-id".to_string()),
            user_prompt_id: None,
            request: InternalGenerateContentRequest {
                contents: vec![Content {
                    role: Some("user".to_string()),
                    parts: vec![Part {
                        text: Some("Hello".to_string()),
                        ..Default::default()
                    }],
                }],
                system_instruction: None,
                generation_config: None,
                tools: None,
                tool_config: None,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"project\":\"my-gcp-project-id\""));
    }

    #[test]
    fn internal_response_deserialize_nested() {
        let json = r#"{
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{"text": "Hello from internal API!"}]
                    }
                }]
            }
        }"#;

        let internal: InternalGenerateContentResponse = serde_json::from_str(json).unwrap();
        let text = internal
            .response
            .candidates
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .content
            .unwrap()
            .parts
            .into_iter()
            .next()
            .unwrap()
            .text;
        assert_eq!(text, Some("Hello from internal API!".to_string()));
    }

    #[test]
    fn creds_deserialize_with_expiry_date() {
        let json = r#"{
            "access_token": "ya29.test-token",
            "refresh_token": "1//test-refresh",
            "expiry_date": 4102444800000
        }"#;

        let creds: GeminiCliOAuthCreds = serde_json::from_str(json).unwrap();
        assert_eq!(creds.access_token.as_deref(), Some("ya29.test-token"));
        assert_eq!(creds.refresh_token.as_deref(), Some("1//test-refresh"));
        assert_eq!(creds.expiry_date, Some(4_102_444_800_000));
        assert!(creds.expiry.is_none());
    }

    #[test]
    fn creds_deserialize_accepts_camel_case_fields() {
        let json = r#"{
            "access_token": "ya29.test-token",
            "idToken": "header.payload.sig",
            "refresh_token": "1//test-refresh",
            "clientId": "test-client-id",
            "clientSecret": "test-client-secret",
            "expiryDate": 4102444800000
        }"#;

        let creds: GeminiCliOAuthCreds = serde_json::from_str(json).unwrap();
        assert_eq!(creds.id_token.as_deref(), Some("header.payload.sig"));
        assert_eq!(creds.client_id.as_deref(), Some("test-client-id"));
        assert_eq!(creds.client_secret.as_deref(), Some("test-client-secret"));
        assert_eq!(creds.expiry_date, Some(4_102_444_800_000));
    }

    #[test]
    fn oauth_retry_detection_for_generation_config_rejection() {
        // Bare quotes (e.g. pre-parsed error string)
        let err =
            "Invalid JSON payload received. Unknown name \"generationConfig\": Cannot find field.";
        assert!(
            GeminiProvider::should_retry_oauth_without_generation_config(
                StatusCode::BAD_REQUEST,
                err
            )
        );
        // JSON-escaped quotes (raw response body from Google API)
        let err_json = r#"Invalid JSON payload received. Unknown name \"generationConfig\": Cannot find field."#;
        assert!(
            GeminiProvider::should_retry_oauth_without_generation_config(
                StatusCode::BAD_REQUEST,
                err_json
            )
        );
        assert!(
            !GeminiProvider::should_retry_oauth_without_generation_config(
                StatusCode::UNAUTHORIZED,
                err
            )
        );
        assert!(
            !GeminiProvider::should_retry_oauth_without_generation_config(
                StatusCode::BAD_REQUEST,
                "something else"
            )
        );
    }

    #[test]
    fn response_deserialization() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello there!"}]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(response.candidates.is_some());
        let text = response
            .candidates
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .content
            .unwrap()
            .parts
            .into_iter()
            .next()
            .unwrap()
            .text;
        assert_eq!(text, Some("Hello there!".to_string()));
    }

    #[test]
    fn error_response_deserialization() {
        let json = r#"{
            "error": {
                "message": "Invalid API key"
            }
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().message, "Invalid API key");
    }

    #[test]
    fn internal_response_deserialization() {
        let json = r#"{
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{"text": "Hello from internal"}]
                    }
                }]
            }
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let text = response
            .into_effective_response()
            .candidates
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .content
            .unwrap()
            .parts
            .into_iter()
            .next()
            .unwrap()
            .text;
        assert_eq!(text, Some("Hello from internal".to_string()));
    }

    // ── Thinking model response tests ──────────────────────────────────────

    #[test]
    fn thinking_response_extracts_non_thinking_text() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"thought": true, "text": "Let me think about this..."},
                        {"text": "The answer is 42."},
                        {"thoughtSignature": "c2lnbmF0dXJl"}
                    ]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let candidate = response.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, Some("The answer is 42.".to_string()));
    }

    #[test]
    fn non_thinking_response_unaffected() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello there!"}]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let candidate = response.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, Some("Hello there!".to_string()));
    }

    #[test]
    fn thinking_only_response_falls_back_to_thinking_text() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"thought": true, "text": "I need more context..."},
                        {"thoughtSignature": "c2lnbmF0dXJl"}
                    ]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let candidate = response.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, Some("I need more context...".to_string()));
    }

    #[test]
    fn empty_parts_returns_none() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": []
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let candidate = response.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, None);
    }

    #[test]
    fn multiple_text_parts_concatenated() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Part one. "},
                        {"text": "Part two."}
                    ]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let candidate = response.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, Some("Part one. Part two.".to_string()));
    }

    #[test]
    fn thought_signature_only_parts_skipped() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"thoughtSignature": "c2lnbmF0dXJl"}
                    ]
                }
            }]
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let candidate = response.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, None);
    }

    #[test]
    fn internal_response_thinking_model() {
        let json = r#"{
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [
                            {"thought": true, "text": "reasoning..."},
                            {"text": "final answer"}
                        ]
                    }
                }]
            }
        }"#;

        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let effective = response.into_effective_response();
        let candidate = effective.candidates.unwrap().into_iter().next().unwrap();
        let text = candidate.content.unwrap().effective_text();
        assert_eq!(text, Some("final answer".to_string()));
    }

    #[tokio::test]
    async fn warmup_without_key_is_noop() {
        let provider = test_provider(None);
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn warmup_oauth_is_noop() {
        let provider = test_provider(Some(test_oauth_auth("ya29.mock-token")));
        let result = provider.warmup().await;
        assert!(result.is_ok());
    }

    #[test]
    fn discover_oauth_cred_paths_does_not_panic() {
        let _paths = GeminiProvider::discover_oauth_cred_paths();
    }

    #[tokio::test]
    async fn rotate_oauth_without_alternatives_returns_false() {
        let state = Arc::new(tokio::sync::Mutex::new(OAuthTokenState {
            access_token: "ya29.mock".to_string(),
            refresh_token: None,
            client_id: None,
            client_secret: None,
            expiry_millis: None,
        }));
        let provider = test_provider(Some(GeminiAuth::OAuthToken(state.clone())));
        assert!(!provider.rotate_oauth_credential(&state).await);
    }

    #[test]
    fn response_parses_usage_metadata() {
        let json = r#"{
            "candidates": [{"content": {"parts": [{"text": "Hello"}]}}],
            "usageMetadata": {"promptTokenCount": 120, "candidatesTokenCount": 40}
        }"#;
        let resp: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage_metadata.unwrap();
        assert_eq!(usage.prompt_token_count, Some(120));
        assert_eq!(usage.candidates_token_count, Some(40));
    }

    #[test]
    fn response_parses_without_usage_metadata() {
        let json = r#"{"candidates": [{"content": {"parts": [{"text": "Hello"}]}}]}"#;
        let resp: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage_metadata.is_none());
    }

    /// Validates that warmup() for ManagedOAuth requires auth_service.
    #[tokio::test]
    async fn warmup_managed_oauth_requires_auth_service() {
        let provider = GeminiProvider {
            auth: Some(GeminiAuth::ManagedOAuth),
            oauth_project: Arc::new(tokio::sync::Mutex::new(None)),
            oauth_cred_paths: Vec::new(),
            oauth_index: Arc::new(tokio::sync::Mutex::new(0)),
            auth_service: None, // Missing auth_service
            auth_profile_override: None,
        };

        let result = provider.warmup().await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("ManagedOAuth requires auth_service"));
    }

    /// Validates that warmup() for CLI OAuth skips validation (existing behavior).
    #[tokio::test]
    async fn warmup_cli_oauth_skips_validation() {
        let provider = test_provider(Some(test_oauth_auth("fake_token")));
        let result = provider.warmup().await;
        // Should succeed without making HTTP requests
        assert!(result.is_ok());
    }

    // ── Vertex AI service account credential loading tests ────────────────

    #[test]
    fn vertex_sa_json_parsing_valid() {
        let pem_str = include_str!("../../tests/fixtures/test_rsa_private_key.pem");
        let sa_json = serde_json::json!({
            "type": "service_account",
            "project_id": "my-project",
            "private_key": pem_str,
            "client_email": "test@my-project.iam.gserviceaccount.com"
        });
        let result =
            GeminiProvider::parse_vertex_service_account_json(&sa_json.to_string(), "europe-west1");
        assert!(result.is_ok());
        let creds = result.unwrap();
        assert_eq!(
            creds.client_email,
            "test@my-project.iam.gserviceaccount.com"
        );
        assert_eq!(creds.project_id, "my-project");
        assert_eq!(creds.region, "europe-west1");
    }

    #[test]
    fn vertex_sa_json_rejects_non_service_account() {
        let sa_json = serde_json::json!({
            "type": "authorized_user",
            "project_id": "my-project",
            "private_key": "-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----",
            "client_email": "test@my-project.iam.gserviceaccount.com"
        });
        let result =
            GeminiProvider::parse_vertex_service_account_json(&sa_json.to_string(), "us-central1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("service_account"));
    }

    #[test]
    fn vertex_sa_json_rejects_missing_fields() {
        let sa_json = serde_json::json!({
            "type": "service_account",
            "project_id": "my-project"
        });
        let result =
            GeminiProvider::parse_vertex_service_account_json(&sa_json.to_string(), "us-central1");
        assert!(result.is_err());
    }

    // GREEN: Vertex URL uses regional aiplatform endpoint with project/location.
    #[test]
    fn vertex_url_uses_regional_endpoint() {
        let auth = test_vertex_auth();
        let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
        assert_eq!(
            url,
            "https://europe-west1-aiplatform.googleapis.com/v1/projects/test-project/locations/europe-west1/publishers/google/models/gemini-3-flash-preview:generateContent"
        );
    }

    // RED: characterization test — documents existing models/ prefix-stripping contract.
    #[test]
    fn vertex_url_strips_models_prefix() {
        let auth = test_vertex_auth();
        let url =
            GeminiProvider::build_generate_content_url("models/gemini-3-flash-preview", &auth);
        assert!(url.contains("/models/gemini-3-flash-preview:"));
        assert!(!url.contains("/models/models/"));
    }

    // RED: characterization test — documents that Vertex URLs never include ?key= parameter.
    #[test]
    fn vertex_url_does_not_include_api_key() {
        let auth = test_vertex_auth();
        let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
        assert!(!url.contains("?key="));
    }

    #[test]
    fn vertex_url_uses_global_endpoint() {
        let pem_bytes = include_bytes!("../../tests/fixtures/test_rsa_private_key.pem");
        let parsed = pem::parse(pem_bytes).expect("test PEM parse");
        let key_pair = match parsed.tag() {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(parsed.contents()),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(parsed.contents()),
            other => panic!("unexpected PEM tag: {other}"),
        }
        .expect("test RSA key parse");
        let auth = GeminiAuth::VertexServiceAccount {
            creds: Arc::new(VertexServiceAccountCreds {
                client_email: "test@test-project.iam.gserviceaccount.com".into(),
                key_pair: Arc::new(key_pair),
                project_id: "test-project".into(),
                region: "global".into(),
            }),
            token_state: Arc::new(tokio::sync::Mutex::new(None)),
        };
        let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
        assert_eq!(
            url,
            "https://aiplatform.googleapis.com/v1/projects/test-project/locations/global/publishers/google/models/gemini-3-flash-preview:generateContent"
        );
    }

    // RED: build_generate_content_request has no VertexServiceAccount match arm;
    // the default arm sends a flat body WITHOUT Bearer auth.
    // Vertex needs Bearer auth with a flat body (not the cloudcode-pa envelope).
    #[test]
    fn vertex_request_uses_bearer_auth_and_flat_body() {
        let auth = test_vertex_auth();
        let provider = test_provider(Some(test_vertex_auth()));
        let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
        let body = GenerateContentRequest {
            contents: vec![Content {
                role: Some("user".into()),
                parts: vec![Part {
                    text: Some("hello".into()),
                    ..Default::default()
                }],
            }],
            system_instruction: None,
            generation_config: GenerationConfig {
                temperature: 0.7,
                max_output_tokens: 8192,
            },
            tools: None,
            tool_config: None,
        };

        let request = provider
            .build_generate_content_request(
                &auth,
                &url,
                &body,
                "gemini-3-flash-preview",
                true,
                None,
                Some("ya29.vertex-token"),
            )
            .build()
            .unwrap();

        // Should have Bearer auth
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok()),
            Some("Bearer ya29.vertex-token")
        );

        // Should use flat body (not cloudcode-pa envelope)
        let payload = request.body().and_then(|b| b.as_bytes()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(payload).unwrap();
        assert!(json.get("contents").is_some());
        assert!(json.get("generationConfig").is_some());
        // Should NOT have cloudcode-pa envelope fields
        assert!(json.get("model").is_none());
        assert!(json.get("request").is_none());
    }

    // ── JWT-based Vertex AI token acquisition tests ───────────────────────

    #[test]
    fn vertex_jwt_has_correct_structure() {
        let auth = test_vertex_auth();
        let creds = match &auth {
            GeminiAuth::VertexServiceAccount { creds, .. } => creds.clone(),
            _ => panic!("expected VertexServiceAccount"),
        };

        let jwt = GeminiProvider::build_vertex_jwt(&creds).expect("JWT build");

        // JWT has 3 dot-separated parts
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT should have header.payload.signature");

        // Decode and verify header
        let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[0])
            .unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["typ"], "JWT");

        // Decode and verify claims
        let claims_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).unwrap();
        assert_eq!(claims["iss"], "test@test-project.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
        assert_eq!(
            claims["scope"],
            "https://www.googleapis.com/auth/cloud-platform"
        );
        assert!(claims["iat"].is_number());
        assert!(claims["exp"].is_number());

        // exp should be iat + 3600
        let iat = claims["iat"].as_i64().unwrap();
        let exp = claims["exp"].as_i64().unwrap();
        assert_eq!(exp - iat, 3600);

        // Signature part should be non-empty
        assert!(!parts[2].is_empty());
    }

    // ── Part struct function-calling serialization tests ──────────────────

    #[test]
    fn part_text_only_serializes_without_function_fields() {
        let part = Part {
            text: Some("hello".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&part).unwrap();
        assert_eq!(json, serde_json::json!({"text": "hello"}));
        assert!(json.get("functionCall").is_none());
        assert!(json.get("functionResponse").is_none());
    }

    #[test]
    fn part_function_call_serializes_correctly() {
        let part = Part {
            function_call: Some(FunctionCallPart {
                name: "get_status".into(),
                args: serde_json::json!({"channel": "general"}),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&part).unwrap();
        assert_eq!(json["functionCall"]["name"], "get_status");
        assert_eq!(json["functionCall"]["args"]["channel"], "general");
        assert!(json.get("text").is_none());
    }

    #[test]
    fn part_function_response_serializes_correctly() {
        let part = Part {
            function_response: Some(FunctionResponsePart {
                name: "get_status".into(),
                response: serde_json::json!({"ok": true}),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&part).unwrap();
        assert_eq!(json["functionResponse"]["name"], "get_status");
        assert_eq!(json["functionResponse"]["response"]["ok"], true);
    }

    #[test]
    fn response_part_deserializes_function_call() {
        let json = serde_json::json!({
            "functionCall": {"name": "get_status", "args": {"channel": "general"}}
        });
        let part: ResponsePart = serde_json::from_value(json).unwrap();
        assert!(part.text.is_none());
        let fc = part.function_call.unwrap();
        assert_eq!(fc.name, "get_status");
        assert_eq!(fc.args["channel"], "general");
    }

    #[test]
    fn gemini_provider_capabilities_include_native_tools() {
        let provider = GeminiProvider::new(Some("test-key"));
        let caps = provider.capabilities();
        assert!(caps.native_tool_calling);
        assert!(caps.vision);
    }

    #[test]
    fn gemini_provider_convert_tools_returns_gemini_payload() {
        use crate::tools::ToolSpec;
        let provider = GeminiProvider::new(Some("test-key"));
        let tools = vec![ToolSpec {
            name: "get_status".into(),
            description: "Get channel status".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"channel": {"type": "string"}}}),
        }];
        let payload = provider.convert_tools(&tools);
        match payload {
            ToolsPayload::Gemini {
                function_declarations,
            } => {
                assert_eq!(function_declarations.len(), 1);
                assert_eq!(function_declarations[0]["name"], "get_status");
            }
            _ => panic!("Expected Gemini payload"),
        }
    }

    #[test]
    fn gemini_supports_native_tools() {
        let provider = GeminiProvider::new(Some("test-key"));
        assert!(provider.supports_native_tools());
    }

    #[test]
    fn candidate_content_extracts_function_calls() {
        let content = CandidateContent {
            parts: vec![ResponsePart {
                text: None,
                thought: false,
                thought_signature: None,
                function_call: Some(FunctionCallResponse {
                    name: "get_status".into(),
                    args: serde_json::json!({"channel": "general", "verbose": true}),
                }),
            }],
        };
        let (text, calls, _) = content.extract_response();
        assert!(text.is_none());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_status");
    }

    #[test]
    fn candidate_content_extracts_mixed_text_and_calls() {
        let content = CandidateContent {
            parts: vec![
                ResponsePart {
                    text: Some("Processing request.".into()),
                    thought: false,
                    thought_signature: None,
                    function_call: None,
                },
                ResponsePart {
                    text: None,
                    thought: false,
                    thought_signature: None,
                    function_call: Some(FunctionCallResponse {
                        name: "get_status".into(),
                        args: serde_json::json!({"channel": "general"}),
                    }),
                },
            ],
        };
        let (text, calls, _) = content.extract_response();
        assert_eq!(text.as_deref(), Some("Processing request."));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn extract_response_returns_raw_parts_with_thinking() {
        let content = CandidateContent {
            parts: vec![
                ResponsePart {
                    text: Some("reasoning...".into()),
                    thought: true,
                    thought_signature: Some("sig1".into()),
                    function_call: None,
                },
                ResponsePart {
                    text: None,
                    thought: false,
                    thought_signature: Some("sig2".into()),
                    function_call: Some(FunctionCallResponse {
                        name: "search".into(),
                        args: serde_json::json!({"q": "test"}),
                    }),
                },
            ],
        };
        let (text, calls, raw_parts) = content.extract_response();

        // text should be the thinking text (fallback since no non-thinking text)
        assert_eq!(text.as_deref(), Some("reasoning..."));
        // one tool call
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        // raw_parts preserves ALL parts
        assert_eq!(raw_parts.len(), 2);
        assert_eq!(raw_parts[0].thought, Some(true));
        assert_eq!(raw_parts[0].thought_signature.as_deref(), Some("sig1"));
        assert_eq!(raw_parts[1].function_call.as_ref().unwrap().name, "search");
        assert_eq!(raw_parts[1].thought_signature.as_deref(), Some("sig2"));
    }

    #[test]
    fn part_round_trips_through_json() {
        let original = Part {
            text: Some("reasoning".into()),
            thought: Some(true),
            thought_signature: Some("sig123".into()),
            function_call: None,
            function_response: None,
        };
        let json = serde_json::to_value(&original).unwrap();
        let restored: Part = serde_json::from_value(json).unwrap();
        assert_eq!(restored.text.as_deref(), Some("reasoning"));
        assert_eq!(restored.thought, Some(true));
        assert_eq!(restored.thought_signature.as_deref(), Some("sig123"));
    }

    #[test]
    fn raw_model_parts_round_trip_preserves_thinking_signatures() {
        // Simulate what extract_response produces
        let original_parts = [
            Part {
                text: Some("reasoning...".into()),
                thought: Some(true),
                thought_signature: Some("sig_thinking".into()),
                ..Default::default()
            },
            Part {
                function_call: Some(FunctionCallPart {
                    name: "search".into(),
                    args: serde_json::json!({"q": "test"}),
                }),
                thought_signature: Some("sig_call".into()),
                ..Default::default()
            },
        ];

        // Serialize to JSON values (as stored in provider_parts)
        let json_values: Vec<serde_json::Value> = original_parts
            .iter()
            .map(|p| serde_json::to_value(p).unwrap())
            .collect();

        // Simulate storing in history JSON
        let history_json = serde_json::json!({
            "content": null,
            "tool_calls": [],
            "raw_model_parts": json_values,
        });

        // Simulate replay: extract raw_model_parts and deserialize
        let raw = history_json
            .get("raw_model_parts")
            .unwrap()
            .as_array()
            .unwrap();
        let restored: Vec<Part> = raw
            .iter()
            .filter_map(|p| serde_json::from_value(p.clone()).ok())
            .collect();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].thought, Some(true));
        assert_eq!(
            restored[0].thought_signature.as_deref(),
            Some("sig_thinking")
        );
        assert_eq!(restored[0].text.as_deref(), Some("reasoning..."));
        assert_eq!(restored[1].function_call.as_ref().unwrap().name, "search");
        assert_eq!(restored[1].thought_signature.as_deref(), Some("sig_call"));
    }

    #[test]
    fn tool_message_with_invalid_json_produces_error_content() {
        // Simulate what the old sanitizer merge produced: two JSON objects
        // joined by newline — invalid JSON.
        let corrupted = r#"{"tool_call_id":"call1","content":"r1"}
{"tool_call_id":"call2","content":"r2"}"#;

        // Attempt to parse like gemini.rs does
        let parsed = serde_json::from_str::<serde_json::Value>(corrupted);
        assert!(parsed.is_err(), "merged JSON must fail to parse");
    }

    #[test]
    fn tool_parse_warning_preview_safe_on_multibyte_utf8() {
        // Build a string where byte 200 falls inside a multi-byte codepoint.
        // 199 ASCII bytes + U+00E9 (e-acute, 2 UTF-8 bytes) puts byte 200
        // inside the multi-byte codepoint.
        let mut content = "a".repeat(199);
        content.push('\u{00E9}'); // bytes 199..201
        content.push_str("trailing");

        // Byte slicing at 200 would panic because it is mid-codepoint
        assert!(
            !content.is_char_boundary(200),
            "byte 200 must NOT be a char boundary for this test to be meaningful"
        );

        // Safe truncation via chars().take(200) must not panic
        let preview: String = content.chars().take(200).collect();
        assert_eq!(preview.chars().count(), 200);
        assert!(preview.ends_with('\u{00E9}'));
    }
}
