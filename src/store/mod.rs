//! Memory persistence layer — trait definition and SQLite-backed implementation.

mod admin;
#[cfg(test)]
pub(crate) mod conformance;
pub(crate) mod crud;
pub mod migration;
mod postgres;
mod query;
mod schema;
mod search;
mod sqlite;
pub(crate) mod vector;

use std::{collections::HashMap, future::Future};

pub use postgres::PostgresStore;
use serde::{Deserialize, Serialize};
pub use sqlite::SqliteStore;
pub(crate) use sqlite::sqlite_write_tx;

use crate::{
    error::StoreError,
    types::{
        AuditAction, AuditDraft, AuditEntry, AuthorizedUpdateOutcome, Memory, MemoryFilter, MemoryId, MemoryStats, MemoryTombstone, MemoryUpdate, QueryContext, ScopeDefinition,
        SearchResult, V2MemoryMetadata, V2MetadataMigrationReport, V2MetadataPatch, V2MigrationReport, WriteOutcome,
    },
};

/// Map from memory ID to its embedding vector.
///
/// Used by [`MemoryReader::fetch_embeddings_for_ids`] and related functions
/// to return embedding vectors keyed by their owning memory.
pub(crate) type EmbeddingMap = HashMap<MemoryId, Vec<f32>>;

/// An ANN neighbor result: `(memory_id, l2_distance)`.
pub(crate) type EmbeddingNeighbor = (MemoryId, f64);

/// Secret-free identity for the vector space produced by an embedding provider.
///
/// Model names are not globally unique across OpenAI-compatible endpoints, so
/// the normalized endpoint is part of the identity. API keys are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EmbeddingProfile {
    /// Provider protocol used to produce vectors.
    pub provider: String,
    /// Normalized provider endpoint.
    pub endpoint: String,
    /// Provider-specific model identifier.
    pub model: String,
    /// Number of elements in every vector.
    pub dimensions: usize,
}

impl EmbeddingProfile {
    /// Build an OpenAI-compatible embedding profile.
    #[must_use]
    pub fn openai_compatible<E: Into<String>, M: Into<String>>(endpoint: E, model: M, dimensions: usize) -> Self {
        Self {
            provider: "openai_compatible".into(),
            endpoint: endpoint.into(),
            model: model.into(),
            dimensions,
        }
    }
}

/// Durable claim for one unembedded memory revision selected for re-embedding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReembedClaim {
    /// Memory ID selected for embedding.
    pub id: MemoryId,
    /// Content to embed for the claimed revision.
    pub content: String,
    /// Revision that must still be current when the embedding is written.
    pub embedding_revision: i64,
    /// Opaque lease token used to release only this claim.
    pub claim_token: String,
}

/// A memory paired with its optional pre-computed embedding vector.
///
/// Primarily used for store internals (batch operations, consolidation queries)
/// and testing infrastructure where both the memory and its embedding need to
/// travel together through the persistence layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MemoryWithEmbedding {
    /// The memory entry.
    pub memory: Memory,
    /// Pre-computed embedding vector, if available.
    pub embedding: Option<Vec<f32>>,
}

/// Outcome of a bulk write operation with per-item authorization.
///
/// Returned by [`MemoryWriter::bulk_delete_ids`] and [`MemoryWriter::bulk_update_ids`]
/// to report how many items were processed vs denied.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BulkAuthOutcome {
    /// IDs of items successfully written (deleted or updated).
    pub applied_ids: Vec<MemoryId>,
    /// Number of items denied due to access policy.
    pub denied: u64,
}

/// Outcome of a scope reassignment operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReassignScopeOutcome {
    /// IDs of memories whose scope was updated.
    pub applied_ids: Vec<MemoryId>,
}

/// Outcome of recording true-use activity for one or more memories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordUseOutcome {
    /// Number of memories whose activity signal was updated.
    pub recorded: u64,
    /// Number of memories denied due to read access or expiry.
    pub denied: u64,
    /// Number of memory IDs that did not exist.
    pub not_found: u64,
}

/// Read-only memory operations: get, search, list, count, and re-embed queries.
pub trait MemoryReader: Send + Sync {
    /// Whether FTS5 full-text search is available in this store.
    fn fts_available(&self) -> bool {
        false
    }
    /// Retrieve a single memory by ID, or `None` if it does not exist.
    /// When `principal` is provided, access policy is enforced; otherwise only public memories are returned.
    fn get(&self, id: &MemoryId, principal: Option<&str>) -> impl Future<Output = Result<Option<Memory>, StoreError>> + Send;

