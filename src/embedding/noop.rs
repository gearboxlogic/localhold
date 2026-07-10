use super::{BoxFuture, EmbeddingProvider};
use crate::error::EmbeddingError;

/// Fallback embedding provider that always returns errors.
/// Used when embeddings are unavailable — memories are still stored, just not semantically searchable.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct NoopEmbedding;

impl NoopEmbedding {
    /// Create a new no-op embedding provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl EmbeddingProvider for NoopEmbedding {
    fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async { Err(EmbeddingError::Disabled) })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async { Err(EmbeddingError::Disabled) })
    }

    fn embed_batch<'a>(&'a self, _texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async { Err(EmbeddingError::Disabled) })
    }
}
