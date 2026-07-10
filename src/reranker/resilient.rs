//! Resilient reranker provider with automatic availability recovery.
//!
//! [`ResilientReranker`] wraps an inner [`RerankerProvider`] and tracks its
//! availability via an [`AtomicBool`]. When a transient error occurs, the
//! provider is marked unavailable and subsequent calls return
//! [`RerankerError::Unavailable`] immediately. A background health-probe task
//! periodically checks functionality and re-enables the provider when the
//! inner reranker recovers.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{BoxFuture, RerankerError, RerankerProvider, RerankerScore};

/// Configuration for the resilient reranker wrapper.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResilientRerankerConfig {
    /// Interval between health-probe attempts when the provider is unavailable.
    pub probe_interval: std::time::Duration,
}

impl Default for ResilientRerankerConfig {
    fn default() -> Self {
        Self {
            probe_interval: std::time::Duration::from_secs(30),
        }
    }
}

/// Reranker provider wrapper that tracks availability and auto-recovers.
///
/// When available, delegates to the inner provider. Transient errors mark the
/// provider as unavailable. A background task periodically probes health and
/// re-enables when the inner provider is functional again.
///
/// Permanent errors (model-specific) do NOT affect availability.
///
/// The background probe task is cancelled when this struct is dropped.
pub struct ResilientReranker<P> {
    inner: Arc<P>,
    available: Arc<AtomicBool>,
    probe_abort_handle: AbortHandle,
    cancel: CancellationToken,
}

impl<P: RerankerProvider + 'static> ResilientReranker<P> {
    /// Create a new resilient wrapper around the given provider.
    ///
    /// Runs an initial health check to set availability, then spawns a
    /// background probe task.
    pub async fn new(inner: P, config: ResilientRerankerConfig) -> Self {
        let inner = Arc::new(inner);
        let initially_available = inner.health_check().await.is_ok();

        if initially_available {
            info!("resilient reranker: inner provider is available");
        } else {
            warn!("resilient reranker: inner provider is unavailable, will probe periodically");
        }

        let available = Arc::new(AtomicBool::new(initially_available));
        let cancel = CancellationToken::new();

        let probe_abort_handle = spawn_health_probe(Arc::clone(&inner), Arc::clone(&available), config.probe_interval, cancel.clone());

        Self {
            inner,
            available,
            probe_abort_handle,
            cancel,
        }
    }

    /// Whether the inner provider is currently considered available.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    /// Handle a reranker error, marking the provider unavailable on transient errors.
    ///
    /// Returns the error unchanged so callers can propagate it.
    fn handle_error(&self, err: RerankerError) -> RerankerError {
        if let RerankerError::Transient(source) = &err {
            warn!("resilient reranker: transient error, marking unavailable: {source}");
            self.available.store(false, Ordering::Release);
        }
        err
    }
}

impl<P> Drop for ResilientReranker<P> {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Defensively abort the probe task in case cancellation is not observed
        // promptly (e.g., blocked in a health_check call).
        self.probe_abort_handle.abort();
    }
}

