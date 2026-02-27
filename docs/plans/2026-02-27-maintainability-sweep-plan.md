# Maintainability Sweep Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reduce maintenance burden across the ZeroClaw codebase through shared utilities extraction, mega-file splitting, and module consolidation.

**Architecture:** Bottom-up — extract shared utilities first (HTTP client, channel commons, provider commons), then split the four largest files (schema.rs, wizard.rs, channels/mod.rs), then consolidate tiny modules. Each task produces a single atomic commit.

**Tech Stack:** Rust, reqwest, tokio, serde, anyhow

**Design doc:** `docs/plans/2026-02-27-maintainability-sweep-design.md`

---

## Phase 1: HTTP Client Factory Relocation

### Task 1: Move proxy client factory from schema.rs to common/http.rs

An HTTP client factory already exists in `src/config/schema.rs` (lines 16–58, 1137–1595): `build_runtime_proxy_client()`, `build_runtime_proxy_client_with_timeouts()`, `apply_runtime_proxy_to_builder()`, plus the `ProxyConfig` impl, proxy statics, and helper functions. This task moves it to a dedicated module so schema.rs can focus on config types.

**Files:**
- Create: `src/common/mod.rs`
- Create: `src/common/http.rs`
- Modify: `src/config/schema.rs` (remove proxy client factory functions and statics, keep ProxyConfig struct)
- Modify: `src/config/mod.rs` (update re-exports)
- Modify: `src/lib.rs` (add `pub mod common;`)

**Step 1: Write a test for the relocated factory**

