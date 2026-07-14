//! Reranker startup, health validation, fallback, and retry policy.

use std::sync::{Arc, Mutex};

use tracing::{info, warn};

use super::{
    RerankerError, RerankerProvider, download,
    onnx::OnnxReranker,
    policy::{compiled_execution_providers, validate_precision_policy},
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
    #[cfg(feature = "reranker-cuda")]
    preload_dynamic_ort_library()?;
    // commit installs the environment in ort's process-global singleton and
    // returns whether this call performed the one-time initialization.
    let initialized = commit_ort_environment_without_panic_output().map_err(|_panic| RerankerError::ProviderUnavailable(onnx_runtime_panic_guidance().into()))?;
    let _environment_inserted = initialized.map_err(|error| RerankerError::Permanent(Box::new(error)))?;
    Ok(())
}

type PanicHook = dyn for<'a> Fn(&std::panic::PanicHookInfo<'a>) + Send + Sync + 'static;

fn commit_ort_environment_without_panic_output() -> std::thread::Result<ort::Result<bool>> {
    static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

    // Panic hooks are process-global. Serialize our temporary replacement and
    // suppress output only for this thread; concurrent panics still reach the
    // hook that was installed before ORT initialization.
    let _hook_guard = PANIC_HOOK_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let initializing_thread = std::thread::current().id();
    let previous_hook: Arc<PanicHook> = Arc::from(std::panic::take_hook());
    let delegated_hook = Arc::clone(&previous_hook);
    std::panic::set_hook(Box::new(move |panic_info| {
        if std::thread::current().id() != initializing_thread {
            delegated_hook(panic_info);
        }
    }));
    let initialized = std::panic::catch_unwind(|| ort::init().commit());
    std::panic::set_hook(Box::new(move |panic_info| previous_hook(panic_info)));
    initialized
}

#[cfg(feature = "reranker-cuda")]
fn preload_dynamic_ort_library() -> Result<(), RerankerError> {
    let configured = std::env::var_os("ORT_DYLIB_PATH").filter(|value| !value.is_empty());
    let executable = std::env::current_exe().ok();
    let path = resolve_dynamic_ort_library_path(configured.as_deref(), executable.as_deref());
    ort::util::preload_dylib(&path).map_err(|error| {
        RerankerError::ProviderUnavailable(format!(
            "ONNX Runtime could not load `{}` or one of its dependencies: {error}; set ORT_DYLIB_PATH to the absolute library path or configure the platform loader",
            path.display()
        ))
    })
}

#[cfg(feature = "reranker-cuda")]
fn resolve_dynamic_ort_library_path(configured: Option<&std::ffi::OsStr>, executable: Option<&std::path::Path>) -> std::path::PathBuf {
    let path = configured.map_or_else(|| std::path::PathBuf::from(dynamic_ort_library_name()), std::path::PathBuf::from);
    if path.is_absolute() {
        return path;
    }
    executable
        .and_then(std::path::Path::parent)
        .map(|parent| parent.join(&path))
        .filter(|relative| relative.exists())
        .unwrap_or(path)
}

#[cfg(feature = "reranker-cuda")]
const fn onnx_runtime_panic_guidance() -> &'static str {
    "ONNX Runtime initialization failed unexpectedly after loading its dynamic library; verify that ORT_DYLIB_PATH selects a compatible ONNX Runtime build"
}

#[cfg(not(feature = "reranker-cuda"))]
const fn onnx_runtime_panic_guidance() -> &'static str {
    "ONNX Runtime initialization failed unexpectedly"
}

#[cfg(all(feature = "reranker-cuda", target_os = "windows"))]
const fn dynamic_ort_library_name() -> &'static str {
    "onnxruntime.dll"
}

#[cfg(all(feature = "reranker-cuda", any(target_os = "linux", target_os = "android")))]
const fn dynamic_ort_library_name() -> &'static str {
    "libonnxruntime.so"
}

#[cfg(all(feature = "reranker-cuda", any(target_os = "macos", target_os = "ios")))]
const fn dynamic_ort_library_name() -> &'static str {
    "libonnxruntime.dylib"
}

#[cfg(all(
    feature = "reranker-cuda",
    not(any(target_os = "windows", target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios"))
))]
const fn dynamic_ort_library_name() -> &'static str {
    "libonnxruntime.so"
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

#[cfg(all(test, feature = "reranker-cuda"))]
mod tests {
    use super::*;

    #[test]
    fn dynamic_ort_path_resolves_configured_path() {
        let root = tempfile::TempDir::new().unwrap();
        let configured = root.path().join("configured/libonnxruntime-test.so");
        assert_eq!(resolve_dynamic_ort_library_path(Some(configured.as_os_str()), None), configured);
    }

    #[test]
    fn dynamic_ort_path_prefers_library_beside_executable() {
        let root = tempfile::TempDir::new().unwrap();
        let executable = root.path().join("bin/hold");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        let library = executable.parent().unwrap().join(dynamic_ort_library_name());
        std::fs::write(&library, b"fixture").unwrap();
        assert_eq!(resolve_dynamic_ort_library_path(None, Some(&executable)), library);
    }
}
