//! Resilient embedding provider with automatic availability recovery.
//!
//! [`ResilientEmbedding`] wraps an inner [`EmbeddingProvider`] and tracks its
//! availability via an [`AtomicBool`]. When retries exhaust a transient error,
//! the provider is marked unavailable and subsequent calls return
//! [`EmbeddingError::Disabled`] immediately. A background health-probe task
//! periodically checks connectivity and re-enables the provider when the
//! inner service recovers. Rate limits are retried without changing provider
//! availability.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::{sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{BoxFuture, EmbeddingProvider, retry::RetryPolicy};
use crate::{
    clock::{Clock, SystemClock},
    error::EmbeddingError,
};

/// Configuration for the resilient embedding wrapper.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResilientConfig {
    /// Interval between health-probe attempts when the provider is unavailable.
    pub probe_interval: std::time::Duration,
    /// Optional notifier signalled when the provider transitions from unavailable to available.
    /// Callers can wait on this to trigger recovery actions (e.g. bulk re-embed).
    pub recovery_notify: Option<Arc<Notify>>,
    /// Number of retries after an initial transient embedding failure.
    pub max_retries: u32,
    /// Delay before the first retry. Later retries use exponential backoff.
    pub initial_backoff: std::time::Duration,
    /// Maximum client or provider-directed delay that may be awaited.
    pub max_backoff: std::time::Duration,
}

impl Default for ResilientConfig {
    fn default() -> Self {
        Self {
            probe_interval: std::time::Duration::from_secs(30),
            recovery_notify: None,
            max_retries: 2,
            initial_backoff: std::time::Duration::from_millis(500),
            max_backoff: std::time::Duration::from_secs(30),
        }
    }
}

impl ResilientConfig {
    /// Set a [`Notify`] that will be signalled when the provider recovers from an outage.
    #[must_use]
    pub fn with_recovery_notify(mut self, notify: Arc<Notify>) -> Self {
        self.recovery_notify = Some(notify);
        self
    }
}

/// Embedding provider wrapper that tracks availability and auto-recovers.
///
/// When available, delegates to the inner provider. Exhausted transient errors
/// mark the provider as unavailable. A background task periodically probes
/// health and re-enables when the inner provider is reachable again.
///
/// Permanent errors (input-specific) do NOT affect availability.
///
/// The background probe task is cancelled when this struct is dropped.
pub struct ResilientEmbedding<P> {
    inner: Arc<P>,
    available: Arc<AtomicBool>,
    probe_handle: JoinHandle<()>,
    cancel: CancellationToken,
    retry_policy: RetryPolicy,
    clock: Arc<dyn Clock>,
}

impl<P: EmbeddingProvider + 'static> ResilientEmbedding<P> {
    /// Create a new resilient wrapper around the given provider.
    ///
    /// Runs an initial health check to set availability, then spawns a
    /// background probe task.
    pub async fn new(inner: P, config: ResilientConfig) -> Self {
        Self::new_with_clock(inner, config, Arc::new(SystemClock::new())).await
    }

    /// Create a resilient wrapper driven by an injected clock.
    pub async fn new_with_clock(inner: P, config: ResilientConfig, clock: Arc<dyn Clock>) -> Self {
        let inner = Arc::new(inner);
        let initial_health = inner.health_check().await;
        let initially_available = matches!(initial_health, Ok(()) | Err(EmbeddingError::RateLimited { .. }));

        match initial_health {
            Ok(()) => info!("resilient embedding: inner provider is available"),
            Err(EmbeddingError::RateLimited { .. }) => warn!("resilient embedding: health check was rate limited; treating provider as available"),
            Err(error) => warn!(%error, "resilient embedding: inner provider is unavailable, will probe periodically"),
        }

        let available = Arc::new(AtomicBool::new(initially_available));
        let cancel = CancellationToken::new();

        let retry_policy = RetryPolicy::new(config.max_retries, config.initial_backoff, config.max_backoff);

        let probe_handle = spawn_health_probe(
            Arc::clone(&inner),
            Arc::clone(&available),
            config.probe_interval,
            cancel.clone(),
            config.recovery_notify,
            Arc::clone(&clock),
        );

        Self {
            inner,
            available,
            probe_handle,
            cancel,
            retry_policy,
            clock,
        }
    }

    /// Whether the inner provider is currently considered available.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    /// Handle an embedding error, marking the provider unavailable on transient errors.
    ///
    /// Returns the error unchanged so callers can propagate it.
    fn handle_embed_error(&self, err: EmbeddingError) -> EmbeddingError {
        if let EmbeddingError::Transient(source) = &err {
            warn!("resilient embedding: transient error, marking unavailable: {source}");
            self.available.store(false, Ordering::Release);
        }
        err
    }

    async fn retry_transient<'a, T, F>(&'a self, mut operation: F) -> Result<T, EmbeddingError>
    where
        F: FnMut() -> BoxFuture<'a, Result<T, EmbeddingError>>,
    {
        let mut attempt = 0_u32;
        loop {
            let error = match operation().await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            if !error.is_retryable() || attempt >= self.retry_policy.max_retries() {
                return Err(self.handle_embed_error(error));
            }

            let Some(delay) = self.retry_policy.delay(attempt, error.retry_after()) else {
                warn!(%error, max_backoff = ?self.retry_policy.max_backoff(), "provider retry delay exceeds configured maximum; returning without retry");
                return Err(self.handle_embed_error(error));
            };
            warn!(attempt = attempt.saturating_add(1), max_retries = self.retry_policy.max_retries(), ?delay, error = %error, "embedding request failed; retrying");
            self.clock.sleep(delay).await;
            attempt = attempt.saturating_add(1);
        }
    }
}