impl<P> std::fmt::Debug for ResilientReranker<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResilientReranker")
            .field("available", &self.available.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl<P: RerankerProvider + 'static> RerankerProvider for ResilientReranker<P> {
    fn rerank<'a>(&'a self, query: &'a str, documents: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>> {
        Box::pin(async move {
            if !self.available.load(Ordering::Acquire) {
                return Err(RerankerError::Unavailable);
            }

            self.inner.rerank(query, documents).await.map_err(|e| self.handle_error(e))
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), RerankerError>> {
        Box::pin(async move {
            if !self.available.load(Ordering::Acquire) {
                return Err(RerankerError::Unavailable);
            }
            self.inner.health_check().await
        })
    }
}

/// Spawn a background task that periodically probes the inner provider's health.
/// When the provider is unavailable and a health check succeeds, it is marked
/// available again. The task exits when the cancellation token is cancelled.
fn spawn_health_probe<P: RerankerProvider + 'static>(inner: Arc<P>, available: Arc<AtomicBool>, interval: std::time::Duration, cancel: CancellationToken) -> AbortHandle {
    tokio::spawn(async move {
        loop {
            #[expect(clippy::integer_division_remainder_used, reason = "tokio::select! macro internally uses % for fairness")]
            {
                tokio::select! {
                    () = tokio::time::sleep(interval) => {}
                    () = cancel.cancelled() => {
                        info!("resilient reranker: health probe task cancelled");
                        return;
                    }
                }
            }

            // Only probe when unavailable — available providers don't need probing.
            if available.load(Ordering::Acquire) {
                continue;
            }

            match inner.health_check().await {
                Ok(()) => {
                    info!("resilient reranker: health probe succeeded, marking available");
                    available.store(true, Ordering::Release);
                }
                Err(e) => {
                    warn!("resilient reranker: health probe failed: {e}");
                }
            }
        }
    })
    .abort_handle()
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

    use super::{BoxFuture, ResilientReranker, ResilientRerankerConfig};
    use crate::reranker::{RerankerError, RerankerProvider, RerankerScore};

    /// Mock provider that can be toggled between healthy/unhealthy states
    /// and tracks call counts.
    struct MockReranker {
        healthy: AtomicBool,
        rerank_count: AtomicUsize,
        health_check_count: AtomicUsize,
    }

    impl MockReranker {
        fn new(initially_healthy: bool) -> Self {
            Self {
                healthy: AtomicBool::new(initially_healthy),
                rerank_count: AtomicUsize::new(0),
                health_check_count: AtomicUsize::new(0),
            }
        }

        fn set_healthy(&self, healthy: bool) {
            self.healthy.store(healthy, Ordering::Release);
        }

        fn rerank_sync(&self, documents: &[&str]) -> Result<Vec<RerankerScore>, RerankerError> {
            self.rerank_count.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Acquire) {
                Ok(documents.iter().enumerate().map(|(i, _)| RerankerScore { index: i, score: 0.5_f64 }).collect())
            } else {
                Err(RerankerError::Transient("mock transient error".into()))
            }
        }

        fn health_check_sync(&self) -> Result<(), RerankerError> {
            self.health_check_count.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(RerankerError::Transient("mock health check failed".into()))
            }
        }
    }

    impl RerankerProvider for MockReranker {
        fn rerank<'a>(&'a self, _query: &'a str, documents: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>> {
            Box::pin(async move { self.rerank_sync(documents) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), RerankerError>> {
            Box::pin(async move { self.health_check_sync() })
        }
    }

    #[tokio::test]
    async fn initially_available_when_healthy() {
        let provider = MockReranker::new(true);
        let resilient = ResilientReranker::new(provider, ResilientRerankerConfig::default()).await;
        assert!(resilient.is_available(), "should be available when inner provider is healthy");
    }

    #[tokio::test]
    async fn initially_unavailable_when_unhealthy() {
        let provider = MockReranker::new(false);
        let resilient = ResilientReranker::new(provider, ResilientRerankerConfig::default()).await;
        assert!(!resilient.is_available(), "should be unavailable when inner provider is unhealthy");
    }

    #[tokio::test]
    async fn rerank_delegates_when_available() {
        let provider = MockReranker::new(true);
        let resilient = ResilientReranker::new(provider, ResilientRerankerConfig::default()).await;
        let docs: &[&str] = &["doc1", "doc2"];
        let result = resilient.rerank("query", docs).await;
        assert!(result.is_ok(), "rerank should succeed when available");
        assert_eq!(result.unwrap().len(), 2, "should return one score per document");
    }

    #[tokio::test]
    async fn rerank_returns_unavailable_when_down() {
        let provider = MockReranker::new(false);
        let resilient = ResilientReranker::new(provider, ResilientRerankerConfig::default()).await;
        let docs: &[&str] = &["doc1"];
        let err = resilient.rerank("query", docs).await.unwrap_err();
        assert!(matches!(err, RerankerError::Unavailable), "should return Unavailable when down");
    }

    #[tokio::test]
    async fn transient_error_marks_unavailable() {
        let provider = MockReranker::new(true);
        let resilient = ResilientReranker::new(provider, ResilientRerankerConfig::default()).await;
        assert!(resilient.is_available(), "should start available");

        // Make inner provider unhealthy, then rerank
        resilient.inner.set_healthy(false);
        let docs: &[&str] = &["doc1"];
        let err = resilient.rerank("query", docs).await.unwrap_err();
        assert!(matches!(err, RerankerError::Transient(_)), "should return transient error");
        assert!(!resilient.is_available(), "should be marked unavailable after transient error");

        // Subsequent calls should return Unavailable without hitting inner
        let rerank_count_before = resilient.inner.rerank_count.load(Ordering::Relaxed);
        let err = resilient.rerank("query", docs).await.unwrap_err();
        assert!(matches!(err, RerankerError::Unavailable), "should return Unavailable on subsequent calls");
        let rerank_count_after = resilient.inner.rerank_count.load(Ordering::Relaxed);
        assert_eq!(rerank_count_before, rerank_count_after, "should not call inner rerank when unavailable");
    }

    /// Provider that returns permanent errors but is otherwise healthy.
    struct PermanentErrorReranker;

    impl RerankerProvider for PermanentErrorReranker {
        fn rerank<'a>(&'a self, _query: &'a str, _documents: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>> {
            Box::pin(async { Err(RerankerError::Permanent("bad input".into())) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), RerankerError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn permanent_error_does_not_mark_unavailable() {
        let resilient = ResilientReranker::new(PermanentErrorReranker, ResilientRerankerConfig::default()).await;
        assert!(resilient.is_available(), "should start available");

        let docs: &[&str] = &["doc1"];
        let err = resilient.rerank("query", docs).await.unwrap_err();
        assert!(matches!(err, RerankerError::Permanent(_)), "should forward permanent error");
        assert!(resilient.is_available(), "should still be available after permanent error");
    }

    /// Wrapper so we can share the `Arc<MockReranker>` between test code and
    /// the resilient wrapper.
    struct HealthProbeWrapper {
        inner: Arc<MockReranker>,
    }

    impl RerankerProvider for HealthProbeWrapper {
        fn rerank<'a>(&'a self, query: &'a str, documents: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>> {
            Box::pin(async move { self.inner.rerank(query, documents).await })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), RerankerError>> {
            Box::pin(async move { self.inner.health_check().await })
        }
    }

    #[tokio::test]
    async fn health_probe_recovers_availability() {
        let provider = Arc::new(MockReranker::new(false));
        let config = ResilientRerankerConfig {
            probe_interval: Duration::from_millis(20),
        };
        let resilient = ResilientReranker::new(HealthProbeWrapper { inner: Arc::clone(&provider) }, config).await;
        assert!(!resilient.is_available(), "should start unavailable");

        // Make provider healthy, then wait for probe
        provider.set_healthy(true);
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(resilient.is_available(), "should recover after health probe succeeds");
        let docs: &[&str] = &["doc1"];
        let result = resilient.rerank("query", docs).await;
        assert!(result.is_ok(), "rerank should succeed after recovery");
    }

    #[tokio::test]
    async fn drop_cancels_probe_task() {
        let provider = MockReranker::new(false);
        let config = ResilientRerankerConfig {
            probe_interval: Duration::from_millis(10),
        };
        let resilient = ResilientReranker::new(provider, config).await;

        // Clone the cancellation token before dropping
        let cancel = resilient.cancel.clone();
        assert!(!cancel.is_cancelled(), "cancellation token should not be cancelled before drop");
        drop(resilient);

        // The cancellation token should be cancelled
        assert!(cancel.is_cancelled(), "cancellation token should be cancelled after drop");
    }
}