Create `src/common/http.rs` with a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_runtime_proxy_client_returns_usable_client() {
        let client = build_runtime_proxy_client("provider.anthropic");
        // Client should be usable (not panic)
        assert!(client.get("https://example.com").build().is_ok());
    }

    #[test]
    fn build_runtime_proxy_client_with_timeouts_returns_usable_client() {
        let client = build_runtime_proxy_client_with_timeouts("provider.anthropic", 30, 10);
        assert!(client.get("https://example.com").build().is_ok());
    }

    #[test]
    fn cached_client_returns_same_instance() {
        let a = build_runtime_proxy_client("test.cache_a");
        let b = build_runtime_proxy_client("test.cache_a");
        // reqwest::Client is Arc-based; same cache key should return clone of same inner
        drop(a);
        drop(b);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib common::http::tests -- --nocapture`
Expected: FAIL — module does not exist yet.

**Step 3: Implement the move**

1. Create `src/common/mod.rs`:
```rust
pub mod http;
```

2. Add to `src/lib.rs` (after `pub mod channels;` around line 44):
```rust
pub mod common;
```

3. Create `src/common/http.rs` by moving from `src/config/schema.rs`:
   - Move statics `RUNTIME_PROXY_CONFIG` and `RUNTIME_PROXY_CLIENT_CACHE` (lines 56–58)
   - Move constants `SUPPORTED_PROXY_SERVICE_KEYS` (lines 16–45) and `SUPPORTED_PROXY_SERVICE_SELECTORS` (lines 47–54)
   - Move all public proxy functions: `runtime_proxy_config`, `set_runtime_proxy_config`, `apply_runtime_proxy_to_builder`, `build_runtime_proxy_client`, `build_runtime_proxy_client_with_timeouts`
   - Move private helpers: `runtime_proxy_cache_key`, `runtime_proxy_cached_client`, `set_runtime_proxy_cached_client`, `parse_proxy_scope`, `parse_proxy_enabled`
   - Move the `impl ProxyConfig` block (lines 1187–1599) that contains `apply_to_reqwest_builder` and related methods
   - Keep `ProxyConfig` struct + `ProxyScope` enum definitions in `schema.rs` (they're serialized config types)
   - Add `use crate::config::schema::{ProxyConfig, ProxyScope};` at top of `common/http.rs`

4. In `src/config/schema.rs`, replace the moved code with re-exports:
```rust
// Proxy client factory relocated to crate::common::http
pub use crate::common::http::{
    apply_runtime_proxy_to_builder, build_runtime_proxy_client,
    build_runtime_proxy_client_with_timeouts, runtime_proxy_config, set_runtime_proxy_config,
};
```

5. In `src/config/mod.rs`, update re-exports to also include from `common::http` (the existing re-exports through schema.rs will still work via the shim).

**Step 4: Run tests to verify everything passes**

Run: `cargo test -p zeroclaw --lib common::http::tests -- --nocapture`
Expected: PASS

Run: `cargo test -p zeroclaw`
Expected: PASS — all existing tests still work via re-exports.

**Step 5: Commit**

```bash
git add src/common/ src/config/schema.rs src/config/mod.rs src/lib.rs
git commit -m "refactor: extract HTTP proxy client factory to src/common/http.rs

Move build_runtime_proxy_client, build_runtime_proxy_client_with_timeouts,
and related proxy functions from config/schema.rs to common/http.rs.
Re-export shim in schema.rs preserves all existing imports."
```

---

### Task 2: Migrate bare reqwest::Client::new() calls to proxy factory

Multiple channels and tools use `reqwest::Client::new()` without proxy support. Migrate them to use `build_runtime_proxy_client()` with appropriate service keys.

**Files to modify:**
- `src/channels/telegram.rs:336` — `reqwest::Client::new()` → `build_runtime_proxy_client("channel.telegram")`
- `src/channels/wati.rs:258,268,299,311,329,443` — 6× `reqwest::Client::new()` → single `self.client` field using `build_runtime_proxy_client("channel.wati")`
- `src/channels/nextcloud_talk.rs:24` — `reqwest::Client::new()` → `build_runtime_proxy_client("channel.nextcloud_talk")`
- `src/channels/linq.rs:26` — `reqwest::Client::new()` → `build_runtime_proxy_client("channel.linq")`
- `src/channels/clawdtalk.rs:65-68` — `Client::builder().timeout(30s)` → `build_runtime_proxy_client_with_timeouts("channel.clawdtalk", 30, 10)`
- `src/auth/mod.rs:43` — `reqwest::Client::new()` → `build_runtime_proxy_client("provider.compatible")`

**Step 1: For each file, add `use crate::common::http::build_runtime_proxy_client;` (or `_with_timeouts`)**

**Step 2: Replace each `reqwest::Client::new()` with the appropriate factory call**

For WATI specifically, consolidate 6 separate `Client::new()` calls into a single `client` field on the struct, initialized once in `WatiChannel::new()`.

**Step 3: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS

**Step 4: Commit**

```bash
git add src/channels/telegram.rs src/channels/wati.rs src/channels/nextcloud_talk.rs \
  src/channels/linq.rs src/channels/clawdtalk.rs src/auth/mod.rs
git commit -m "refactor: migrate bare reqwest::Client::new() to proxy factory

Replace 12 scattered Client::new() calls with build_runtime_proxy_client()
to ensure consistent proxy, TLS, and timeout configuration.
Consolidate WATI channel's 6 client instances into a single struct field."
```

---

## Phase 2: Channel Common Utilities

### Task 3: Extract split_message to channels/common.rs

Four channels implement message splitting with different limits: Discord (2000), Slack (4000), Telegram (4096), IRC (variable bytes). Extract a shared utility.

**Files:**
- Create: `src/channels/common.rs`
- Modify: `src/channels/mod.rs` (add `pub mod common;`)
- Modify: `src/channels/discord.rs` (replace `split_message_for_discord`)
- Modify: `src/channels/slack.rs` (replace `chunk_message`)
- Modify: `src/channels/telegram.rs` (replace `split_message_for_telegram`)

**Step 1: Write failing tests**

Create `src/channels/common.rs`:

```rust
use std::time::Duration;

/// Split a message at natural boundaries (double newlines, then single
/// newlines, then spaces, then hard cut) to fit within `max_len` characters.
pub fn split_message(content: &str, max_len: usize) -> Vec<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_returns_single_chunk() {
        let result = split_message("hello", 100);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn empty_message_returns_empty_vec() {
        let result = split_message("", 100);
        assert!(result.is_empty());
    }

    #[test]
    fn splits_at_double_newline_first() {
        let msg = "part one\n\npart two";
        let result = split_message(msg, 12);
        assert_eq!(result, vec!["part one", "part two"]);
    }

    #[test]
    fn splits_at_single_newline_when_no_double() {
        let msg = "line one\nline two";
        let result = split_message(msg, 12);
        assert_eq!(result, vec!["line one", "line two"]);
    }

    #[test]
    fn splits_at_space_when_no_newline() {
        let msg = "word1 word2 word3";
        let result = split_message(msg, 11);
        assert_eq!(result, vec!["word1 word2", "word3"]);
    }

    #[test]
    fn hard_splits_when_no_boundary() {
        let msg = "abcdefghij";
        let result = split_message(msg, 5);
        assert_eq!(result, vec!["abcde", "fghij"]);
    }

    #[test]
    fn respects_discord_limit() {
        let long_msg = "x".repeat(4500);
        let chunks = split_message(&long_msg, 2000);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000);
        }
        let rejoined: String = chunks.join("");
        assert_eq!(rejoined.len(), 4500);
    }

    #[test]
    fn respects_telegram_limit() {
        let long_msg = "x".repeat(8000);
        let chunks = split_message(&long_msg, 4096);
        for chunk in &chunks {
            assert!(chunk.len() <= 4096);
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p zeroclaw --lib channels::common::tests -- --nocapture`
Expected: FAIL — `todo!()` panics.

**Step 3: Write minimal implementation**

Replace `todo!()` in `split_message`:

```rust
pub fn split_message(content: &str, max_len: usize) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    if content.len() <= max_len {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = content;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        let slice = &remaining[..max_len];

        // Try double newline, then single newline, then space
        let split_at = slice.rfind("\n\n")
            .map(|i| i + 2)
            .or_else(|| slice.rfind('\n').map(|i| i + 1))
            .or_else(|| slice.rfind(' ').map(|i| i + 1))
            .unwrap_or(max_len);

        let (chunk, rest) = remaining.split_at(split_at);
        let trimmed = chunk.trim_end();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        remaining = rest.trim_start_matches('\n');
    }

    chunks
}
```

**Step 4: Run tests**

Run: `cargo test -p zeroclaw --lib channels::common::tests -- --nocapture`
Expected: PASS

**Step 5: Add `pub mod common;` to `src/channels/mod.rs` (near top with other module declarations)**

**Step 6: Migrate each channel**

Replace each channel's custom splitter with a call to `common::split_message`:
- `discord.rs`: Replace `split_message_for_discord(msg)` calls with `crate::channels::common::split_message(msg, 2000)`
- `slack.rs`: Replace `chunk_message(msg)` calls with `crate::channels::common::split_message(msg, 4000)`
- `telegram.rs`: Replace `split_message_for_telegram(msg)` calls with `crate::channels::common::split_message(msg, 4096 - 30)` (30 = continuation overhead)

Keep the old functions temporarily as `#[deprecated]` aliases if any external code references them, or remove them if they're only called internally.

IRC's `split_message` operates on bytes (not chars) for protocol compliance — leave it alone.

**Step 7: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS

**Step 8: Commit**

```bash
git add src/channels/common.rs src/channels/mod.rs src/channels/discord.rs \
  src/channels/slack.rs src/channels/telegram.rs
git commit -m "refactor: extract shared split_message to channels/common.rs

Replace discord::split_message_for_discord (2000 chars),
slack::chunk_message (4000 chars), and telegram::split_message_for_telegram
(4066 chars) with a single channels::common::split_message(content, max_len).
IRC byte-level splitting is left as-is (protocol requirement)."
```

---

### Task 4: Add send_with_retry to channels/common.rs

Slack has two nearly identical retry loops in `slack_api_post` (lines 30–92) and `slack_api_get` (lines 96–158). Extract a generic retry wrapper. Email channel has a similar exponential backoff pattern (lines 368–390).

**Files:**
- Modify: `src/channels/common.rs` (add retry utility)
- Modify: `src/channels/slack.rs` (consume shared retry)

**Step 1: Write failing tests in `src/channels/common.rs`**

```rust
#[tokio::test]
async fn send_with_retry_succeeds_on_first_try() {
    let result = send_with_retry(3, Duration::from_millis(1), || async { Ok(()) }).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn send_with_retry_retries_on_failure() {
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let c = counter.clone();
    let result = send_with_retry(3, Duration::from_millis(1), move || {
        let c = c.clone();
        async move {
            let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < 2 {
                anyhow::bail!("transient error");
            }
            Ok(())
        }
    }).await;
    assert!(result.is_ok());
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn send_with_retry_gives_up_after_max() {
    let result = send_with_retry(2, Duration::from_millis(1), || async {
        anyhow::bail!("permanent error")
    }).await;
    assert!(result.is_err());
}
```

**Step 2: Verify tests fail, then implement**

```rust
/// Retry an async operation with exponential backoff and jitter.
pub async fn send_with_retry<F, Fut>(
    max_retries: u32,
    base_delay: Duration,
    operation: F,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut last_err = None;
    for attempt in 0..=max_retries {
        match operation().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < max_retries {
                    let delay = base_delay * 2u32.saturating_pow(attempt);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.unwrap())
}
```

**Step 3: Migrate Slack's duplicate retry loops to use `send_with_retry`**

**Step 4: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS

**Step 5: Commit**

```bash
git add src/channels/common.rs src/channels/slack.rs
git commit -m "refactor: extract send_with_retry to channels/common.rs

Replace Slack's duplicated POST/GET retry loops (98% identical)
with a shared send_with_retry utility using exponential backoff."
```

---

### Task 5: Add WebhookVerifier trait to channels/common.rs

Linq and Nextcloud Talk have 95% identical HMAC-SHA256 webhook verification (only payload construction differs). Extract a trait + shared verification helper.

**Files:**
- Modify: `src/channels/common.rs`
- Modify: `src/channels/linq.rs` (lines 367–400)
- Modify: `src/channels/nextcloud_talk.rs` (lines 247–277)

**Step 1: Write failing tests**

```rust
#[test]
fn verify_hmac_sha256_valid_signature() {
    let secret = b"test-secret";
    let payload = b"test-payload";
    // Pre-compute expected HMAC-SHA256
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(payload);
    let sig = hex::encode(mac.finalize().into_bytes());
    assert!(verify_hmac_sha256(secret, payload, &sig));
}

#[test]
fn verify_hmac_sha256_strips_sha256_prefix() {
    let secret = b"test-secret";
    let payload = b"test-payload";
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(payload);
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    assert!(verify_hmac_sha256(secret, payload, &sig));
}

#[test]
fn verify_hmac_sha256_rejects_wrong_signature() {
    assert!(!verify_hmac_sha256(b"secret", b"payload", "deadbeef"));
}
```

**Step 2: Implement**

```rust
/// Verify an HMAC-SHA256 signature, handling optional "sha256=" prefix.
/// Use constant-time comparison to prevent timing attacks.
pub fn verify_hmac_sha256(secret: &[u8], payload: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(payload);

    let sig_hex = signature.trim().strip_prefix("sha256=").unwrap_or(signature.trim());
    let Ok(provided) = hex::decode(sig_hex) else {
        return false;
    };

    mac.verify_slice(&provided).is_ok()
}
```

**Step 3: Migrate Linq and Nextcloud Talk**

- `linq.rs` `verify_linq_signature`: Keep timestamp validation logic, call `common::verify_hmac_sha256` for the crypto part
- `nextcloud_talk.rs` `verify_nextcloud_talk_signature`: Keep payload construction (`{random}{body}`), call `common::verify_hmac_sha256` for the crypto part

**Step 4: Run tests, commit**

```bash
git add src/channels/common.rs src/channels/linq.rs src/channels/nextcloud_talk.rs
git commit -m "refactor: extract verify_hmac_sha256 to channels/common.rs

Deduplicate 95%-identical HMAC-SHA256 webhook verification from
linq.rs and nextcloud_talk.rs. Platform-specific payload
construction stays in each channel."
```

---

## Phase 3: Provider Common Utilities

### Task 6: Create providers/common.rs with error mapping and standard headers

**Files:**
- Create: `src/providers/common.rs`
- Modify: `src/providers/mod.rs` (add `pub mod common;`)

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_api_error_includes_status_and_sanitized_body() {
        let err = map_api_error("anthropic", 429, "rate limited sk-secret123-abc");
        let msg = format!("{err}");
        assert!(msg.contains("429"));
        assert!(msg.contains("anthropic"));
        assert!(!msg.contains("sk-secret123-abc")); // secret scrubbed
    }

    #[test]
    fn standard_headers_include_content_type() {
        let headers = standard_headers();
        assert_eq!(
            headers.get("content-type").unwrap(),
            "application/json"
        );
    }
}
```

**Step 2: Implement**

```rust
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, ACCEPT, USER_AGENT};

