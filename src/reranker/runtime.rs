//! Reranker startup, health validation, fallback, and retry policy.

use std::sync::Arc;

use tracing::{info, warn};

use super::{
    RerankerError, RerankerProvider,
    onnx::OnnxReranker,
    policy::compiled_execution_providers,
    resilient::{ResilientReranker, ResilientRerankerConfig},
};
use crate::config::{RerankerConfig, RerankerExecutionProvider};

/// Successfully initialized reranker provider.
pub struct InitializedReranker {
    provider: Arc<dyn RerankerProvider>,
}

impl std::fmt::Debug for InitializedReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitializedReranker")
            .field("selected_execution_provider", &self.provider.selected_execution_provider())
            .field("active_execution_provider", &self.provider.active_execution_provider())
            .finish()
    }
}

impl InitializedReranker {
    /// Concrete provider selected while constructing the model session.
    #[must_use]
    pub fn selected_execution_provider(&self) -> Option<RerankerExecutionProvider> {
        self.provider.selected_execution_provider()
    }

    /// Provider currently available after health inference.
    #[must_use]
    pub fn active_execution_provider(&self) -> Option<RerankerExecutionProvider> {
        self.provider.active_execution_provider()
    }

    /// Consume the result and return the provider for attachment to the engine.
    #[must_use]
    pub fn into_provider(self) -> Arc<dyn RerankerProvider> {
        self.provider
    }
}

/// Initialize a reranker, retrying only transient construction failures.
///
/// # Errors
///
/// Returns immediately for permanent model errors, unavailable explicitly
/// requested providers, and failed required-mode health inference.
pub async fn initialize_with_retry(config: &RerankerConfig) -> Result<InitializedReranker, RerankerError> {
    const MAX_RETRIES: u32 = 3;
    // commit installs the environment in ort's process-global singleton and
    // returns whether this call performed the one-time initialization.
    let _environment_inserted = ort::init().commit().map_err(|error| RerankerError::Permanent(Box::new(error)))?;
    let mut delay = std::time::Duration::from_secs(2);
    for attempt in 0..MAX_RETRIES {
        match initialize(config).await {
            Ok(initialized) => return Ok(initialized),
            Err(error @ (RerankerError::Permanent(_) | RerankerError::ProviderUnavailable(_))) => return Err(error),
            Err(error) => {
                warn!("reranker init attempt {}/{MAX_RETRIES} failed: {error}, retrying in {delay:?}", attempt.saturating_add(1));
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
        }
    }
    initialize(config).await
}

async fn initialize(config: &RerankerConfig) -> Result<InitializedReranker, RerankerError> {
    let mut initialized = load_provider(config).await?;

    if config.execution_provider == RerankerExecutionProvider::Auto
        && initialized.selected_execution_provider() == Some(RerankerExecutionProvider::Cuda)
        && initialized.active_execution_provider().is_none()
    {
        warn!("CUDA reranker failed initial health inference; auto policy falling back to CPU");
        let mut cpu_config = config.clone();
        cpu_config.execution_provider = RerankerExecutionProvider::Cpu;
        initialized = load_provider(&cpu_config).await?;
    }

    let selected = initialized.selected_execution_provider();
    let active = initialized.active_execution_provider();
    if config.required && active.is_none() {
        return Err(RerankerError::ProviderUnavailable(format!(
            "{} was selected but failed initial health inference while reranker.required = true",
            selected.map_or_else(|| "no provider".into(), |provider| provider.to_string())
        )));
    }

    let compiled = compiled_execution_providers().iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    info!(
        %compiled,
        requested = %config.execution_provider,
        required = config.required,
        selected = %selected.map_or_else(|| "none".into(), |provider| provider.to_string()),
        active = %active.map_or_else(|| "none".into(), |provider| provider.to_string()),
        "reranker initialized (available: {})",
        active.is_some()
    );

    Ok(InitializedReranker { provider: Arc::new(initialized) })
}

async fn load_provider(config: &RerankerConfig) -> Result<ResilientReranker<OnnxReranker>, RerankerError> {
    let config = config.clone();
    let onnx = tokio::task::spawn_blocking(move || OnnxReranker::new(&config))
        .await
        .map_err(|error| RerankerError::Transient(Box::new(error)))??;
    Ok(ResilientReranker::new(onnx, ResilientRerankerConfig::default()).await)
}
