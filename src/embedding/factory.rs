//! Construction of configured embedding providers and vector-space identity.

use std::{sync::Arc, time::Duration};

use tokio::sync::Notify;
use tracing::info;

use super::{BoxFuture, EmbeddingProvider, NoopEmbedding, OpenAiEmbedding, ResilientEmbedding, resilient::ResilientConfig};
use crate::{
    clock::{Clock, SystemClock},
    config::{EmbeddingConfig, LimitsConfig},
    error::EmbeddingError,
    store::EmbeddingProfile,
};

struct DeferredEmbedding {
    config: EmbeddingConfig,
    limits: LimitsConfig,
    clock: Arc<dyn Clock>,
    inner: tokio::sync::OnceCell<Arc<dyn EmbeddingProvider>>,
}

impl DeferredEmbedding {
    fn new(config: EmbeddingConfig, limits: LimitsConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            config,
            limits,
            clock,
            inner: tokio::sync::OnceCell::new(),
        }
    }

    async fn provider(&self) -> Arc<dyn EmbeddingProvider> {
        Arc::clone(
            self.inner
                .get_or_init(|| create_embedding_provider_with_clock(&self.config, &self.limits, None, Arc::clone(&self.clock)))
                .await,
        )
    }
}

impl EmbeddingProvider for DeferredEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            let provider = self.provider().await;
            provider.embed(text).await
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async move {
            let provider = self.provider().await;
            provider.health_check().await
        })
    }

    fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move {
            let provider = self.provider().await;
            provider.embed_batch(texts).await
        })
    }
}

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

/// Build a provider that performs no endpoint health check until embeddings
/// are first requested.
///
/// This is intended for read-only/browser flows where listing and keyword
/// search do not need an embedding endpoint.
#[must_use]
pub fn create_deferred_embedding_provider_with_clock(config: &EmbeddingConfig, limits: &LimitsConfig, clock: Arc<dyn Clock>) -> Arc<dyn EmbeddingProvider> {
    match config {
        EmbeddingConfig::Noop { .. } => Arc::new(NoopEmbedding::new()),
        EmbeddingConfig::OpenAiCompatible { .. } => Arc::new(DeferredEmbedding::new(config.clone(), limits.clone(), clock)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DeferredEmbedding, active_embedding_profile};
    use crate::{
        clock::SystemClock,
        config::{EmbeddingConfig, LimitsConfig, OpenAiCompatibleConfig},
    };

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

    #[test]
    fn deferred_provider_construction_does_not_initialize_its_endpoint() {
        let provider = DeferredEmbedding::new(
            EmbeddingConfig::OpenAiCompatible {
                dimensions: 384,
                openai_compatible: OpenAiCompatibleConfig {
                    base_url: "https://embeddings.invalid/v1".into(),
                    model: "embed-v1".into(),
                    ..OpenAiCompatibleConfig::default()
                },
            },
            LimitsConfig::default(),
            Arc::new(SystemClock::new()),
        );

        assert!(provider.inner.get().is_none(), "provider construction must not start endpoint initialization");
    }
}