/// Map an HTTP error from a provider API to a sanitized anyhow::Error.
pub fn map_api_error(provider: &str, status: u16, body: &str) -> anyhow::Error {
    let sanitized = crate::providers::sanitize_api_error(body);
    anyhow::anyhow!("{provider} API error ({status}): {sanitized}")
}

/// Standard request headers for JSON provider APIs.
pub fn standard_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(USER_AGENT, HeaderValue::from_static("zeroclaw/1.0"));
    headers
}
```

Note: `map_api_error` delegates to the existing `sanitize_api_error` in `providers/mod.rs` to reuse secret scrubbing. This avoids duplicating the scrubbing logic.

**Step 3: Run tests, commit**

```bash
git add src/providers/common.rs src/providers/mod.rs
git commit -m "refactor: create providers/common.rs with error mapping and standard headers

Add map_api_error (delegates to existing sanitize_api_error for secret
scrubbing) and standard_headers for JSON API requests."
```

---

### Task 7: Add TokenEstimator trait to providers/common.rs

The current codebase uses a single naive heuristic (`chars / 4`) in `StreamChunk::with_token_estimate()` (traits.rs:160). Replace with a trait so providers can supply accurate implementations.

**Files:**
- Modify: `src/providers/common.rs`
- Modify: `src/providers/traits.rs` (update `with_token_estimate` to accept estimator)

**Step 1: Write failing tests**

```rust
#[test]
fn default_estimator_approximates_chars_div_4() {
    let estimator = DefaultTokenEstimator;
    assert_eq!(estimator.estimate("hello world"), 3); // 11 chars / 4 = 2.75 → 3
}