    /// Find memories whose embeddings are nearest to the query vector, applying optional filters.
    /// When `max_distance` is set, results with L2 distance exceeding the threshold are excluded.
    #[expect(
        clippy::too_many_arguments,
        reason = "search requires embedding, limit, filter, context, and distance threshold — all semantically distinct"
    )]
    fn search_by_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: &MemoryFilter,
        ctx: &QueryContext,
        max_distance: Option<f64>,
    ) -> impl Future<Output = Result<Vec<SearchResult>, StoreError>> + Send;

    /// Find memories whose content matches a text query (LIKE search), applying optional filters.
    fn search_by_text(&self, query: &str, limit: usize, filter: &MemoryFilter, ctx: &QueryContext) -> impl Future<Output = Result<Vec<SearchResult>, StoreError>> + Send;

    /// Find memories using FTS5 full-text search with BM25 ranking, applying optional filters.
    /// When `context` is provided, non-stopword tokens from the context are appended as
    /// optional OR terms to broaden the FTS5 match.
    #[expect(
        clippy::too_many_arguments,
        reason = "FTS search requires query, limit, filter, caller context, and optional search context — all semantically distinct"
    )]
    fn search_by_fts(
        &self,
        query: &str,
        limit: usize,
        filter: &MemoryFilter,
        ctx: &QueryContext,
        context: Option<&str>,
    ) -> impl Future<Output = Result<Vec<SearchResult>, StoreError>> + Send;

    /// List memories ordered by creation time, applying optional filters and limit.
    fn list(&self, filter: MemoryFilter, ctx: QueryContext) -> impl Future<Output = Result<Vec<Memory>, StoreError>> + Send;

    /// Return aggregate statistics about stored memories matching the filter.
    fn count(&self, filter: MemoryFilter, ctx: QueryContext, top_tags_limit: usize) -> impl Future<Output = Result<MemoryStats, StoreError>> + Send;

    /// Fetch memories without embeddings for re-embedding, returning `(id, content, revision)` tuples.
    /// Results are ordered by creation time (oldest first) and capped at `limit`.
    #[expect(
        clippy::type_complexity,
        reason = "impl Future + Send is required by native async-in-trait; the inner tuple is domain-specific"
    )]
    fn list_for_reembed(&self, limit: usize) -> impl Future<Output = Result<Vec<(MemoryId, String, i64)>, StoreError>> + Send;

    /// Fetch a single memory for re-embedding, checking write access.
    /// Returns `(content, embedding_revision)` if authorized, `None` otherwise.
    #[expect(
        clippy::type_complexity,
        reason = "impl Future + Send is required by native async-in-trait; the inner tuple is domain-specific"
    )]
    fn get_for_reembed(&self, id: &MemoryId, principal: &str) -> impl Future<Output = Result<Option<(String, i64)>, StoreError>> + Send;

    /// Fetch memories with their embedding vectors for consolidation.
    ///
    /// Applies optional scope filter, returns up to `limit` memories that have embeddings.
    /// Each result includes the memory and its embedding vector.
    fn list_with_embeddings(&self, scopes_any: Option<&[String]>, limit: usize) -> impl Future<Output = Result<Vec<MemoryWithEmbedding>, StoreError>> + Send;

    /// Query the audit log for a specific memory ID.
    fn query_audit_log(&self, memory_id: &MemoryId, limit: usize) -> impl Future<Output = Result<Vec<AuditEntry>, StoreError>> + Send;

    /// Fetch the deleted-memory authorization tombstone for a memory ID.
    fn get_tombstone(&self, memory_id: &MemoryId) -> impl Future<Output = Result<Option<MemoryTombstone>, StoreError>> + Send;

    /// Fetch embedding vectors for the given memory IDs.
    ///
    /// Returns a map from `MemoryId` to its embedding vector. Memories without
    /// embeddings are silently omitted from the result.
    fn fetch_embeddings_for_ids(&self, ids: &[MemoryId]) -> impl Future<Output = Result<EmbeddingMap, StoreError>> + Send;

    /// Find nearest neighbors for an embedding within an L2 distance threshold.
    ///
    /// Returns `(neighbor_memory_id, l2_distance)` pairs. Self-matches and
    /// superseded memories are excluded.
    fn find_embedding_neighbors(&self, embedding: &[f32], max_l2_distance: f64, limit: usize) -> impl Future<Output = Result<Vec<EmbeddingNeighbor>, StoreError>> + Send;
}

