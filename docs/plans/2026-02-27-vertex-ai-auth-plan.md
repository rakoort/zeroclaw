# Vertex AI Service Account Authentication — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add Vertex AI service account authentication to the Gemini provider so it can route requests to regional Vertex AI endpoints.

**Architecture:** New `GeminiAuth::VertexServiceAccount` enum variant holds parsed credentials and a cached access token. JWT-based token acquisition (RS256 via ring) exchanges a self-signed assertion for a GCP access token. Vertex endpoint URLs embed the project and region. Request body uses the standard `GenerateContentRequest` (same as API key path).

**Tech Stack:** ring (RS256 signing, already a dependency), pem (PEM-to-DER decode, new dependency), base64 (already a dependency), serde_json (already a dependency)

**Design doc:** `docs/plans/2026-02-27-vertex-ai-auth-design.md`

---

### Task 1: Add `pem` dependency to Cargo.toml

**Files:**
- Modify: `Cargo.toml:95` (near the `ring` dependency)

**Step 1: Add the dependency**

Add `pem` after `ring` in Cargo.toml dependencies:

```toml
# PEM decode (Vertex AI service account key parsing)
pem = "3"
```

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: successful compilation, no errors

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add pem dependency for Vertex AI key parsing"
```

---

### Task 2: Add Vertex data structures and enum variant

**Files:**
- Modify: `src/providers/gemini.rs:17-80` (data structures and GeminiAuth enum)

**Step 1: Write tests for the new auth variant**

Add to the `#[cfg(test)] mod tests` block in `src/providers/gemini.rs` (after the existing `auth_source_oauth` test around line 1493):

```rust
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
```

These tests will not compile yet (no `test_vertex_auth` helper, no variant). That's expected.

**Step 2: Add the data structures and enum variant**

Add after `OAuthTokenState` (around line 38):

```rust
/// Parsed GCP service account credentials for Vertex AI.
struct VertexServiceAccountCreds {
    client_email: String,
    key_pair: Arc<ring::signature::RsaKeyPair>,
    project_id: String,
    region: String,
}

/// Cached Vertex AI access token with expiry tracking.
struct VertexTokenState {
    access_token: String,
    /// Expiry as unix millis.
    expiry_millis: i64,
}
```

Add import at top of file (with other imports):

```rust
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use ring::rand::SystemRandom;
```

Add new variant to `GeminiAuth` enum (after `ManagedOAuth`):

```rust
/// Vertex AI via GCP service account JWT grant.
/// Bearer token acquired via self-signed JWT exchange.
VertexServiceAccount {
    creds: Arc<VertexServiceAccountCreds>,
    token_state: Arc<tokio::sync::Mutex<Option<VertexTokenState>>>,
},
```

**Step 3: Update `is_api_key()`, `is_oauth()`, `api_key_credential()`, and `auth_source()`**

In `is_api_key()` — the existing `matches!` macro already excludes unknown variants (returns false). No change needed.

In `is_oauth()` — same, the `matches!` macro only matches `OAuthToken | ManagedOAuth`. No change needed.

In `api_key_credential()` — add `GeminiAuth::VertexServiceAccount { .. }` to the `""` arm alongside `OAuthToken` and `ManagedOAuth`:

```rust
GeminiAuth::OAuthToken(_) | GeminiAuth::ManagedOAuth | GeminiAuth::VertexServiceAccount { .. } => "",
```

In `auth_source()` — add before the `None` arm:

```rust
Some(GeminiAuth::VertexServiceAccount { .. }) => "Vertex AI service account",
```

**Step 4: Add test helper for Vertex auth**

Add to the test module, near `test_oauth_auth`:

```rust
/// Helper to create a test Vertex auth variant with a dummy key.
fn test_vertex_auth() -> GeminiAuth {
    // Generate a throwaway RSA key pair for testing
    use ring::signature::KeyPair;
    let rng = ring::rand::SystemRandom::new();
    let pkcs8_doc = ring::signature::RsaKeyPair::generate_serializable(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256_SIGNING,
        &rng,
    )
    .expect("test RSA key generation");
    let key_pair = ring::signature::RsaKeyPair::from_pkcs8(pkcs8_doc.as_ref())
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
```

Note: ring may not support `generate_serializable` — check if it does. If not, use a pre-generated test PKCS#8 DER key embedded as a `const &[u8]`. The fallback approach:

```rust
fn test_vertex_auth() -> GeminiAuth {
    // Use a pre-generated 2048-bit RSA PKCS#8 key for tests.
    // Generated via: openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform DER -out test.der
    // Then: xxd -i test.der
    // For now, we skip key-dependent tests if no test key is available.
    // A simpler approach: use the pem crate to parse an inline PEM test key.
    let test_pem = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC7o4qne60TB3pP
... (truncated for plan — use a real test key at implementation time)
-----END PRIVATE KEY-----";
    let parsed = pem::parse(test_pem).expect("test PEM parse");
    let key_pair = ring::signature::RsaKeyPair::from_pkcs8(parsed.contents())
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
```