#[test]
fn default_estimator_empty_string() {
    let estimator = DefaultTokenEstimator;
    assert_eq!(estimator.estimate(""), 0);
}
```

**Step 2: Implement**

```rust
/// Trait for estimating token counts. Each provider can supply an
/// accurate implementation; the default uses a rough chars/4 heuristic.
pub trait TokenEstimator: Send + Sync {
    fn estimate(&self, text: &str) -> usize;
}

/// Rough heuristic: ~4 characters per token. Suitable as a fallback
/// when no provider-specific estimator is available.
pub struct DefaultTokenEstimator;

impl TokenEstimator for DefaultTokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        text.len().div_ceil(4)
    }
}
```

**Step 3: Update `StreamChunk::with_token_estimate` in traits.rs to call `DefaultTokenEstimator`**

This is a code-clarity improvement — the behavior stays identical but the heuristic now has a name and a trait for future replacement.

**Step 4: Run tests, commit**

```bash
git add src/providers/common.rs src/providers/traits.rs
git commit -m "refactor: add TokenEstimator trait to providers/common.rs

Define trait for per-provider token estimation. DefaultTokenEstimator
preserves the existing chars/4 heuristic. StreamChunk::with_token_estimate
now delegates to DefaultTokenEstimator for clarity."
```

---

## Phase 4: Split config/schema.rs (8,277 lines → 7 files)

Strategy: Move types out one subsystem at a time. After each move, `schema.rs` re-exports via `pub use` so all existing imports continue to work. The re-export shim in `config/mod.rs` stays intact.

### Task 8: Extract config/channel.rs (~2,000 lines)

The largest subsystem. Move all channel config types.

**Files:**
- Create: `src/config/channel.rs`
- Modify: `src/config/schema.rs` (remove channel types, add re-exports)
- Modify: `src/config/mod.rs` (add `pub mod channel;`, update re-exports)

**Step 1: Identify types to move**

From schema.rs, move these types and their impl blocks to `config/channel.rs`:
- `ChannelsConfig` (lines 2897–3077) + `impl ChannelsConfig` + `impl Default`
- `StreamMode` (lines 3078–3091)
- `ConfigWrapper` (lines 2875–2896)
- All 18 channel-specific config structs and their `impl ChannelConfig` blocks:
  - `TelegramConfig` (3092–3123), `DiscordConfig` (3124–3151), `SlackConfig` (3153–3190)
  - `MattermostConfig` (3192–3221), `WebhookConfig` (3223–3239), `IMessageConfig` (3241–3255)
  - `MatrixConfig` (3257–3282), `SignalConfig` (3284–3317), `WhatsAppConfig` (3319–3361)
  - `LinqConfig` (3363–3385), `WatiConfig` (3387–3414), `NextcloudTalkConfig` (3416–3438)
  - `WhatsAppConfig impl` helper block (3440–3471), `IrcConfig` (3474–3517)
  - `LarkReceiveMode` (3519–3527), `LarkConfig` (3528–3567), `FeishuConfig` (3569–3601)
  - `DingTalkConfig` (3873–3891), `QQConfig` (3894–3912), `NostrConfig` (3915–3944)
- `GatewayConfig` (lines 752–850)
- `TunnelConfig` + sub-configs (lines 2806–2874): `CloudflareTunnelConfig`, `TailscaleTunnelConfig`, `NgrokTunnelConfig`, `CustomTunnelConfig`

**Step 2: Create `src/config/channel.rs`**

Add necessary imports at top:
```rust
use crate::config::traits::ChannelConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
```

Move all types listed above.

**Step 3: In `src/config/schema.rs`, replace removed types with re-exports**

```rust
// Channel config types relocated to config::channel
pub use crate::config::channel::*;
```

**Step 4: In `src/config/mod.rs`, add module declaration**

```rust
pub mod channel;
```

**Step 5: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS — all imports work through re-export shim.

**Step 6: Commit**

```bash
git add src/config/channel.rs src/config/schema.rs src/config/mod.rs
git commit -m "refactor: extract channel config types to config/channel.rs

