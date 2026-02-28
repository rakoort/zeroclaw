pub mod catalog;
pub(crate) mod catalog_registry;
pub mod linear;
pub mod slack;

// Re-export catalog types for callers (gateway API, main.rs CLI).
#[allow(unused_imports)]
pub use catalog::{
    handle_command, IntegrationCategory, IntegrationEntry, IntegrationStatus,
};

use async_trait::async_trait;
use std::sync::Arc;

use crate::channels::traits::Channel;
use crate::tools::traits::Tool;

/// A runtime integration that owns an authenticated API client and exposes
/// tools (and optionally a channel) to the agent.
#[async_trait]
pub trait Integration: Send + Sync {
    /// Short identifier for this integration (e.g. `"slack"`, `"linear"`).
    fn name(&self) -> &str;

    /// Tools provided by this integration for LLM function calling.
    fn tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Quick connectivity check. Returns `true` if the API is reachable.
    async fn health_check(&self) -> bool {
        true
    }

    /// If this integration also acts as a channel, return a channel reference.
    fn as_channel(&self) -> Option<Arc<dyn Channel>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyIntegration;

    #[async_trait]
    impl Integration for DummyIntegration {
        fn name(&self) -> &str {
            "dummy"
        }
        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }
    }

    #[tokio::test]
    async fn dummy_integration_default_methods() {
        let i = DummyIntegration;
        assert_eq!(i.name(), "dummy");
        assert!(i.tools().is_empty());
        assert!(i.health_check().await);
        assert!(i.as_channel().is_none());
    }

}
