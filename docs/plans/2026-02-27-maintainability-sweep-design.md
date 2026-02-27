# Maintainability Sweep — Design Document

**Date:** 2026-02-27
**Scope:** Codebase-wide refactoring for maintainability, simplification, and code reduction
**Approach:** Bottom-up (extract shared utilities first, then split large files)

## Problem

The ZeroClaw codebase (158K lines, 219 files) has strong architectural bones — trait-driven extensibility, factory patterns, isolated subsystems — but suffers from localized maintainability issues:

- **Four mega-files** account for 28K lines: `schema.rs` (8.3K), `wizard.rs` (7.2K), `channels/mod.rs` (6.9K), `agent/loop_.rs` (5.6K).
- **54+ scattered HTTP client instances** duplicate connection setup across channels and providers.
- **22 channel implementations** repeat message chunking, retry, health check, and webhook verification logic.
- **Three tiny modules** (`daemon/`, `health/`, `heartbeat/`) overlap with `service/` and `observability/`.

## Constraints

- Each step must produce a compiling, testable result.
- Feature flags (`channel-lark`, `channel-matrix`, `whatsapp-web`, etc.) must not regress; shared utilities must not force unconditional dependencies.
- Config types are public API — backward compatibility matters during migration.
- The `agent/loop_.rs` split is deferred because tool execution and history management are tightly coupled. Revisit after the foundation stabilizes.

---

## Step 1: HTTP Client Factory — `src/common/http.rs`

### What changes

Create `src/common/mod.rs` and `src/common/http.rs`. Provide a small set of pre-configured clients selected by profile, initialized within the async entry point and passed via `Arc<AppState>` or constructor injection.

### Design

```rust
// src/common/http.rs

pub enum ClientProfile {
    /// Remote cloud APIs: standard TLS, default proxy, 30s timeout
    Cloud,
    /// Local hardware/peripherals: custom TLS roots, no proxy, 5s timeout
    LocalHardware,
    /// Long-running streams: standard TLS, 5min timeout
    Streaming,
}

pub struct HttpClients {
    cloud: reqwest::Client,
    local: reqwest::Client,
    streaming: reqwest::Client,
}

impl HttpClients {
    /// Call once during async startup, not in a global static.
    pub fn new(proxy: Option<&str>) -> Result<Self> { ... }

    pub fn get(&self, profile: ClientProfile) -> &reqwest::Client { ... }
}
```

Integrations add auth per-request:

```rust
self.http.get(ClientProfile::Cloud)
    .get(url)
    .bearer_auth(&self.token)
    .send()
    .await
```

### Why not a global static

A `Lazy<reqwest::Client>` binds to the tokio runtime active during first access. If anything touches it before the main runtime starts (pre-flight checks, test harnesses, worker threads), it panics. Explicit initialization and injection avoids this.

### Migration

Additive. Existing code compiles until migrated. Each channel/provider replaces its inline `reqwest::Client::builder()...build()` with a call through the injected `HttpClients`.

### Risk: Low

---

## Step 2: Channel Common Utilities — `src/channels/common.rs`

### What changes

Extract duplicated logic from 22 channel implementations into a shared module.

### Design

```rust
// src/channels/common.rs

/// Split message at natural boundaries (newlines, then spaces, then hard cut).
pub fn split_message(content: &str, max_len: usize) -> Vec<String> { ... }

/// Retry wrapper for channel send operations with exponential backoff.
pub async fn send_with_retry<F, Fut>(
    max_retries: u32,
    base_delay: Duration,
    operation: F,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<()>>,
{ ... }
```

Webhook signature verification uses a trait to avoid forcing crypto dependencies on channels that don't need them:

```rust
pub trait WebhookVerifier {
    fn verify(&self, payload: &[u8], signature: &str) -> bool;
}
```

Concrete implementations (HMAC-SHA256 for Telegram, Ed25519 for Discord, etc.) live in each channel's file behind existing feature gates.

### What stays per-channel

Platform-specific API calls, message format conversion (Telegram HTML, Discord embeds, Slack blocks), and platform-specific webhook handling.

### Risk: Low

---

## Step 3: Provider Common Utilities — `src/providers/common.rs`

### What changes

Extract duplicated patterns from 25+ provider implementations.

### Design

```rust
// src/providers/common.rs

/// Map provider HTTP errors to standard ProviderError variants.
pub fn map_api_error(status: StatusCode, body: &str) -> ProviderError { ... }

/// Parse SSE event stream (OpenAI-compatible format).
/// Feature-gated if it pulls heavy dependencies.
pub fn parse_sse_stream(
    response: reqwest::Response,
) -> impl Stream<Item = Result<SseEvent>> { ... }

/// Build standard request headers (content-type, accept, user-agent).
pub fn standard_headers() -> HeaderMap { ... }
```

Token estimation becomes a trait, not a shared heuristic:

