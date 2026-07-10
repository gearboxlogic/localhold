//! Embedding provider abstraction with OpenAI-compatible HTTP, noop, and resilient implementations.

/// Explicit bulk-request execution and per-input fallback.
pub(crate) mod batch;
/// Configured provider construction and vector-space identity.
pub mod factory;
/// Shared provider request concurrency limit.
pub(crate) mod limited;
/// Fallback embedding provider (always returns errors).
pub mod noop;
/// OpenAI-compatible embedding provider.
pub mod openai;
/// Embedding orchestrator — enforces the store-then-embed invariant.
pub(crate) mod orchestrator;
/// Resilient wrapper with automatic availability recovery.
pub mod resilient;
/// Retry-delay policy shared by resilient providers.
pub(crate) mod retry;

use std::{future::Future, pin::Pin};

pub use noop::NoopEmbedding;
pub use openai::OpenAiEmbedding;
pub use resilient::ResilientEmbedding;

use crate::error::EmbeddingError;

/// Boxed future alias used by [`EmbeddingProvider`] trait methods.
///
/// Keeps the trait `dyn`-compatible (`Arc<dyn EmbeddingProvider>`) without
/// the `async_trait` proc-macro by returning a pinned, boxed, `Send` future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait for generating text embedding vectors.
///
/// Methods return [`BoxFuture`] so the trait remains `dyn`-compatible for
/// `Arc<dyn EmbeddingProvider>`. Implementors should use `async fn` in the
/// method body wrapped with `Box::pin(async move { ... })`.
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding vector for the given text.
    ///
    /// # Normalization contract
    ///
    /// Implementations SHOULD return L2-normalized (unit-length) vectors. Consumers
    /// (sqlite-vec ANN search) assume embeddings are comparable via L2 distance;
    /// unnormalized vectors will produce degraded search quality.
    #[expect(clippy::type_complexity, reason = "BoxFuture IS the simplified alias; clippy counts the expansion")]
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>>;

    /// Verify that the embedding service is reachable and functional.
    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>>;

    /// Generate embedding vectors for multiple texts in a single call.
    ///
    /// The default implementation calls [`embed`](Self::embed) sequentially.
    /// Providers that support batch APIs should override this for efficiency.
    #[expect(clippy::type_complexity, reason = "BoxFuture IS the simplified alias; clippy counts the expansion")]
    fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move {
            let mut results = Vec::with_capacity(texts.len());
            for text in texts {
                results.push(self.embed(text).await?);
            }
            Ok(results)
        })
    }
}