impl<P> Drop for ResilientEmbedding<P> {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Defensively abort the probe task in case cancellation is not observed
        // promptly (e.g., blocked in a health_check call).
        self.probe_handle.abort();
    }
}

impl<P> std::fmt::Debug for ResilientEmbedding<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilientEmbedding")
            .field("available", &self.available.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl<P: EmbeddingProvider + 'static> EmbeddingProvider for ResilientEmbedding<P> {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            if !self.available.load(Ordering::Acquire) {
                return Err(EmbeddingError::Disabled);
            }

            self.retry_transient(|| self.inner.embed(text)).await
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async move {
            if !self.available.load(Ordering::Acquire) {
                return Err(EmbeddingError::Disabled);
            }
            self.inner.health_check().await
        })
    }

    fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move {
            if !self.available.load(Ordering::Acquire) {
                return Err(EmbeddingError::Disabled);
            }

            self.retry_transient(|| self.inner.embed_batch(texts)).await
        })
    }
}

/// Spawn a background task that periodically probes the inner provider's health.
/// When the provider is unavailable and a health check succeeds, it is marked
/// available again. The task exits when the cancellation token is cancelled.
#[expect(clippy::too_many_arguments, reason = "probe task owns provider state, cancellation, notification, interval, and clock")]
fn spawn_health_probe<P: EmbeddingProvider + 'static>(
    inner: Arc<P>,
    available: Arc<AtomicBool>,
    interval: std::time::Duration,
    cancel: CancellationToken,
    recovery_notify: Option<Arc<Notify>>,
    clock: Arc<dyn Clock>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            #[expect(clippy::integer_division_remainder_used, reason = "tokio::select! macro internally uses % for fairness")]
            {
                tokio::select! {
                    () = clock.sleep(interval) => {}
                    () = cancel.cancelled() => {
                        info!("resilient embedding: health probe task cancelled");
                        return;
                    }
                }
            }

            // Only probe when unavailable — available providers don't need probing.
            if available.load(Ordering::Acquire) {
                continue;
            }

            let recovered = match inner.health_check().await {
                Ok(()) | Err(EmbeddingError::RateLimited { .. }) => {
                    info!("resilient embedding: health probe reached provider, marking available");
                    available.store(true, Ordering::Release);
                    true
                }
                Err(e) => {
                    warn!("resilient embedding: health probe failed: {e}");
                    false
                }
            };
            if recovered && let Some(notify) = &recovery_notify {
                notify.notify_one();
            }
        }
    })
}