At implementation time, decide which approach works with ring 0.17. The implementer should generate a real test key using `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform PEM` and embed it as a const string.

**Step 5: Run tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass including the 3 new ones.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): add VertexServiceAccount auth variant and data structures"
```

---

### Task 3: Add credential loading (`try_load_vertex_service_account`)

**Files:**
- Modify: `src/providers/gemini.rs` (new method on GeminiProvider)

**Step 1: Write tests for credential loading**

Add to the test module:

```rust
#[test]
fn vertex_sa_json_parsing_valid() {
    let sa_json = serde_json::json!({
        "type": "service_account",
        "project_id": "my-project",
        "private_key": include_str!("../../tests/fixtures/test_rsa_private_key.pem"),
        "client_email": "test@my-project.iam.gserviceaccount.com"
    });
    let result = GeminiProvider::parse_vertex_service_account_json(
        &sa_json.to_string(),
        "europe-west1",
    );
    assert!(result.is_ok());
    let creds = result.unwrap();
    assert_eq!(creds.client_email, "test@my-project.iam.gserviceaccount.com");
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
    let result = GeminiProvider::parse_vertex_service_account_json(
        &sa_json.to_string(),
        "us-central1",
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("service_account"));
}

#[test]
fn vertex_sa_json_rejects_missing_fields() {
    let sa_json = serde_json::json!({
        "type": "service_account",
        "project_id": "my-project"
        // missing private_key and client_email
    });
    let result = GeminiProvider::parse_vertex_service_account_json(
        &sa_json.to_string(),
        "us-central1",
    );
    assert!(result.is_err());
}
```

These tests require a test fixture PEM key. Create it first (see Step 2).

**Step 2: Create test RSA key fixture**

Run this command to generate a test-only RSA key:

```bash
mkdir -p tests/fixtures
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform PEM -out tests/fixtures/test_rsa_private_key.pem 2>/dev/null
```

This key is for tests only — no real credentials.

**Step 3: Implement `parse_vertex_service_account_json()`**

Add to `impl GeminiProvider` block:

```rust
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
        other => anyhow::bail!("unsupported PEM tag \"{other}\", expected PRIVATE KEY or RSA PRIVATE KEY"),
    }
    .map_err(|e| anyhow::anyhow!("failed to parse RSA private key: {e}"))?;

    Ok(VertexServiceAccountCreds {
        client_email,
        key_pair: Arc::new(key_pair),
        project_id,
        region: region.to_string(),
    })
}
```

**Step 4: Implement `try_load_vertex_service_account()`**

Add to `impl GeminiProvider` block:

```rust
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
    // (GOOGLE_APPLICATION_CREDENTIALS can also point to authorized_user JSON)
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
```

**Step 5: Run tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass including the 3 new credential parsing tests.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs tests/fixtures/test_rsa_private_key.pem
git commit -m "feat(gemini): add Vertex AI service account credential loading"
```

---

### Task 4: Wire Vertex into auth priority chain

**Files:**
- Modify: `src/providers/gemini.rs` (methods `new()`, `new_with_auth()`, `has_any_auth()`)

**Step 1: Write test for auth priority**

Add to the test module:

```rust
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
```

(This test already passes by existing logic — it documents the priority contract.)

**Step 2: Update `new()` to include Vertex in the chain**

In `GeminiProvider::new()`, insert `.or_else(|| Self::try_load_vertex_service_account())` after the `GOOGLE_API_KEY` check and before the CLI OAuth fallback:

```rust
pub fn new(api_key: Option<&str>) -> Self {
    let oauth_cred_paths = Self::discover_oauth_cred_paths();
    let resolved_auth = api_key
        .and_then(Self::normalize_non_empty)
        .map(GeminiAuth::ExplicitKey)
        .or_else(|| Self::load_non_empty_env("GEMINI_API_KEY").map(GeminiAuth::EnvGeminiKey))
        .or_else(|| Self::load_non_empty_env("GOOGLE_API_KEY").map(GeminiAuth::EnvGoogleKey))
        .or_else(|| Self::try_load_vertex_service_account())
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
```

**Step 3: Update `new_with_auth()` similarly**

In `GeminiProvider::new_with_auth()`, insert Vertex after API key checks, before the managed OAuth check:

