//! Explicit embedding batch execution with input-isolating fallback.

use std::sync::Arc;

use tracing::warn;

use super::EmbeddingProvider;
use crate::error::EmbeddingError;

type PerItemEmbeddingResults = Vec<Result<Vec<f32>, EmbeddingError>>;

/// Result of one provider batch call.
pub(crate) enum BatchEmbeddingResult {
    /// Every input has an independent result in input order.
    PerItem(PerItemEmbeddingResults),
    /// The whole request failed and should remain pending as a unit.
    BatchFailed(EmbeddingError),
}

#[cfg(test)]
impl BatchEmbeddingResult {
    fn into_per_item(self) -> Option<PerItemEmbeddingResults> {
        match self {
            Self::PerItem(results) => Some(results),
            Self::BatchFailed(_) => None,
        }
    }
}

/// Runs bounded explicit batches against a shared embedding provider.
#[derive(Clone)]
pub(crate) struct BatchEmbeddingExecutor {
    provider: Arc<dyn EmbeddingProvider>,
    chunk_size: usize,
}

impl BatchEmbeddingExecutor {
    pub(crate) fn new(provider: Arc<dyn EmbeddingProvider>, chunk_size: usize) -> Self {
        Self {
            provider,
            chunk_size: chunk_size.max(1),
        }
    }

    pub(crate) const fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub(crate) const fn provider(&self) -> &Arc<dyn EmbeddingProvider> {
        &self.provider
    }

    /// Embed one chunk. Permanent batch-level failures fall back to individual
    /// requests so one invalid input does not block the rest of the chunk.
    pub(crate) async fn execute_chunk(&self, contents: &[Arc<str>]) -> BatchEmbeddingResult {
        let texts: Vec<&str> = contents.iter().map(AsRef::as_ref).collect();
        match self.provider.embed_batch(&texts).await {
            Ok(embeddings) if embeddings.len() == contents.len() => BatchEmbeddingResult::PerItem(embeddings.into_iter().map(Ok).collect()),
            Ok(embeddings) => {
                warn!(
                    expected = contents.len(),
                    actual = embeddings.len(),
                    "embedding batch returned the wrong result count; retrying inputs individually"
                );
                self.execute_individually(contents).await
            }
            Err(EmbeddingError::Permanent(error)) => {
                warn!(%error, "embedding batch was permanently rejected; retrying inputs individually");
                self.execute_individually(contents).await
            }
            Err(error) => BatchEmbeddingResult::BatchFailed(error),
        }
    }

    async fn execute_individually(&self, contents: &[Arc<str>]) -> BatchEmbeddingResult {
        let mut results = Vec::with_capacity(contents.len());
        for content in contents {
            results.push(self.provider.embed(content).await);
        }
        BatchEmbeddingResult::PerItem(results)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::{BatchEmbeddingExecutor, BatchEmbeddingResult};
    use crate::{
        embedding::{BoxFuture, EmbeddingProvider},
        error::EmbeddingError,
    };

    struct RecordingProvider {
        batch_calls: AtomicUsize,
        single_calls: AtomicUsize,
        batch_behavior: BatchBehavior,
    }

    enum BatchBehavior {
        Success,
        PermanentFailure,
        TransientFailure,
        WrongCount,
    }

    impl RecordingProvider {
        fn embed_sync(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
            let _previous = self.single_calls.fetch_add(1, Ordering::Relaxed);
            if text == "invalid" {
                Err(EmbeddingError::Permanent("invalid input".into()))
            } else {
                Ok(vec![1.0])
            }
        }
    }

    impl EmbeddingProvider for RecordingProvider {
        fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move { self.embed_sync(text) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }

        fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            Box::pin(async move {
                let _previous = self.batch_calls.fetch_add(1, Ordering::Relaxed);
                match self.batch_behavior {
                    BatchBehavior::Success => Ok(texts.iter().map(|_text| vec![1.0]).collect()),
                    BatchBehavior::PermanentFailure => Err(EmbeddingError::Permanent("bad batch input".into())),
                    BatchBehavior::TransientFailure => Err(EmbeddingError::Transient("provider unavailable".into())),
                    BatchBehavior::WrongCount => Ok(Vec::new()),
                }
            })
        }
    }

    fn executor(behavior: BatchBehavior) -> (Arc<RecordingProvider>, BatchEmbeddingExecutor) {
        let provider = Arc::new(RecordingProvider {
            batch_calls: AtomicUsize::new(0),
            single_calls: AtomicUsize::new(0),
            batch_behavior: behavior,
        });
        let executor = BatchEmbeddingExecutor::new(Arc::<RecordingProvider>::clone(&provider), 32);
        (provider, executor)
    }

    #[tokio::test]
    async fn successful_batch_does_not_issue_single_requests() {
        let (provider, executor) = executor(BatchBehavior::Success);
        let contents = [Arc::<str>::from("first"), Arc::<str>::from("second")];

        let results = executor.execute_chunk(&contents).await.into_per_item().unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(provider.batch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.single_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn permanent_batch_failure_isolates_invalid_inputs() {
        let (provider, executor) = executor(BatchBehavior::PermanentFailure);
        let contents = [Arc::<str>::from("valid"), Arc::<str>::from("invalid")];

        let results = executor.execute_chunk(&contents).await.into_per_item().unwrap();
        let _valid = results[0].as_ref().unwrap();
        assert!(matches!(results[1], Err(EmbeddingError::Permanent(_))));
        assert_eq!(provider.batch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.single_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn malformed_batch_result_falls_back_to_single_requests() {
        let (provider, executor) = executor(BatchBehavior::WrongCount);
        let contents = [Arc::<str>::from("first"), Arc::<str>::from("second")];

        let results = executor.execute_chunk(&contents).await.into_per_item().unwrap();
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(provider.single_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn transient_batch_failure_does_not_amplify_requests() {
        let (provider, executor) = executor(BatchBehavior::TransientFailure);
        let contents = [Arc::<str>::from("first"), Arc::<str>::from("second")];

        let result = executor.execute_chunk(&contents).await;
        assert!(matches!(result, BatchEmbeddingResult::BatchFailed(EmbeddingError::Transient(_))));
        assert_eq!(provider.batch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.single_calls.load(Ordering::Relaxed), 0);
    }
}
