//! Reranker startup, health validation, fallback, and retry policy.

use std::sync::Arc;

use tracing::{info, warn};

use super::{
    RerankerError, RerankerProvider, download,
    onnx::OnnxReranker,
    policy::compiled_execution_providers,
    resilient::{ResilientReranker, ResilientRerankerConfig},
};

/// Resolved reranker artifact identity suitable for diagnostics.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RerankerModelIdentity {
    /// Immutable model revision or the direct-file marker.
    pub revision: String,
    /// Managed artifact profile or direct/custom marker.
    pub artifact: String,
    /// Configured numeric precision.
    pub precision: RerankerPrecision,
    /// Expected model artifact SHA-256, or `not_configured` for direct files.
    pub model_sha256: String,
    /// Expected tokenizer artifact SHA-256, or `not_configured` for direct files.
    pub tokenizer_sha256: String,
}
use crate::{
    clock::{Clock, SystemClock},
    config::{RerankerConfig, RerankerExecutionProvider, RerankerPrecision},
};

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
    initialize_with_retry_and_clock(config, Arc::new(SystemClock::new())).await
}

/// Initialize a reranker with retries driven by an injected clock.
///
/// # Errors
///
/// Returns model, provider, or inference errors using the same policy as
/// [`initialize_with_retry`].
pub async fn initialize_with_retry_and_clock(config: &RerankerConfig, clock: Arc<dyn Clock>) -> Result<InitializedReranker, RerankerError> {
    const MAX_RETRIES: u32 = 3;
    validate_precision_policy(config)?;
    initialize_ort()?;
    let mut delay = std::time::Duration::from_secs(2);
    for attempt in 0..MAX_RETRIES {
        match initialize(config, Arc::clone(&clock)).await {
            Ok(initialized) => return Ok(initialized),
            Err(error @ (RerankerError::Permanent(_) | RerankerError::ProviderUnavailable(_))) => return Err(error),
            Err(error) => {
                warn!("reranker init attempt {}/{MAX_RETRIES} failed: {error}, retrying in {delay:?}", attempt.saturating_add(1));
                clock.sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
        }
    }
    initialize(config, clock).await
}

/// Resolve the configured artifact identity without touching the model cache.
///
/// # Errors
///
/// Returns an error when an auto-downloaded model lacks immutable revision or
/// hash pins.
pub fn model_identity(config: &RerankerConfig) -> Result<RerankerModelIdentity, RerankerError> {
    validate_precision_policy(config)?;
    if !config.model_path.is_empty() {
        return Ok(RerankerModelIdentity {
            revision: if config.revision.is_empty() { "direct_file".into() } else { config.revision.clone() },
            artifact: "direct_file".into(),
            precision: config.precision,
            model_sha256: if config.model_sha256.is_empty() {
                "not_configured".into()
            } else {
                config.model_sha256.clone()
            },
            tokenizer_sha256: if config.tokenizer_sha256.is_empty() {
                "not_configured".into()
            } else {
                config.tokenizer_sha256.clone()
            },
        });
    }
    let pins = download::download_pins(config)?;
    Ok(RerankerModelIdentity {
        revision: pins.revision,
        artifact: pins.artifact,
        precision: config.precision,
        model_sha256: pins.model_sha256,
        tokenizer_sha256: pins.tokenizer_sha256,
    })
}

/// Initialize and run the normal inference health probe for diagnostics.
///
/// Without `allow_downloads`, only direct files or an already complete,
/// hash-verified cache entry are used.
///
/// # Errors
///
/// Returns model, provider, or inference errors from normal initialization.
pub async fn initialize_for_diagnostics(config: &RerankerConfig, allow_downloads: bool) -> Result<InitializedReranker, RerankerError> {
    initialize_for_diagnostics_with_clock(config, allow_downloads, Arc::new(SystemClock::new())).await
}

/// Initialize diagnostic inference with retries and probes driven by `clock`.
///
/// # Errors
///
/// Returns model, provider, download, or inference errors from normal
/// initialization.
pub async fn initialize_for_diagnostics_with_clock(config: &RerankerConfig, allow_downloads: bool, clock: Arc<dyn Clock>) -> Result<InitializedReranker, RerankerError> {
    validate_precision_policy(config)?;
    let _provider_candidates = crate::reranker::policy::execution_provider_candidates(config.execution_provider)?;
    if allow_downloads {
        return initialize_with_retry_and_clock(config, clock).await;
    }
    let paths = download::resolve_cached_model_paths(config)?;
    let mut local_config = config.clone();
    local_config.model_path = paths.onnx_path.to_string_lossy().into_owned();
    initialize_ort()?;
    initialize(&local_config, clock).await
}

fn initialize_ort() -> Result<(), RerankerError> {
    // commit installs the environment in ort's process-global singleton and
    // returns whether this call performed the one-time initialization.
    let _environment_inserted = ort::init().commit().map_err(|error| RerankerError::Permanent(Box::new(error)))?;
    Ok(())
}

async fn initialize(config: &RerankerConfig, clock: Arc<dyn Clock>) -> Result<InitializedReranker, RerankerError> {
    let mut initialized = load_provider(config, Arc::clone(&clock)).await?;

    if config.execution_provider == RerankerExecutionProvider::Auto
        && initialized.selected_execution_provider() == Some(RerankerExecutionProvider::Cuda)
        && initialized.active_execution_provider().is_none()
    {
        warn!("CUDA reranker failed initial health inference; auto policy falling back to CPU");
        let mut cpu_config = config.clone();
        cpu_config.execution_provider = RerankerExecutionProvider::Cpu;
        initialized = load_provider(&cpu_config, clock).await?;
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
        precision = %config.precision,
        required = config.required,
        selected = %selected.map_or_else(|| "none".into(), |provider| provider.to_string()),
        active = %active.map_or_else(|| "none".into(), |provider| provider.to_string()),
        "reranker initialized (available: {})",
        active.is_some()
    );

    Ok(InitializedReranker { provider: Arc::new(initialized) })
}

async fn load_provider(config: &RerankerConfig, clock: Arc<dyn Clock>) -> Result<ResilientReranker<OnnxReranker>, RerankerError> {
    let config = config.clone();
    let onnx = tokio::task::spawn_blocking(move || OnnxReranker::new(&config))
        .await
        .map_err(|error| RerankerError::Transient(Box::new(error)))??;
    Ok(ResilientReranker::new_with_clock(onnx, ResilientRerankerConfig::default(), clock).await)
}

fn validate_precision_policy(config: &RerankerConfig) -> Result<(), RerankerError> {
    if config.precision == RerankerPrecision::Fp16 && config.execution_provider != RerankerExecutionProvider::Cuda {
        return Err(RerankerError::Permanent(
            "reranker precision fp16 requires execution provider cuda; auto fallback and CPU execution are not supported".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp16_policy_requires_explicit_cuda() {
        for execution_provider in [RerankerExecutionProvider::Auto, RerankerExecutionProvider::Cpu] {
            let config = RerankerConfig {
                precision: RerankerPrecision::Fp16,
                execution_provider,
                ..RerankerConfig::default()
            };
            assert!(validate_precision_policy(&config).is_err());
        }

        let config = RerankerConfig {
            precision: RerankerPrecision::Fp16,
            execution_provider: RerankerExecutionProvider::Cuda,
            ..RerankerConfig::default()
        };
        validate_precision_policy(&config).unwrap();
    }
}