```rust
// First check API keys, then Vertex
let resolved_auth = api_key
    .and_then(Self::normalize_non_empty)
    .map(GeminiAuth::ExplicitKey)
    .or_else(|| Self::load_non_empty_env("GEMINI_API_KEY").map(GeminiAuth::EnvGeminiKey))
    .or_else(|| Self::load_non_empty_env("GOOGLE_API_KEY").map(GeminiAuth::EnvGoogleKey))
    .or_else(|| Self::try_load_vertex_service_account());
```

The rest of the method (managed OAuth check, CLI OAuth fallback) stays the same.

**Step 4: Update `has_any_auth()`**

Add Vertex env var check:

```rust
pub fn has_any_auth() -> bool {
    Self::load_non_empty_env("GEMINI_API_KEY").is_some()
        || Self::load_non_empty_env("GOOGLE_API_KEY").is_some()
        || Self::load_non_empty_env("VERTEX_SERVICE_ACCOUNT_JSON").is_some()
        || Self::load_non_empty_env("GOOGLE_APPLICATION_CREDENTIALS").is_some()
        || Self::has_cli_credentials()
}
```

**Step 5: Run tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): wire Vertex AI into auth priority chain"
```

---

### Task 5: Implement Vertex AI URL building

**Files:**
- Modify: `src/providers/gemini.rs` (method `build_generate_content_url()`)

**Step 1: Write tests**

Add to the test module:

```rust
#[test]
fn vertex_url_uses_regional_endpoint() {
    let auth = test_vertex_auth();
    let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
    assert_eq!(
        url,
        "https://europe-west1-aiplatform.googleapis.com/v1/projects/test-project/locations/europe-west1/publishers/google/models/gemini-3-flash-preview:generateContent"
    );
}

#[test]
fn vertex_url_strips_models_prefix() {
    let auth = test_vertex_auth();
    let url = GeminiProvider::build_generate_content_url("models/gemini-3-flash-preview", &auth);
    assert!(url.contains("/models/gemini-3-flash-preview:"));
    assert!(!url.contains("/models/models/"));
}

