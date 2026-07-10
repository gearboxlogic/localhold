//! Error type hierarchy — engine, store, and embedding error variants.

use thiserror::Error;

/// Top-level error type for the `LocalHold` engine.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// A configuration error.
    #[error("config error: {0}")]
    Config(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// An input validation error.
    #[error(transparent)]
    Validation(#[from] ValidationError),

    /// A persistence-layer error.
    #[error(transparent)]
    Store(#[from] StoreError),

    /// The embedding provider is unreachable or non-functional.
    #[error("embedding unavailable: {0}")]
    EmbeddingUnavailable(String),

    /// The requested search mode cannot run with the currently available backends.
    #[error("search unavailable: {0}")]
    SearchUnavailable(String),

    /// An embedding subsystem error (transient, permanent, or disabled).
    #[error(transparent)]
    Embedding(#[from] EmbeddingError),

    /// The engine is shutting down and no new background embedding work may be admitted.
    #[error("server is shutting down")]
    ShuttingDown,
}

impl EngineError {
    /// Create a configuration error, preserving the original error source chain.
    pub fn config<E: Into<Box<dyn std::error::Error + Send + Sync>>>(source: E) -> Self {
        Self::Config(source.into())
    }
}

/// Errors originating from the persistence layer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// An underlying database operation failed.
    #[error("database error: {0}")]
    Database(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Serialization or deserialization of stored data failed.
    #[error("serialization error: {0}")]
    Serialization(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The requested resource was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A uniqueness or concurrency conflict occurred.
    #[error("conflict: {0}")]
    Conflict(String),

    /// A schema migration step failed.
    #[error("migration {step} failed")]
    MigrationFailed {
        /// Human-readable name of the migration step that failed.
        step: &'static str,
        /// The underlying database error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(Box::new(e))
    }
}

impl From<sqlx_core::Error> for StoreError {
    fn from(e: sqlx_core::Error) -> Self {
        Self::Database(Box::new(e))
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(Box::new(e))
    }
}

/// A domain-level validation error, independent of any wire protocol.
///
/// Created by validation helpers when input fails a constraint (empty field,
/// too-long content, etc.).  Converted to protocol-specific error types
/// (e.g. `rmcp::ErrorData`) at the handler boundary.
#[derive(Debug, Error)]
#[error("{field}: {message}")]
#[non_exhaustive]
pub struct ValidationError {
    /// Which input field triggered the error.
    pub field: String,
    /// Human-readable explanation of what went wrong.
    pub message: String,
}

impl ValidationError {
    /// Convenience constructor.
    pub fn new<F: Into<String>, M: Into<String>>(field: F, message: M) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Error returned when parsing a string into a domain enum fails.
#[derive(Debug, Error)]
#[error("{0}")]
#[non_exhaustive]
pub struct ParseEnumError(pub String);

impl From<String> for ParseEnumError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Errors from the embedding subsystem.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EmbeddingError {
    /// A transient error (timeout, network blip) — worth retrying.
    #[error("transient embedding error: {0}")]
    Transient(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A permanent error (bad model name, invalid input) — retrying won't help.
    #[error("permanent embedding error: {0}")]
    Permanent(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Embedding is intentionally disabled (noop provider).
    #[error("embedding is disabled")]
    Disabled,
}
