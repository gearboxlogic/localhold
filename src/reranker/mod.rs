//! Cross-encoder reranker — scores (query, document) pairs for precision reranking.
//!
//! Follows the same trait-abstraction pattern as [`crate::embedding`]: a
//! `dyn`-safe [`RerankerProvider`] trait with [`BoxFuture`] return types,
//! plus an [`RerankerError::Unavailable`] fallback when no model is configured.

/// ONNX cross-encoder reranker (requires `reranker` feature).
#[cfg(feature = "reranker")]
pub mod onnx;

/// Resilient wrapper with automatic availability recovery (requires `reranker` feature).
#[cfg(feature = "reranker")]
pub mod resilient;

/// Model file download from `HuggingFace` (requires `reranker` feature).
#[cfg(feature = "reranker")]
mod download;

use std::{future::Future, pin::Pin};

use thiserror::Error;

/// Boxed future alias used by [`RerankerProvider`] trait methods.
///
/// Keeps the trait `dyn`-compatible (`Arc<dyn RerankerProvider>`) without
/// the `async_trait` proc-macro by returning a pinned, boxed, `Send` future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A single (query, document) pair scored by the reranker.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RerankerScore {
    /// Index into the original input slice, so the caller can correlate
    /// scores back to `SearchResult` entries without cloning documents.
    pub index: usize,
    /// Cross-encoder relevance score in `[0.0, 1.0]` (sigmoid-normalized).
    pub score: f64,
}

impl RerankerScore {
    /// Create a reranker score for the document at `index`.
    #[must_use]
    pub const fn new(index: usize, score: f64) -> Self {
        Self { index, score }
    }
}

/// Errors from the reranker subsystem.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RerankerError {
    /// A transient error (model loading hiccup, resource contention) — worth retrying.
    #[error("transient reranker error: {0}")]
    Transient(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A permanent error (bad model, incompatible ONNX graph) — retrying won't help.
    #[error("permanent reranker error: {0}")]
    Permanent(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The reranker is intentionally disabled (noop provider).
    #[error("reranker is disabled")]
    Unavailable,
}

/// Trait for cross-encoder reranking of (query, document) pairs.
///
/// Methods return [`BoxFuture`] so the trait remains `dyn`-compatible for
/// `Arc<dyn RerankerProvider>`. Implementors should use `async fn` in the
/// method body wrapped with `Box::pin(async move { ... })`.
pub trait RerankerProvider: Send + Sync {
    /// Score a batch of documents against the given query.
    ///
    /// Returns one [`RerankerScore`] per input document, with the `index`
    /// field corresponding to the position in `documents`. The scores are
    /// sigmoid-normalized to `[0.0, 1.0]`.
    #[expect(clippy::type_complexity, reason = "BoxFuture IS the simplified alias; clippy counts the expansion")]
    fn rerank<'a>(&'a self, query: &'a str, documents: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>>;

    /// Verify that the reranker model is loaded and functional.
    fn health_check(&self) -> BoxFuture<'_, Result<(), RerankerError>>;
}