#[cfg(test)]
#[expect(unused_results, reason = "test setup and assertions discard many results intentionally")]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use futures::FutureExt as _;

    use super::{BoxFuture, ResilientConfig, ResilientEmbedding};
    use crate::{clock::MockClock, embedding::EmbeddingProvider, error::EmbeddingError};

    /// Mock provider that can be toggled between healthy/unhealthy states
    /// and tracks call counts.
    struct MockProvider {
        healthy: AtomicBool,
        embed_count: AtomicUsize,
        health_check_count: AtomicUsize,
    }

    impl MockProvider {
        fn new(initially_healthy: bool) -> Self {
            Self {
                healthy: AtomicBool::new(initially_healthy),
                embed_count: AtomicUsize::new(0),
                health_check_count: AtomicUsize::new(0),
            }
        }

        fn set_healthy(&self, healthy: bool) {
            self.healthy.store(healthy, Ordering::Release);
        }

        fn embed_sync(&self) -> Result<Vec<f32>, EmbeddingError> {
            self.embed_count.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Acquire) {
                Ok(vec![1.0, 0.0, 0.0])
            } else {
                Err(EmbeddingError::Transient("mock transient error".into()))
            }
        }

        fn health_check_sync(&self) -> Result<(), EmbeddingError> {
            self.health_check_count.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(EmbeddingError::Transient("mock health check failed".into()))
            }
        }
    }

    impl EmbeddingProvider for MockProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move { self.embed_sync() })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async move { self.health_check_sync() })
        }
    }

    fn fast_retry_config() -> ResilientConfig {
        ResilientConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            ..ResilientConfig::default()
        }
    }

    #[tokio::test]
    async fn initially_available_when_healthy() {
        let provider = MockProvider::new(true);
        let resilient = ResilientEmbedding::new(provider, ResilientConfig::default()).await;
        assert!(resilient.is_available(), "should be available when inner provider is healthy");
    }

    #[tokio::test]
    async fn initially_unavailable_when_unhealthy() {
        let provider = MockProvider::new(false);
        let resilient = ResilientEmbedding::new(provider, ResilientConfig::default()).await;
        assert!(!resilient.is_available(), "should be unavailable when inner provider is unhealthy");
    }

    #[tokio::test]
    async fn embed_delegates_when_available() {
        let provider = MockProvider::new(true);
        let resilient = ResilientEmbedding::new(provider, ResilientConfig::default()).await;
        let result = resilient.embed("test").await;
        assert!(result.is_ok(), "embed should succeed when available");
        assert_eq!(result.unwrap(), vec![1.0, 0.0, 0.0]);
    }

    #[tokio::test]
    async fn embed_returns_disabled_when_unavailable() {
        let provider = MockProvider::new(false);
        let resilient = ResilientEmbedding::new(provider, ResilientConfig::default()).await;
        let err = resilient.embed("test").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Disabled), "should return Disabled when unavailable");
    }

    #[tokio::test]
    async fn transient_error_marks_unavailable() {
        let provider = MockProvider::new(true);
        let resilient = ResilientEmbedding::new(provider, fast_retry_config()).await;
        assert!(resilient.is_available(), "should start available");

        // Make inner provider unhealthy, then embed
        resilient.inner.set_healthy(false);
        let err = resilient.embed("test").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Transient(_)), "should return transient error");
        assert!(!resilient.is_available(), "should be marked unavailable after transient error");

        // Subsequent calls should return Disabled without hitting inner
        let embed_count_before = resilient.inner.embed_count.load(Ordering::Relaxed);
        let err = resilient.embed("test2").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Disabled), "should return Disabled on subsequent calls");
        let embed_count_after = resilient.inner.embed_count.load(Ordering::Relaxed);
        assert_eq!(embed_count_before, embed_count_after, "should not call inner embed when unavailable");
    }

    struct FlakyProvider {
        failures_remaining: AtomicUsize,
        embed_count: AtomicUsize,
    }

    struct RateLimitedProvider {
        failures_remaining: AtomicUsize,
        embed_count: AtomicUsize,
        health_rate_limited: bool,
        retry_after: Option<Duration>,
    }

    impl RateLimitedProvider {
        fn rate_limit_error(&self) -> EmbeddingError {
            EmbeddingError::RateLimited {
                source: "quota exceeded".into(),
                retry_after: self.retry_after,
            }
        }

        fn embed_sync(&self) -> Result<Vec<f32>, EmbeddingError> {
            self.embed_count.fetch_add(1, Ordering::Relaxed);
            let failed = self
                .failures_remaining
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| remaining.checked_sub(1))
                .is_ok();
            if failed { Err(self.rate_limit_error()) } else { Ok(vec![1.0, 0.0, 0.0]) }
        }

        fn health_check_sync(&self) -> Result<(), EmbeddingError> {
            if self.health_rate_limited { Err(self.rate_limit_error()) } else { Ok(()) }
        }
    }

    impl EmbeddingProvider for RateLimitedProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move { self.embed_sync() })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async move { self.health_check_sync() })
        }
    }

    impl FlakyProvider {
        fn embed_sync(&self) -> Result<Vec<f32>, EmbeddingError> {
            self.embed_count.fetch_add(1, Ordering::Relaxed);
            let failed = self
                .failures_remaining
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| remaining.checked_sub(1))
                .is_ok();
            if failed {
                return Err(EmbeddingError::Transient("retryable failure".into()));
            }
            Ok(vec![1.0, 0.0, 0.0])
        }
    }

    impl EmbeddingProvider for FlakyProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move { self.embed_sync() })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn retries_transient_failures_before_opening_circuit() {
        let resilient = ResilientEmbedding::new(
            FlakyProvider {
                failures_remaining: AtomicUsize::new(2),
                embed_count: AtomicUsize::new(0),
            },
            fast_retry_config(),
        )
        .await;

        assert_eq!(resilient.embed("test").await.unwrap(), vec![1.0, 0.0, 0.0]);
        assert_eq!(resilient.inner.embed_count.load(Ordering::Relaxed), 3);
        assert!(resilient.is_available(), "successful retry must keep the circuit closed");
    }

    #[tokio::test]
    async fn retries_rate_limits_without_opening_circuit() {
        let resilient = ResilientEmbedding::new(
            RateLimitedProvider {
                failures_remaining: AtomicUsize::new(2),
                embed_count: AtomicUsize::new(0),
                health_rate_limited: false,
                retry_after: None,
            },
            fast_retry_config(),
        )
        .await;

        assert_eq!(resilient.embed("test").await.unwrap(), vec![1.0, 0.0, 0.0]);
        assert_eq!(resilient.inner.embed_count.load(Ordering::Relaxed), 3);
        assert!(resilient.is_available());
    }

    #[tokio::test]
    async fn exhausted_rate_limit_keeps_circuit_available() {
        let resilient = ResilientEmbedding::new(
            RateLimitedProvider {
                failures_remaining: AtomicUsize::new(3),
                embed_count: AtomicUsize::new(0),
                health_rate_limited: false,
                retry_after: None,
            },
            fast_retry_config(),
        )
        .await;

        let error = resilient.embed("test").await.unwrap_err();
        assert!(matches!(error, EmbeddingError::RateLimited { .. }));
        assert_eq!(resilient.inner.embed_count.load(Ordering::Relaxed), 3);
        assert!(resilient.is_available());
    }

    #[tokio::test]
    async fn provider_delay_over_cap_skips_retry_without_opening_circuit() {
        let config = ResilientConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            ..ResilientConfig::default()
        };
        let resilient = ResilientEmbedding::new(
            RateLimitedProvider {
                failures_remaining: AtomicUsize::new(1),
                embed_count: AtomicUsize::new(0),
                health_rate_limited: false,
                retry_after: Some(Duration::from_millis(6)),
            },
            config,
        )
        .await;

        let error = resilient.embed("test").await.unwrap_err();
        assert!(matches!(error, EmbeddingError::RateLimited { .. }));
        assert_eq!(resilient.inner.embed_count.load(Ordering::Relaxed), 1);
        assert!(resilient.is_available());
    }

    #[tokio::test]
    async fn rate_limited_initial_health_is_available() {
        let resilient = ResilientEmbedding::new(
            RateLimitedProvider {
                failures_remaining: AtomicUsize::new(0),
                embed_count: AtomicUsize::new(0),
                health_rate_limited: true,
                retry_after: None,
            },
            fast_retry_config(),
        )
        .await;

        assert!(resilient.is_available());
        assert_eq!(resilient.embed("test").await.unwrap(), vec![1.0, 0.0, 0.0]);
    }

    /// Provider that returns permanent errors but is otherwise healthy.
    struct PermanentErrorProvider;

    impl EmbeddingProvider for PermanentErrorProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Err(EmbeddingError::Permanent("bad input".into())) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn permanent_error_does_not_mark_unavailable() {
        let resilient = ResilientEmbedding::new(PermanentErrorProvider, ResilientConfig::default()).await;
        assert!(resilient.is_available(), "should start available");

        let err = resilient.embed("test").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Permanent(_)), "should forward permanent error");
        assert!(resilient.is_available(), "should still be available after permanent error");
    }

    #[tokio::test]
    async fn health_probe_recovers_availability() {
        let provider = Arc::new(MockProvider::new(false));
        let config = ResilientConfig {
            probe_interval: Duration::from_millis(20),
            recovery_notify: None,
            ..ResilientConfig::default()
        };
        let clock = Arc::new(MockClock::new());
        let resilient = ResilientEmbedding::new_with_clock(HealthProbeWrapper { inner: Arc::clone(&provider) }, config, Arc::<MockClock>::clone(&clock)).await;
        assert!(!resilient.is_available(), "should start unavailable");

        // Make provider healthy, then advance directly to the probe deadline.
        provider.set_healthy(true);
        tokio::task::yield_now().await;
        clock.advance(chrono::TimeDelta::milliseconds(20));
        tokio::task::yield_now().await;

        assert!(resilient.is_available(), "should recover after health probe succeeds");
        let result = resilient.embed("test").await;
        assert!(result.is_ok(), "embed should succeed after recovery");
    }

    /// Wrapper so we can share the `Arc<MockProvider>` between test code and
    /// the resilient wrapper.
    struct HealthProbeWrapper {
        inner: Arc<MockProvider>,
    }

    impl EmbeddingProvider for HealthProbeWrapper {
        fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move { self.inner.embed(text).await })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async move { self.inner.health_check().await })
        }
    }

    #[tokio::test]
    async fn embed_batch_delegates_when_available() {
        let provider = MockProvider::new(true);
        let resilient = ResilientEmbedding::new(provider, ResilientConfig::default()).await;
        let texts: &[&str] = &["a", "b"];
        let result = resilient.embed_batch(texts).await;
        assert!(result.is_ok(), "embed_batch should succeed when available");
        assert_eq!(result.unwrap().len(), 2, "should return one embedding per input");
    }

    #[tokio::test]
    async fn embed_batch_returns_disabled_when_unavailable() {
        let provider = MockProvider::new(false);
        let resilient = ResilientEmbedding::new(provider, ResilientConfig::default()).await;
        let texts: &[&str] = &["a", "b"];
        let err = resilient.embed_batch(texts).await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Disabled), "should return Disabled when unavailable");
    }

    #[tokio::test]
    async fn embed_batch_transient_marks_unavailable() {
        let provider = MockProvider::new(true);
        let resilient = ResilientEmbedding::new(provider, fast_retry_config()).await;
        assert!(resilient.is_available(), "should start available");

        resilient.inner.set_healthy(false);
        let texts: &[&str] = &["a"];
        let err = resilient.embed_batch(texts).await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Transient(_)), "should return transient error");
        assert!(!resilient.is_available(), "should be marked unavailable after transient batch error");
    }

    #[tokio::test]
    async fn drop_cancels_probe_task() {
        let provider = MockProvider::new(false);
        let config = ResilientConfig {
            probe_interval: Duration::from_millis(10),
            recovery_notify: None,
            ..ResilientConfig::default()
        };
        let resilient = ResilientEmbedding::new(provider, config).await;

        // Clone the cancellation token before dropping
        let cancel = resilient.cancel.clone();
        assert!(!cancel.is_cancelled(), "cancellation token should not be cancelled before drop");
        drop(resilient);

        // The cancellation token should be cancelled
        assert!(cancel.is_cancelled(), "cancellation token should be cancelled after drop");
    }

    #[tokio::test]
    async fn recovery_notify_fires_on_availability_transition() {
        let provider = Arc::new(MockProvider::new(false));
        let notify = Arc::new(tokio::sync::Notify::new());
        let config = ResilientConfig {
            probe_interval: Duration::from_millis(20),
            recovery_notify: Some(Arc::clone(&notify)),
            ..ResilientConfig::default()
        };
        let clock = Arc::new(MockClock::new());
        let resilient = ResilientEmbedding::new_with_clock(HealthProbeWrapper { inner: Arc::clone(&provider) }, config, Arc::<MockClock>::clone(&clock)).await;
        assert!(!resilient.is_available(), "should start unavailable");

        // Register before advancing so the transition cannot be missed.
        let notified = notify.notified();
        tokio::pin!(notified);
        provider.set_healthy(true);
        tokio::task::yield_now().await;
        clock.advance(chrono::TimeDelta::milliseconds(20));
        notified.await;

        assert!(resilient.is_available(), "should be available after recovery");
    }

    #[tokio::test]
    async fn recovery_notify_does_not_fire_when_already_available() {
        let provider = Arc::new(MockProvider::new(true));
        let notify = Arc::new(tokio::sync::Notify::new());
        let config = ResilientConfig {
            probe_interval: Duration::from_millis(20),
            recovery_notify: Some(Arc::clone(&notify)),
            ..ResilientConfig::default()
        };
        let clock = Arc::new(MockClock::new());
        let _resilient = ResilientEmbedding::new_with_clock(HealthProbeWrapper { inner: Arc::clone(&provider) }, config, Arc::<MockClock>::clone(&clock)).await;

        tokio::task::yield_now().await;
        clock.advance(chrono::TimeDelta::milliseconds(20));
        tokio::task::yield_now().await;
        assert!(
            notify.notified().now_or_never().is_none(),
            "recovery_notify should not fire when provider is already available"
        );
    }
}