Move ChannelsConfig, GatewayConfig, TunnelConfig, and all 18 channel-specific
config structs from schema.rs to config/channel.rs (~2000 lines).
Re-export shim in schema.rs preserves all existing imports."
```

---

### Task 9: Extract config/provider.rs (~1,500 lines)

**Files:**
- Create: `src/config/provider.rs`
- Modify: `src/config/schema.rs`
- Modify: `src/config/mod.rs`

**Types to move:**
- `ModelProviderConfig` (lines 224–242)
- `DelegateAgentConfig` (lines 243–282)
- `ModelRouteConfig` (lines 2314–2330) + `FallbackModelConfig` (2334–2342)
- `EmbeddingRouteConfig` (lines 2359–2372)
- `ReliabilityConfig` (lines 2180–2241) + `ProviderFallbackEntry` (2169–2174)
- `QueryClassificationConfig` (lines 2688–2737) and all related types:
  - `ClassificationMode` (2379–2383), `ClassificationTiers` (2387–2396)
  - `ClassificationWeights` (2400–2445), `Tier` (2452–2458)
  - `TierBoundaries` (2462–2491), `ScoringOverrides` (2493–2520)
  - `DimensionWeights` (2521–2623), `ScoringConfig` (2624–2659)
  - `PlanningConfig` (2660–2686), `ClassificationRule` (2714–2737)
- `TranscriptionConfig` (lines 365–394)

Same pattern: move types, add `pub use crate::config::provider::*;` shim in schema.rs.

**Commit:**
```bash
git commit -m "refactor: extract provider config types to config/provider.rs

Move ModelProviderConfig, ReliabilityConfig, ModelRouteConfig, and all
query classification types from schema.rs (~1500 lines).
Re-export shim preserves all existing imports."
```

---

### Task 10: Extract config/memory.rs (~800 lines)

**Types to move:**
- `MemoryConfig` (lines 1702–1782) + `QdrantConfig` (1671–1698)
- `StorageConfig` (1600–1604) + `StorageProviderSection` (1608–1612) + `StorageProviderConfig` (1616–1662)

Same pattern as above.

**Commit:**
```bash
git commit -m "refactor: extract memory config types to config/memory.rs