```rust
pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, text: &str) -> usize;
}
```

Each provider supplies its own implementation (tiktoken-based for OpenAI, SentencePiece-based for others, rough heuristic as a fallback default). This avoids inaccurate context-window management.

### Feature-gate discipline

If `parse_sse_stream` requires crates not already in the unconditional dependency set, gate it behind a feature flag. The `common.rs` module itself must compile with no features enabled.

### Risk: Low-medium (SSE parsing needs testing across provider variants)

---

## Step 4: Split `config/schema.rs` (8,277 → 6 files)

### What changes

Split the monolithic schema into subsystem-focused files.

| New file | Contents | ~LOC |
|----------|----------|------|
| `core.rs` | Top-level `Config`, workspace paths, API/server, merge logic | 1,500 |
| `provider.rs` | Provider configs, model routing, reliability/fallback | 1,500 |
| `channel.rs` | Channel configs (all 22), gateway, tunnel settings | 2,000 |
| `memory.rs` | Memory backend configs, embeddings, vector store | 800 |
| `security.rs` | Autonomy levels, security policy, secret store | 1,000 |
| `integrations.rs` | Composio, browser, HTTP tools, SOP, skills, cron | 1,200 |

### Backward-compatible migration

To avoid a big-bang import breakage across 219 files, keep `schema.rs` as a re-export shim during transition:

```rust
// src/config/schema.rs (temporary shim)
pub use crate::config::core::*;
pub use crate::config::provider::*;
pub use crate::config::channel::*;
pub use crate::config::memory::*;
pub use crate::config::security::*;
pub use crate::config::integrations::*;
```

This lets the codebase compile immediately. Imports migrate incrementally. Remove the shim once all imports point to the new locations.

### Risk: Low (shim prevents compile breakage)

---

## Step 5: Split `onboard/wizard.rs` (7,198 → 4 files)

### What changes

| New file | Contents | ~LOC |
|----------|----------|------|
| `wizard.rs` | Flow orchestration, step sequencing, progress tracking | 1,200 |
| `provider_setup.rs` | Provider selection, API key prompting, model testing | 2,000 |
| `channel_setup.rs` | Channel selection, token/webhook configuration | 2,500 |
| `common.rs` | Shared UI helpers: prompts, selection menus, validation | 1,500 |

`wizard.rs` becomes a thin orchestrator calling `provider_setup::run()`, `channel_setup::run()`, etc. Memory and security setup remain in `wizard.rs` — they are small enough not to warrant separate files.

### Risk: Low (no downstream dependents)

---

## Step 6: Split `channels/mod.rs` (6,876 → 4 files)

### What changes

| New file | Contents | ~LOC |
|----------|----------|------|
| `mod.rs` | Module exports, re-exports | 200 |
| `types.rs` | Shared types, enums, and structs used by both factory and orchestrator | 500 |
| `factory.rs` | Channel creation factory, registration | 1,500 |
| `orchestrator.rs` | Multi-channel routing, conversation management, fan-out | 3,000 |
| `message_handler.rs` | Formatting, history, response assembly | 1,500 |

### Dependency graph first

Before splitting, map which types the factory and orchestrator share. Extract those into `types.rs` (or lean on existing `traits.rs`) so both can depend on it without circular references. Do not make internal types `pub` just to satisfy the split — if a type needs to be shared, move it to `types.rs`.

### Risk: Low-medium (requires dependency mapping)

---

## Step 7: Consolidate Small Modules

| Current module | Move to | Rationale |
|----------------|---------|-----------|
| `src/daemon/` (~100 LOC) | `src/service/daemon.rs` | Service lifecycle belongs in `service/` |
| `src/health/` (~100 LOC) | `src/observability/health.rs` | Health status is an observability concern |
| `src/heartbeat/` (~200 LOC) | `src/service/heartbeat.rs` | Periodic background pings are a service concern |

### Migration

Move files, update `src/lib.rs` declarations, update imports. Straightforward.

### Risk: Low

---

## Deferred: `agent/loop_.rs` Split

The 5,646-line agentic loop has tight coupling between tool execution and history management. Splitting it requires careful boundary identification. Revisit after Steps 1–7 land and the codebase has stabilized.

---

## Validation Strategy

After each step:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

For Steps 2–3, also verify feature-gated builds compile cleanly:

```bash
cargo check --no-default-features
cargo check --all-features
```

## Review Credits

This plan was adversarially reviewed by Gemini CLI. Six findings were incorporated:

1. `ClientProfile` enum replaces single global client (Step 1)
2. Explicit async initialization via injection replaces `Lazy` static (Step 1)
3. Feature-gate discipline for shared commons (Steps 2–3)
4. `TokenEstimator` trait replaces shared heuristic (Step 3)
5. `pub use` re-export shim for incremental schema migration (Step 4)
6. `types.rs` extraction and dependency mapping before channel split (Step 6)