/// Write operations: store, update, delete, batch store, set embedding,
/// and authorization-checked variants.
pub trait MemoryWriter: Send + Sync {
    /// Persist a memory and optionally its embedding vector. Returns the assigned ID.
    fn store(&self, memory: &Memory, embedding: Option<&[f32]>) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Persist a memory and audit row in one transaction.
    fn store_audited(&self, memory: &Memory, embedding: Option<&[f32]>, audit: &AuditDraft) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store a memory and atomically mark an older memory as superseded.
    ///
    /// The old memory's `superseded_by` is set to the new memory's ID.
    /// Returns an error if the superseded memory does not exist.
    fn store_with_supersession(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store a memory, supersession state, and audit row in one transaction.
    fn store_with_supersession_audited(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: &MemoryId,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store a memory and required v2 metadata in one transaction.
    /// When `supersedes_id` is provided, the older memory is marked superseded
    /// in the same transaction.
    fn store_with_v2_metadata(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &V2MemoryMetadata,
    ) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store a memory, required v2 metadata, optional supersession state, and
    /// audit row in one transaction.
    #[expect(clippy::too_many_arguments, reason = "audited v2 store needs memory, embedding, supersession, metadata, and audit draft")]
    fn store_with_v2_metadata_audited(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &V2MemoryMetadata,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store multiple memories atomically in a single transaction.
    /// Returns the list of assigned IDs in the same order as the input.
    fn store_batch(&self, memories: &[MemoryWithEmbedding]) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store multiple memories and matching audit rows atomically.
    /// `audits` must have the same length as `memories`.
    fn store_batch_audited(&self, memories: &[MemoryWithEmbedding], audits: &[AuditDraft]) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store multiple memories with per-item supersession in a single transaction.
    /// Each memory may optionally supersede an older memory.
    /// `supersedes` must have the same length as `memories`.
    fn store_batch_with_supersession(&self, memories: &[MemoryWithEmbedding], supersedes: &[Option<MemoryId>]) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store multiple memories with per-item supersession and matching audit
    /// rows in a single transaction.
    fn store_batch_with_supersession_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        audits: &[AuditDraft],
    ) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store multiple memories and their required v2 metadata in one transaction.
    /// Each memory may optionally supersede an older memory.
    /// `supersedes` and `metadata` must have the same length as `memories`.
    fn store_batch_with_v2_metadata(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[V2MemoryMetadata],
    ) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store multiple memories, required v2 metadata, optional supersession
    /// state, and matching audit rows in one transaction.
    fn store_batch_with_v2_metadata_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[V2MemoryMetadata],
        audits: &[AuditDraft],
    ) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Update fields of an existing memory. Returns `false` if the memory doesn't exist.
    /// If content changes, `has_embedding` is reset to `false` (stale embedding).
    fn update(&self, id: &MemoryId, update: &MemoryUpdate) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Delete a memory by ID. Returns `true` if a row was actually removed.
    fn delete(&self, id: &MemoryId) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Store or replace the embedding vector for an existing memory.
    /// `expected_revision` enforces freshness: writes are accepted only for the
    /// current memory revision.
    fn set_embedding(&self, id: &MemoryId, embedding: &[f32], expected_revision: i64) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Claim unembedded memory revisions for background re-embedding.
    ///
    /// Claimed rows are hidden from subsequent claim attempts until the lease
    /// expires or the claim is released/completed.
    fn claim_for_reembed(&self, limit: usize) -> impl Future<Output = Result<Vec<ReembedClaim>, StoreError>> + Send;

    /// Release a previously claimed unembedded memory revision.
    ///
    /// Returns `true` when the exact claim token was still present and cleared.
    fn release_embedding_claim(&self, id: &MemoryId, expected_revision: i64, claim_token: &str) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Authorization-aware update. Checks write access before applying the update.
    ///
    /// Returns whether the row was updated and, when content changed, the new
    /// revision number that must be used for re-embedding.
    fn update_authorized(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Authorization-aware update plus audit row in one transaction.
    fn update_authorized_audited(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Authorization-aware update, optional v2 metadata patch, and audit row in
    /// one transaction.
    #[expect(clippy::too_many_arguments, reason = "audited revise needs id, update, metadata patch, principal, and audit draft")]
    fn update_authorized_with_v2_metadata_audited(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&V2MetadataPatch>,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Authorization-aware delete. Checks write access before removing the memory.
    fn delete_authorized(&self, id: &MemoryId, principal: &str) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;

    /// Authorization-aware delete plus tombstone and audit row in one transaction.
    fn delete_authorized_audited(&self, id: &MemoryId, principal: &str, audit: &AuditDraft) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;

    /// Delete multiple memories by ID in a single transaction, checking write
    /// access per-ID inside the transaction to avoid TOCTOU races.
    ///
    /// Returns a [`BulkAuthOutcome`] with `applied` (deleted) and `denied` counts.
    fn bulk_delete_ids(&self, ids: Vec<MemoryId>, principal: &str) -> impl Future<Output = Result<BulkAuthOutcome, StoreError>> + Send;

    /// Delete multiple memories and audit each applied delete in one transaction.
    fn bulk_delete_ids_audited(&self, ids: Vec<MemoryId>, principal: &str, audit: &AuditDraft) -> impl Future<Output = Result<BulkAuthOutcome, StoreError>> + Send;

    /// Apply the same update to multiple memories by ID in a single transaction,
    /// checking write access per-ID inside the transaction to avoid TOCTOU races.
    ///
    /// Returns a [`BulkAuthOutcome`] with `applied` (updated) and `denied` counts.
    fn bulk_update_ids(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> impl Future<Output = Result<BulkAuthOutcome, StoreError>> + Send;

    /// Apply a bulk update and audit each applied update in one transaction.
    #[expect(clippy::too_many_arguments, reason = "audited bulk update needs ids, update, principal, timestamp, and audit draft")]
    fn bulk_update_ids_audited(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<BulkAuthOutcome, StoreError>> + Send;

    /// Batch-update impression tracking for memories returned in a search.
    /// Increments `impression_count` and sets `last_impressed_at` for each ID.
    /// These are analytics-only fields; they do not feed into ranking.
    fn record_search_impression(&self, ids: &[MemoryId]) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Record a real use event for the given memories, updating the decayed
    /// `activity_mass` and `last_used_at` timestamp. This feeds into the
    /// activity ranking signal (unlike impressions which are analytics-only).
    #[expect(clippy::too_many_arguments, reason = "ids + principal + weight + now + half_life are all semantically distinct")]
    fn record_memory_use(
        &self,
        ids: &[MemoryId],
        principal: &str,
        event_weight: f64,
        now: chrono::DateTime<chrono::Utc>,
        activity_half_life_hours: f64,
    ) -> impl Future<Output = Result<RecordUseOutcome, StoreError>> + Send;

    /// Write an audit log entry for a memory operation.
    #[expect(
        clippy::too_many_arguments,
        reason = "audit entry requires memory_id, action, caller, timestamp, and details — all semantically distinct"
    )]
    fn write_audit_entry(
        &self,
        memory_id: &MemoryId,
        action: AuditAction,
        principal: Option<&str>,
        timestamp: chrono::DateTime<chrono::Utc>,
        details: Option<&serde_json::Value>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Mark a memory as superseded by another memory ID, without creating a new memory.
    ///
    /// Used by consolidation to mark duplicates. Returns `true` if the row was updated.
    fn mark_superseded_by(&self, id: &MemoryId, superseded_by: &MemoryId) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Authorization-aware supersession used by consolidation.
    ///
    /// Checks write access and marks the row superseded within one serialized
    /// store closure to avoid TOCTOU races.
    fn mark_superseded_by_authorized(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;

    /// Authorization-aware supersession plus audit row in one transaction.
    fn mark_superseded_by_authorized_audited(
        &self,
        id: &MemoryId,
        superseded_by: &MemoryId,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;
}

/// Administrative operations: eviction, scope reassignment.
pub trait MemoryAdmin: Send + Sync {
    /// Remove all expired memories and return the number of deleted rows.
    fn evict_expired(&self) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Reassign conversation scope for matching memories.
    ///
    /// Updates `provenance.source_conversation` from `from_scope` to `to_scope`.
    /// When `origin_conversation` is set, only memories with that origin are
    /// reassigned. Checks write access per memory inside the serialized store
    /// transaction and returns only the IDs that were actually moved.
    fn reassign_scope(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
    ) -> impl Future<Output = Result<ReassignScopeOutcome, StoreError>> + Send;

    /// Reassign scope and audit each applied row in one transaction.
    #[expect(clippy::too_many_arguments, reason = "audited reassign needs scope pair, optional origin, principal, and audit draft")]
    fn reassign_scope_audited(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<ReassignScopeOutcome, StoreError>> + Send;

    /// Register or replace a scope definition.
    fn register_scope(&self, scope: ScopeDefinition) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// List all registered scope definitions ordered by key.
    fn list_scopes(&self) -> impl Future<Output = Result<Vec<ScopeDefinition>, StoreError>> + Send;

    /// Upsert non-destructive v2 metadata for a memory.
    fn upsert_v2_metadata(&self, metadata: V2MemoryMetadata) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Upsert non-destructive v2 metadata and audit the memory in one transaction.
    fn upsert_v2_metadata_audited(&self, metadata: V2MemoryMetadata, audit: &AuditDraft) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Fetch non-destructive v2 metadata for a memory.
    fn get_v2_metadata(&self, memory_id: &MemoryId) -> impl Future<Output = Result<Option<V2MemoryMetadata>, StoreError>> + Send;

    /// Return conservative migration/reporting counts for v2 metadata.
    fn v2_migration_report(&self) -> impl Future<Output = Result<V2MigrationReport, StoreError>> + Send;

    /// Add v2 metadata rows for existing memories without rewriting original content.
    fn migrate_v2_metadata(&self, registered_scope_keys: &[String], dry_run: bool) -> impl Future<Output = Result<V2MetadataMigrationReport, StoreError>> + Send;

    /// Add v2 metadata rows and audit each inserted row in one transaction.
    fn migrate_v2_metadata_audited(
        &self,
        registered_scope_keys: &[String],
        dry_run: bool,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<V2MetadataMigrationReport, StoreError>> + Send;
}

/// Combined trait for full memory store access: read, write, and admin.
///
/// Automatically implemented for any type that implements all three sub-traits.
pub trait MemoryStore: MemoryReader + MemoryWriter + MemoryAdmin {}

impl<T: MemoryReader + MemoryWriter + MemoryAdmin> MemoryStore for T {}

pub(crate) fn merge_v2_metadata_patch(
    memory_id: MemoryId,
    patch: &V2MetadataPatch,
    existing: Option<&V2MemoryMetadata>,
    fallback_scope: Option<&str>,
    principal: &str,
) -> V2MemoryMetadata {
    V2MemoryMetadata {
        memory_id,
        scope_key: patch
            .scope_key
            .clone()
            .or_else(|| existing.and_then(|metadata| metadata.scope_key.clone()))
            .or_else(|| fallback_scope.map(ToOwned::to_owned)),
        summary: patch.summary.clone().or_else(|| existing.and_then(|metadata| metadata.summary.clone())),
        agent_label: patch.agent_label.clone().or_else(|| existing.and_then(|metadata| metadata.agent_label.clone())),
        created_by_principal: existing.and_then(|metadata| metadata.created_by_principal.clone()).or_else(|| Some(principal.to_owned())),
        quality_flags: existing.map_or_else(Vec::new, |metadata| metadata.quality_flags.clone()),
        schema_version: 2,
    }
}

pub(crate) fn update_audit_draft_for_locked_memory(audit: &AuditDraft, update: &MemoryUpdate, existing: &Memory) -> AuditDraft {
    if update.content.is_none() {
        return audit.clone();
    }

    let mut audit = audit.clone();
    audit.details = Some(audit_details_with_old_content_hash(audit.details.take(), crud::content_hash(&existing.content)));
    audit
}

fn audit_details_with_old_content_hash(details: Option<serde_json::Value>, old_content_hash: String) -> serde_json::Value {
    match details {
        Some(serde_json::Value::Object(mut fields)) => {
            let _previous = fields.insert("old_content_hash".into(), serde_json::Value::String(old_content_hash));
            serde_json::Value::Object(fields)
        }
        Some(value) => serde_json::json!({
            "old_content_hash": old_content_hash,
            "details": value,
        }),
        None => serde_json::json!({"old_content_hash": old_content_hash}),
    }
}
