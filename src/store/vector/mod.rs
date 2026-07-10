//! Store-internal vector index abstraction.
//!
//! Vector indexes return memory IDs and distances only. The memory store keeps
//! ownership of hydration, access policy, TTL, redaction, and filtering.

mod sqlite_vec;

pub(crate) use sqlite_vec::SqliteVecIndex;

use super::EmbeddingMap;
use crate::{error::StoreError, types::MemoryId};

/// Validate an embedding before it reaches backend-specific vector storage or search.
pub(crate) fn validate_embedding_vector(embedding: &[f32], dimensions: usize) -> Result<(), StoreError> {
    if embedding.len() != dimensions {
        return Err(StoreError::Conflict(format!(
            "embedding dimension mismatch: expected {}, got {}",
            dimensions,
            embedding.len()
        )));
    }
    if let Some((idx, value)) = embedding.iter().enumerate().find(|(_, value)| !value.is_finite()) {
        return Err(StoreError::Conflict(format!("embedding contains non-finite value at index {idx}: {value}")));
    }
    Ok(())
}

/// A nearest-neighbor hit from a vector index.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub(crate) struct VectorHit {
    /// The memory identified by the vector index.
    pub memory_id: MemoryId,
    /// Backend-reported L2 distance. Lower is more similar.
    pub distance: f64,
}

/// A bounded vector candidate batch.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub(crate) struct VectorBatch {
    /// Parsed hits returned by the index.
    pub hits: Vec<VectorHit>,
    /// Raw backend row count before parse-time skips.
    pub returned_count: usize,
}

/// Transaction-scoped vector index operations for a concrete backend session.
pub(crate) trait VectorIndex<Db: ?Sized>: Send + Sync {
    /// Configured embedding dimensions for this index.
    fn dimensions(&self) -> usize;

    /// Initialize backend-specific vector schema.
    fn init_schema(&self, db: &Db) -> Result<(), StoreError>;

    /// Insert or replace a memory embedding.
    fn upsert(&self, db: &Db, memory_id: &str, embedding: &[f32]) -> Result<(), StoreError>;

    /// Delete a memory embedding if present.
    fn delete(&self, db: &Db, memory_id: &str) -> Result<(), StoreError>;

    /// Return nearest vector candidates, capped by `limit`.
    fn search_batch(&self, db: &Db, embedding: &[f32], limit: usize) -> Result<VectorBatch, StoreError>;

    /// Return nearest neighbors within `max_l2_distance`.
    fn neighbors(&self, db: &Db, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<VectorHit>, StoreError>;

    /// Fetch stored embeddings for known memory IDs.
    fn fetch_many(&self, db: &Db, ids: &[MemoryId]) -> Result<EmbeddingMap, StoreError>;
}