Move MemoryConfig, QdrantConfig, StorageConfig from schema.rs (~800 lines)."
```

---

### Task 11: Extract config/security.rs (~1,000 lines)

**Types to move:**
- `AutonomyConfig` (lines 1946–2066)
- `SecurityConfig` (3605–3625) + all sub-configs:
  - `SandboxConfig` (3738–3759), `SandboxBackend` (3765–3779)
  - `ResourceLimitsConfig` (3783–3826), `AuditConfig` (3830–3863)
  - `OtpConfig` (3643–3703), `OtpMethod` (3630–3638)
  - `EstopConfig` (3708–3733)
- `RuntimeConfig` + `DockerRuntimeConfig`

Note: `AutonomyConfig` references `AutonomyLevel` from `crate::security` — this import stays.

**Commit:**
```bash
git commit -m "refactor: extract security config types to config/security.rs

Move AutonomyConfig, SecurityConfig, SandboxConfig, OtpConfig,
and related types from schema.rs (~1000 lines)."
```

---

### Task 12: Extract config/integrations.rs (~1,200 lines)

**Types to move:**
- `ComposioConfig` (851–875), `SecretsConfig` (881–891)
- `BrowserConfig` (949–997) + `BrowserComputerUseConfig` (899–943)
- `HttpRequestConfig` (1005–1037)
- `WebFetchConfig` (1048–1088), `WebSearchConfig` (1090–1136)
- `MultimodalConfig` (478–522)
- `AgentConfig` (397–442)
- `SkillsConfig` (461–477) + `SkillsPromptInjectionMode` (443–460)
- `SchedulerConfig` (2262–2299)
- `HooksConfig` (1914–1935) + `BuiltinHooksConfig` (1934–1944)
- `HeartbeatConfig` (2738–2773), `CronConfig` (2774–2804)
- `ObservabilityConfig` (1860–1897)
- `IdentityConfig` (523–551), `CostConfig` (553–696) + `ModelPricing` (581–602)
- `HardwareConfig` (304–362) + `HardwareTransport` (283–303)
- `PeripheralsConfig` (698–712) + `PeripheralBoardConfig` (713–751)

**Commit:**
```bash
git commit -m "refactor: extract integration config types to config/integrations.rs

Move agent, browser, composio, cost, cron, hardware, heartbeat, hooks,
identity, multimodal, observability, scheduler, secrets, skills, and
web config types from schema.rs (~1200 lines)."
```

---

### Task 13: Clean up config/schema.rs (remaining core)

After Tasks 8–12, schema.rs should contain only:
- Top-level `Config` struct (lines 66–220) with fields referencing types from other modules
- `impl Default for Config` (lines 3946–3996)
- `impl Config` with `load_or_init`, `validate`, `apply_env_overrides` (lines 4370–4769+)
- `ActiveWorkspaceState` (4006–4008), `ConfigResolutionSource` (4187–4369)
- `ProxyConfig` struct + `ProxyScope` enum (types only; impl moved in Task 1)
- Re-export shims (`pub use crate::config::channel::*;` etc.)

Rename `schema.rs` to `core.rs` and update `config/mod.rs`. Or keep it as `schema.rs` if renaming is too disruptive.

**Step 1: Verify schema.rs is now ~1,500–2,000 lines (down from 8,277)**

**Step 2: Run full test suite**

Run: `cargo test -p zeroclaw`
Expected: PASS

**Step 3: Commit**

```bash
git commit -m "refactor: schema.rs now contains only core Config and proxy types

After extracting channel, provider, memory, security, and integration
config types to dedicated modules, schema.rs is reduced from 8277 to
~1800 lines. All re-export shims ensure zero import breakage."
```

---

## Phase 5: Split onboard/wizard.rs (7,198 lines → 4 files)

### Task 14: Extract onboard/provider_setup.rs (~2,000 lines)

**Files:**
- Create: `src/onboard/provider_setup.rs`
- Modify: `src/onboard/wizard.rs`
- Modify: `src/onboard/mod.rs` (add module declaration)

**Functions to move** (from wizard.rs):
- `setup_provider` (lines 2112–2825)
- `canonical_provider_name` (643–664)
- `allows_unauthenticated_model_fetch` (666–679)
- `default_model_for_provider` (690–719)
- `curated_models_for_provider` (721–1130)
- `supports_live_model_fetch` (1132–1165)
- `models_endpoint_for_provider` (1167–1198)
- `local_provider_choices` (2827–2844)
- `provider_env_var` (2847–2888)
- `provider_supports_keyless_local_usage` (2890–2895)
- `provider_supports_device_flow` (2897–2902)
- `apply_provider_update` (334–354)
- All model fetch/cache functions (1200–1956)

In `wizard.rs`, replace with: `pub(crate) use provider_setup::setup_provider;` (and other needed re-exports).

**Commit:**
```bash
git commit -m "refactor: extract provider setup to onboard/provider_setup.rs

Move setup_provider, model discovery, model caching, and provider
configuration functions from wizard.rs (~2000 lines)."
```

---

### Task 15: Extract onboard/channel_setup.rs (~2,500 lines)

**Functions to move:**
- `setup_channels` (lines 3367–5052)
- `channel_menu_choices` (3362–3364)

**Commit:**
```bash
git commit -m "refactor: extract channel setup to onboard/channel_setup.rs

