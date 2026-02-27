# Vertex AI Service Account Authentication for Gemini Provider

**Date:** 2026-02-27
**Scope:** `src/providers/gemini.rs`, `Cargo.toml`
**Risk tier:** Medium (auth boundary change, no security policy weakening)

## Problem

The Gemini provider supports only Google AI Studio authentication (API keys, Gemini CLI OAuth, managed OAuth). The public AI Studio API has unreliable rate limits for production workloads. Vertex AI offers stable, regional endpoints with service account authentication suited for server deployments.

## Solution

Add a `GeminiAuth::VertexServiceAccount` variant that authenticates via GCP service account JWT grant and routes requests to regional Vertex AI endpoints. No new config struct fields. All configuration is environment-driven.

## Data Structures

```rust
struct VertexServiceAccountCreds {
    client_email: String,
    key_pair: Arc<ring::signature::RsaKeyPair>,  // parsed at construction time
    project_id: String,
    region: String,            // VERTEX_REGION env, default "europe-west1"
}

struct VertexTokenState {
    access_token: String,
    expiry_millis: i64,
}

enum GeminiAuth {
    // ... existing variants ...
    VertexServiceAccount {
        creds: VertexServiceAccountCreds,
        token_state: Arc<tokio::sync::Mutex<Option<VertexTokenState>>>,
    },
}
```

Token state starts `None` (lazy acquisition on first use). The 60-second expiry buffer from `get_valid_oauth_token` is reused.

## Auth Priority

Updated chain in both `new()` and `new_with_auth()`:

```
ExplicitKey -> GEMINI_API_KEY -> GOOGLE_API_KEY -> VertexServiceAccount -> CLI OAuth / ManagedOAuth
```

Vertex slots after API keys but before OAuth. Explicit keys always win; Vertex takes precedence over CLI OAuth for production deployments.

## Credential Loading

New helper `try_load_vertex_service_account() -> Option<GeminiAuth>`:

1. Check `VERTEX_SERVICE_ACCOUNT_JSON` env var -- base64-decode to get JSON (containerized deployments).
2. Otherwise check `GOOGLE_APPLICATION_CREDENTIALS` env var -- read the file at that path.
3. Parse JSON, validate `"type": "service_account"`, extract `client_email`, `private_key`, `project_id`.
4. Decode PEM private key to DER bytes using the `pem` crate. Dispatch based on PEM tag:
   - `"PRIVATE KEY"` (PKCS#8, standard for Google SA) -> `RsaKeyPair::from_pkcs8()`
   - `"RSA PRIVATE KEY"` (PKCS#1, rare) -> `RsaKeyPair::from_der()`
   - Other tags -> fail with clear error
5. Validate the key parses at construction time (fail fast). Store the parsed `RsaKeyPair` in an `Arc` inside `VertexServiceAccountCreds`.
6. Read `VERTEX_REGION` env var, default `"europe-west1"`.

## URL Building

New arm in `build_generate_content_url()`:

```
https://{region}-aiplatform.googleapis.com/v1/projects/{project_id}/locations/{region}/publishers/google/models/{model}:generateContent
```

The model ID strips any `models/` prefix.

## Request Construction

Vertex AI uses the standard `GenerateContentRequest` -- same body as the API key path, not the cloudcode-pa envelope. The existing default arm in `build_generate_content_request()` handles this. The only addition: attach `Authorization: Bearer {token}` for Vertex auth.

## Token Acquisition

New async method `get_valid_vertex_token()`:

1. Lock token state mutex.
2. If cached token exists and expiry is more than 60 seconds away, return it.
3. Build JWT:
   - Header: `{"alg":"RS256","typ":"JWT"}`
   - Claims: `{iss, scope, aud, iat, exp}` with 1-hour lifetime
   - Sign with `ring::signature::RsaKeyPair::sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), msg, &mut sig)` where `sig` buffer is `key_pair.public().modulus_len()` bytes
4. POST to `GOOGLE_TOKEN_ENDPOINT` with `grant_type=urn:ietf:params:oauth:grant_type:jwt-bearer&assertion={jwt}`
5. Parse `access_token` and `expires_in`, cache as `VertexTokenState`.

## Integration in `send_generate_content()`

New match arm alongside `OAuthToken` and `ManagedOAuth`:

```rust
GeminiAuth::VertexServiceAccount { creds, token_state } => {
    let token = Self::get_valid_vertex_token(creds, token_state).await?;
    (Some(token), None)  // project is in the URL, not the body
}
```

## Error Handling

- Vertex responses use the same `GenerateContentResponse` format. Existing parsing works unchanged.
- The cloudcode-pa retry paths (`should_retry_oauth_without_generation_config`, `should_rotate_oauth_on_error`) do not apply to Vertex because `is_oauth()` returns `false` for the Vertex variant.
- JWT/token acquisition failures return clear errors with project_id and region (never the key).

## Method Updates

| Method | Change |
|--------|--------|
| `is_api_key()` | Returns `false` for Vertex |
| `is_oauth()` | Returns `false` for Vertex |
| `auth_source()` | Returns `"Vertex AI service account"` |
| `has_any_auth()` | Also checks Vertex env vars |
| `warmup()` | Logs project_id and region, optionally pre-acquires token |

## New Dependency

`pem` crate added to `Cargo.toml` -- pure Rust PEM decoder, no feature flags needed. `ring` is already in the dependency tree via `rustls`.

## Environment Variables

| Variable | Purpose | Required |
|----------|---------|----------|
| `GOOGLE_APPLICATION_CREDENTIALS` | Path to service account JSON file | One of these two |
| `VERTEX_SERVICE_ACCOUNT_JSON` | Inline base64-encoded service account JSON | One of these two |
| `VERTEX_REGION` | Vertex AI region | No (default: `europe-west1`) |

## Not In Scope

- Streaming (`streamGenerateContent`)
- Vertex-specific features (tuned models, model garden)
- Vertex AI pricing/quota management
- Changes to `config/provider.rs`

## Rollback

Revert the commit. No config schema changes, no migration needed. Environment variables are inert if the code is absent.