#[test]
fn vertex_url_does_not_include_api_key() {
    let auth = test_vertex_auth();
    let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
    assert!(!url.contains("?key="));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests::vertex_url 2>&1 | tail -10`
Expected: FAIL (Vertex variant not matched in `build_generate_content_url`)

**Step 3: Add the Vertex arm to `build_generate_content_url()`**

Add a new match arm before the `_ =>` default:

```rust
GeminiAuth::VertexServiceAccount { ref creds, .. } => {
    let model_id = model.strip_prefix("models/").unwrap_or(model);
    format!(
        "https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{model}:generateContent",
        region = creds.region,
        project = creds.project_id,
        model = model_id,
    )
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): add Vertex AI regional endpoint URL building"
```

---

### Task 6: Implement Vertex request construction (Bearer auth)

**Files:**
- Modify: `src/providers/gemini.rs` (method `build_generate_content_request()`)

**Step 1: Write test**

Add to the test module:

```rust
#[test]
fn vertex_request_uses_bearer_auth_and_flat_body() {
    let auth = test_vertex_auth();
    let provider = test_provider(Some(test_vertex_auth()));
    let url = GeminiProvider::build_generate_content_url("gemini-3-flash-preview", &auth);
    let body = GenerateContentRequest {
        contents: vec![Content {
            role: Some("user".into()),
            parts: vec![Part { text: "hello".into() }],
        }],
        system_instruction: None,
        generation_config: GenerationConfig {
            temperature: 0.7,
            max_output_tokens: 8192,
        },
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
        request.headers().get(reqwest::header::AUTHORIZATION)
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
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests::vertex_request 2>&1 | tail -10`
Expected: FAIL (Vertex falls through to default arm which doesn't set Bearer header)

**Step 3: Update `build_generate_content_request()`**

Add a new arm for Vertex before the `_ =>` default arm. The Vertex path uses the flat `GenerateContentRequest` body (like API key) but adds Bearer auth (like OAuth):

```rust
GeminiAuth::VertexServiceAccount { .. } => {
    let token = oauth_token.unwrap_or_default();
    self.http_client()
        .post(url)
        .json(request)
        .bearer_auth(token)
}
```

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): add Vertex AI Bearer auth request construction"
```

---

### Task 7: Implement JWT-based token acquisition

**Files:**
- Modify: `src/providers/gemini.rs` (new methods `build_vertex_jwt()` and `get_valid_vertex_token()`)

**Step 1: Write test for JWT structure**

Add to the test module:

```rust
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
        .decode(parts[0]).unwrap();
    let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
    assert_eq!(header["alg"], "RS256");
    assert_eq!(header["typ"], "JWT");

    // Decode and verify claims
    let claims_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1]).unwrap();
    let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).unwrap();
    assert_eq!(claims["iss"], "test@test-project.iam.gserviceaccount.com");
    assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
    assert_eq!(claims["scope"], "https://www.googleapis.com/auth/cloud-platform");
    assert!(claims["iat"].is_number());
    assert!(claims["exp"].is_number());

    // exp should be iat + 3600
    let iat = claims["iat"].as_i64().unwrap();
    let exp = claims["exp"].as_i64().unwrap();
    assert_eq!(exp - iat, 3600);

    // Signature part should be non-empty
    assert!(!parts[2].is_empty());
}
```

**Step 2: Implement `build_vertex_jwt()`**

Add to `impl GeminiProvider`:

```rust
/// Build a self-signed JWT for Vertex AI service account token exchange.
///
/// JWT structure:
/// - Header: {"alg":"RS256","typ":"JWT"}
/// - Claims: {iss, scope, aud, iat, exp} with 1-hour lifetime
/// - Signature: RS256 (PKCS#1 v1.5 + SHA-256) via ring
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

    let rng = SystemRandom::new();
    let mut signature = vec![0u8; creds.key_pair.public().modulus_len()];
    creds
        .key_pair
        .sign(&RSA_PKCS1_SHA256, &rng, signing_input.as_bytes(), &mut signature)
        .map_err(|_| anyhow::anyhow!("RSA signing failed"))?;

    let signature_b64 = b64.encode(&signature);
    Ok(format!("{signing_input}.{signature_b64}"))
}
```

**Step 3: Implement `get_valid_vertex_token()`**

Add to `impl GeminiProvider`:

```rust
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

        #[derive(Deserialize)]
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
```

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass (JWT structure test validates without hitting network).

**Step 5: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): implement JWT-based Vertex AI token acquisition"
```

---

### Task 8: Integrate Vertex into `send_generate_content()` and `warmup()`

**Files:**
- Modify: `src/providers/gemini.rs` (methods `send_generate_content()` and `warmup()`)

**Step 1: Update `send_generate_content()`**

Add a new match arm in the token resolution section (around line 959-982). After the `ManagedOAuth` arm, before `_ =>`:

```rust
GeminiAuth::VertexServiceAccount { ref creds, ref token_state } => {
    let token = Self::get_valid_vertex_token(creds, token_state).await?;
    (Some(token), None)
}
```

**Step 2: Update `warmup()`**

Add a new match arm in the `warmup()` method for Vertex:

```rust
GeminiAuth::VertexServiceAccount { ref creds, .. } => {
    tracing::info!(
        project_id = %creds.project_id,
        region = %creds.region,
        "Gemini provider: Vertex AI service account ready"
    );
}
```

**Step 3: Update no-auth error message**

In `send_generate_content()`, update the no-auth error message to mention Vertex:

```rust
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
```

**Step 4: Run full test suite**

Run: `cargo test -p zeroclaw --lib providers::gemini::tests 2>&1 | tail -20`
Expected: All tests pass.

**Step 5: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: No warnings.

**Step 6: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "feat(gemini): integrate Vertex AI auth into request flow and warmup"
```

---

### Task 9: Final validation and cleanup

**Files:**
- Verify: `src/providers/gemini.rs`, `Cargo.toml`

**Step 1: Run full validation suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: All pass, no warnings, no formatting issues.

**Step 2: Verify the module doc comment is accurate**

Update the module doc at the top of `src/providers/gemini.rs` to include Vertex:

```rust
//! Google Gemini provider with support for:
//! - Direct API key (`GEMINI_API_KEY` env var or config)
//! - Vertex AI service account (`GOOGLE_APPLICATION_CREDENTIALS` or `VERTEX_SERVICE_ACCOUNT_JSON`)
//! - Gemini CLI OAuth tokens (reuse existing ~/.gemini/ authentication)
//! - ZeroClaw auth-profiles OAuth tokens
```

(Move Vertex before CLI OAuth to match the auth priority order.)

**Step 3: Commit**

```bash
git add src/providers/gemini.rs
git commit -m "docs(gemini): update module doc to reflect Vertex AI auth support"
```

---

## Summary

| Task | Description | Files |
|------|-------------|-------|
| 1 | Add `pem` dependency | `Cargo.toml` |
| 2 | Data structures + enum variant | `src/providers/gemini.rs` |
| 3 | Credential loading | `src/providers/gemini.rs`, test fixture |
| 4 | Auth priority chain | `src/providers/gemini.rs` |
| 5 | URL building | `src/providers/gemini.rs` |
| 6 | Request construction | `src/providers/gemini.rs` |
| 7 | JWT + token acquisition | `src/providers/gemini.rs` |
| 8 | Integration + warmup | `src/providers/gemini.rs` |
| 9 | Validation + cleanup | `src/providers/gemini.rs` |

All 9 tasks are atomic, independently testable, and each produces a working commit.