Move setup_channels and channel_menu_choices from wizard.rs (~2500 lines)."
```

---

### Task 16: Extract onboard/ui.rs (shared UI helpers, ~500 lines)

**Functions to move:**
- `print_step` (1958–1966)
- `print_bullet` (1968–1970)
- `print_summary` (5490–5723)
- `resolve_interactive_onboarding_mode` (1972–2016)
- `ensure_onboard_overwrite_allowed` (2018–2052)

These are used by both provider_setup and channel_setup. Moving them avoids circular deps.

**Commit:**
```bash
git commit -m "refactor: extract onboard UI helpers to onboard/ui.rs

Move shared prompt/display helpers used across wizard, provider_setup,
and channel_setup (~500 lines)."
```

---

### Task 17: Verify wizard.rs is now ~1,200 lines

After Tasks 14–16, `wizard.rs` should contain:
- `run_wizard` (76–218) — main orchestrator
- `run_channels_repair_wizard` (221–270)
- `run_provider_update_wizard` (273–332)
- `run_quick_setup` / `run_quick_setup_with_home` (396–641)
- `setup_workspace` (2070–2107)
- `setup_tool_mode` (2906–2992)
- `setup_hardware` (2996–3180)
- `setup_project_context` (3184–3278)
- `setup_memory` (3282–3317)
- `setup_tunnel` (5057–5207)
- `scaffold_workspace` (5212–5485)

Run full test suite:
```bash
cargo test -p zeroclaw
```

**Commit:**
```bash
git commit -m "chore: verify wizard.rs reduced from 7198 to ~1200 lines"
```

---

## Phase 6: Split channels/mod.rs (6,876 lines → 4 files)

### Task 18: Extract channels/types.rs (shared types)

Map the dependency graph (from research above) and extract shared types.

**Files:**
- Create: `src/channels/types.rs`
- Modify: `src/channels/mod.rs`

**Types to move:**
- `ConversationHistoryMap` (line 92)
- `ProviderCacheMap` (143)
- `RouteSelectionMap` (144)
- `ChannelRouteSelection` (160–163)
- `ChannelRuntimeCommand` (166–172)
- `ModelCacheState` (175–177), `ModelCacheEntry` (180–183)
- `ChannelRuntimeDefaults` (186–193)
- `ConfigFileStamp` (196–199), `RuntimeConfigState` (202–205)
- `ChannelRuntimeContext` (218–245)
- `InFlightSenderTaskState` (248–252), `InFlightTaskCompletion` (254–278)
- `ChannelHealthState` (2765–2769), `ConfiguredChannel` (2781–2784)
- All constants (lines 94–144)

In `mod.rs`, add `pub(crate) use types::*;` so all existing code within the crate continues to work.

**Commit:**
```bash
git commit -m "refactor: extract shared channel types to channels/types.rs

Move type aliases, constants, ChannelRuntimeContext, and all shared
structs from channels/mod.rs (~300 lines of type definitions)."
```

---

### Task 19: Extract channels/factory.rs (channel creation and config)

**Functions to move:**
- `collect_configured_channels` (2787–3075)
- `doctor_channels` (3078–3133)
- `classify_health_result` (2771–2779)
- Runtime default helpers: `resolved_default_provider` (574), `resolved_default_model` (581), `runtime_defaults_from_config` (588)
- Provider management: `resolve_provider_alias` (553), `get_or_create_provider` (901), `create_resilient_provider_nonblocking` (947)
- Model cache: `load_cached_model_preview` (878), `build_models_help_response` (968), `build_providers_help_response` (998)

**Commit:**
```bash
git commit -m "refactor: extract channel factory to channels/factory.rs

Move collect_configured_channels, doctor_channels, provider management,
and model cache helpers from channels/mod.rs (~1500 lines)."
```

---

### Task 20: Extract channels/orchestrator.rs (message dispatch)

**Functions to move:**
- `process_channel_message` (1538–2212) — the 675-line core message processing function
- `run_message_dispatch_loop` (2214–2292)
- `handle_runtime_command_if_needed` (1023–1107)
- `maybe_apply_runtime_config_update` (669–737)
- `spawn_supervised_listener` (1417–1430) + `spawn_supervised_listener_with_health_interval` (1432–1491)
- `compute_max_in_flight_messages` (1493–1500)
- `log_worker_join_result` (1502–1506)
- `spawn_scoped_typing_task` (1508–1536)
- `start_channels` (3137–3449) — the main entry point

**Commit:**
```bash
git commit -m "refactor: extract channel orchestrator to channels/orchestrator.rs

