//! Construction of configured embedding providers and vector-space identity.

use std::{sync::Arc, time::Duration};

use tokio::sync::Notify;
use tracing::info;

use super::{EmbeddingProvider, NoopEmbedding, OpenAiEmbedding, ResilientEmbedding, resilient::ResilientConfig};
use crate::{
    clock::{Clock, SystemClock},
    config::{EmbeddingConfig, LimitsConfig},
    store::EmbeddingProfile,
};

/// Return the persisted vector-space identity for the active provider.
#[must_use]
pub fn active_embedding_profile(config: &EmbeddingConfig) -> Option<EmbeddingProfile> {
    match config {
        EmbeddingConfig::OpenAiCompatible { dimensions, openai_compatible } => Some(EmbeddingProfile::openai_compatible(
            openai_compatible.base_url.clone(),
            openai_compatible.model.clone(),
            *dimensions,
        )),
        EmbeddingConfig::Noop { .. } => None,
    }
}

/// Build the configured provider, including outage detection and recovery.
pub async fn create_embedding_provider(config: &EmbeddingConfig, limits: &LimitsConfig, recovery_notify: Option<Arc<Notify>>) -> Arc<dyn EmbeddingProvider> {
    create_embedding_provider_with_clock(config, limits, recovery_notify, Arc::new(SystemClock::new())).await
}

/// Build the configured provider with all deadlines driven by `clock`.
pub async fn create_embedding_provider_with_clock(
    config: &EmbeddingConfig,
    limits: &LimitsConfig,
    recovery_notify: Option<Arc<Notify>>,
    clock: Arc<dyn Clock>,
) -> Arc<dyn EmbeddingProvider> {
    match config {
        EmbeddingConfig::OpenAiCompatible { dimensions, openai_compatible } => {
            let timeout = Duration::from_secs(limits.embedding_timeout_secs);
            let provider = match OpenAiEmbedding::new_with_clock(openai_compatible, *dimensions, timeout, Arc::clone(&clock)) {
                Ok(provider) => provider,
                Err(error) => {
                    tracing::error!(%error, "openai-compatible embedding provider could not be created; falling back to noop");
                    return Arc::new(NoopEmbedding::new());
                }
            };
            let mut resilient_config = ResilientConfig {
                max_retries: limits.embedding_max_retries,
                initial_backoff: Duration::from_millis(limits.embedding_retry_initial_backoff_ms),
                max_backoff: Duration::from_millis(limits.embedding_retry_max_backoff_ms),
                ..ResilientConfig::default()
            };
            if let Some(notify) = recovery_notify {
                resilient_config = resilient_config.with_recovery_notify(notify);
            }
            let resilient = ResilientEmbedding::new_with_clock(provider, resilient_config, clock).await;
            info!(
                base_url = openai_compatible.base_url,
                model = openai_compatible.model,
                dimensions,
                available = resilient.is_available(),
                "openai-compatible embedding provider initialized"
            );
            Arc::new(resilient)
        }
        EmbeddingConfig::Noop { dimensions } => {
            info!(dimensions, "noop embedding provider initialized");
            Arc::new(NoopEmbedding::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::active_embedding_profile;
    use crate::config::{EmbeddingConfig, OpenAiCompatibleConfig};

    #[test]
    fn profile_is_present_only_for_vector_provider() {
        let config = EmbeddingConfig::OpenAiCompatible {
            dimensions: 384,
            openai_compatible: OpenAiCompatibleConfig {
                base_url: "https://embeddings.example/v1".into(),
                model: "embed-v1".into(),
                ..OpenAiCompatibleConfig::default()
            },
        };
        let profile = active_embedding_profile(&config);
        assert_eq!(profile.as_ref().map(|profile| profile.dimensions), Some(384));
        assert_eq!(profile.as_ref().map(|profile| profile.model.as_str()), Some("embed-v1"));

        assert!(active_embedding_profile(&EmbeddingConfig::Noop { dimensions: 384 }).is_none());
    }
}
