//! Shared concurrency limit for all embedding provider operations.

use std::sync::Arc;

use tokio::sync::Semaphore;

use super::{BoxFuture, EmbeddingProvider};
use crate::error::EmbeddingError;

/// Bounds concurrent calls to an embedding provider.
pub(crate) struct ConcurrencyLimitedEmbedding {
    inner: Arc<dyn EmbeddingProvider>,
    permits: Semaphore,
}

impl ConcurrencyLimitedEmbedding {
    /// Wrap a provider with a shared request limit.
    #[must_use]
    pub(crate) fn new(inner: Arc<dyn EmbeddingProvider>, max_concurrent_requests: usize) -> Self {
        Self {
            inner,
            permits: Semaphore::new(max_concurrent_requests.max(1)),
        }
    }

    async fn embed_impl(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let _permit = self.permits.acquire().await.map_err(|_closed| EmbeddingError::Disabled)?;
        self.inner.embed(text).await
    }

    async fn embed_batch_impl(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let _permit = self.permits.acquire().await.map_err(|_closed| EmbeddingError::Disabled)?;
        self.inner.embed_batch(texts).await
    }

    async fn health_check_impl(&self) -> Result<(), EmbeddingError> {
        let _permit = self.permits.acquire().await.map_err(|_closed| EmbeddingError::Disabled)?;
        self.inner.health_check().await
    }
}

impl std::fmt::Debug for ConcurrencyLimitedEmbedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcurrencyLimitedEmbedding")
            .field("available_permits", &self.permits.available_permits())
            .finish_non_exhaustive()
    }
}

impl EmbeddingProvider for ConcurrencyLimitedEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(self.embed_impl(text))
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(self.health_check_impl())
    }

    fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(self.embed_batch_impl(texts))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::{Notify, Semaphore};

    use super::ConcurrencyLimitedEmbedding;
    use crate::{
        embedding::{BoxFuture, EmbeddingProvider},
        error::EmbeddingError,
    };

    struct BlockingProvider {
        calls: Arc<AtomicUsize>,
        started: Arc<Notify>,
        release: Arc<Semaphore>,
    }

    impl EmbeddingProvider for BlockingProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move {
                let _previous = self.calls.fetch_add(1, Ordering::Relaxed);
                self.started.notify_waiters();
                let _permit = self.release.acquire().await.map_err(|_closed| EmbeddingError::Disabled)?;
                Ok(vec![1.0, 0.0, 0.0])
            })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn limits_concurrent_provider_calls() {
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let inner = Arc::new(BlockingProvider {
            calls: Arc::clone(&calls),
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        });
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(ConcurrencyLimitedEmbedding::new(inner, 1));

        let first_started = started.notified();
        let first_provider = Arc::clone(&provider);
        let first = tokio::spawn(async move { first_provider.embed("first").await });
        first_started.await;

        let second_provider = Arc::clone(&provider);
        let second = tokio::spawn(async move { second_provider.embed("second").await });
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Relaxed), 1, "second call must wait for a permit");

        release.add_permits(2);
        let _first_embedding = first.await.unwrap().unwrap();
        let _second_embedding = second.await.unwrap().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