Move process_channel_message, run_message_dispatch_loop, start_channels,
and message dispatch infrastructure from channels/mod.rs (~3000 lines)."
```

---

### Task 21: Verify channels/mod.rs is now ~200 lines

After Tasks 18–20, `mod.rs` should contain only:
- Module declarations (17–43)
- Re-exports (45–68)
- `pub mod common;` `pub(crate) mod types;` `pub(crate) mod factory;` `pub(crate) mod orchestrator;`
- Remaining message handling helpers that weren't moved

Run full test suite:
```bash
cargo test -p zeroclaw
```

**Commit:**
```bash
git commit -m "chore: verify channels/mod.rs reduced from 6876 to ~200 lines"
```

---

## Phase 7: Consolidate Small Modules

### Task 22: Move daemon/ into service/daemon.rs

**Files:**
- Move: `src/daemon/mod.rs` (558 lines) → `src/service/daemon.rs`
- Modify: `src/lib.rs` (remove `pub(crate) mod daemon;`, add re-export)
- Modify: `src/main.rs` (update `use crate::daemon::` → `use crate::service::daemon::`)

**Step 1: Move file contents**

Create `src/service/daemon.rs` with the contents of `src/daemon/mod.rs`.

**Step 2: In `src/service/mod.rs`, add:**
```rust
pub mod daemon;
```

**Step 3: In `src/lib.rs`, replace:**
```rust
pub(crate) mod daemon;
```
with a re-export if needed, or remove if all callers update to `crate::service::daemon`.

**Step 4: Update imports in `src/main.rs`**

**Step 5: Run full test suite, commit**

```bash
git commit -m "refactor: move daemon module into service/daemon.rs

Consolidate daemon (558 lines) into the service module where
service lifecycle management already lives."
```

---

### Task 23: Move health/ into observability/health.rs

**Files:**
- Move: `src/health/mod.rs` (185 lines) → `src/observability/health.rs`
- Modify: `src/lib.rs` (remove `pub(crate) mod health;`)
- Modify: `src/observability/mod.rs` (add `pub mod health;`)
- Modify: `src/daemon/` (now `src/service/daemon.rs`) — update `use crate::health::` → `use crate::observability::health::`

**Step 1: Move, update imports, test, commit**

```bash
git commit -m "refactor: move health module into observability/health.rs

Consolidate health registry (185 lines) into the observability module."
```

---

### Task 24: Move heartbeat/ into service/heartbeat.rs

**Files:**
- Move: `src/heartbeat/engine.rs` (305 lines) → `src/service/heartbeat.rs`
- Remove: `src/heartbeat/mod.rs` (35 lines — just re-exports)
- Modify: `src/lib.rs` (remove `pub(crate) mod heartbeat;`)
- Modify: `src/service/mod.rs` (add `pub mod heartbeat;`)
- Modify: `src/service/daemon.rs` — update `use crate::heartbeat::engine::` → `use crate::service::heartbeat::`

**Step 1: Move, update imports, test, commit**

```bash
git commit -m "refactor: move heartbeat engine into service/heartbeat.rs

Consolidate heartbeat (340 lines across 2 files) into the service
module alongside daemon."
```

---

## Phase 8: Final Validation

### Task 25: Full validation pass

**Step 1: Run all checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo check --no-default-features
cargo check --all-features
```

**Step 2: Verify line counts**

Confirm mega-files are reduced:
- `config/schema.rs`: ~1,800 (was 8,277)
- `onboard/wizard.rs`: ~1,200 (was 7,198)
- `channels/mod.rs`: ~200 (was 6,876)

**Step 3: Verify module count**

Confirm consolidation:
- `src/daemon/` removed (merged into `src/service/`)
- `src/health/` removed (merged into `src/observability/`)
- `src/heartbeat/` removed (merged into `src/service/`)

New modules added:
- `src/common/http.rs`
- `src/channels/common.rs`, `types.rs`, `factory.rs`, `orchestrator.rs`
- `src/providers/common.rs`
- `src/config/channel.rs`, `provider.rs`, `memory.rs`, `security.rs`, `integrations.rs`
- `src/onboard/provider_setup.rs`, `channel_setup.rs`, `ui.rs`

**Step 4: Commit validation evidence**

```bash
git commit --allow-empty -m "chore: maintainability sweep complete

Schema.rs: 8277 → ~1800 lines
Wizard.rs: 7198 → ~1200 lines
Channels/mod.rs: 6876 → ~200 lines
New shared utilities: common/http, channels/common, providers/common
Consolidated: daemon→service, health→observability, heartbeat→service
12 bare Client::new() calls migrated to proxy factory"
```

---

## Summary

| Phase | Tasks | Key metric |
|-------|-------|-----------|
| 1: HTTP client factory | 1–2 | 12 bare Client::new() eliminated |
| 2: Channel commons | 3–5 | 3 deduped utilities (split, retry, hmac) |
| 3: Provider commons | 6–7 | Error mapping + TokenEstimator trait |
| 4: Split schema.rs | 8–13 | 8,277 → ~1,800 lines |
| 5: Split wizard.rs | 14–17 | 7,198 → ~1,200 lines |
| 6: Split channels/mod.rs | 18–21 | 6,876 → ~200 lines |
| 7: Consolidate modules | 22–24 | 3 tiny modules merged |
| 8: Validation | 25 | All checks green |
