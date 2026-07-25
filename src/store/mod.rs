//! Memory persistence layer — trait definition and SQLite-backed implementation.

mod admin;
pub mod backup;
#[cfg(test)]
pub(crate) mod conformance;
mod context_store;
pub(crate) mod crud;
pub mod migration;
mod postgres;
pub(crate) mod postgres_migrations;
mod query;
mod schema;
mod search;
mod sqlite;
mod sqlite_lease;
pub(crate) mod vector;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
};

pub use postgres::PostgresStore;
pub(crate) use postgres::validate_published_v2_metadata_upgrade;
pub(crate) use schema::{existing_embedding_dimensions, validate_published_v2_metadata};
use serde::{Deserialize, Serialize};
pub use sqlite::SqliteStore;
pub(crate) use sqlite::sqlite_write_tx;
#[cfg(unix)]
pub(crate) use sqlite_lease::database_identity as sqlite_database_identity;

use crate::{
    context::{
        ContextAnchorPolicyDraft, ContextAnchorPolicyRecord, ContextAuditDraft, ContextAuditEvent, ContextCreateDraft, ContextDefinition, ContextDefinitionPatch,
        ContextExactLookup, ContextGrant, ContextId, ContextKindDefinition, ContextKindDraft, ContextKindPolicyDraft, ContextKindPolicyRecord, ContextLifecycle, ContextRecord,
        MemoryContext,
    },
    error::StoreError,
    types::{
        AccessPolicy, AuditAction, AuditDraft, AuditEntry, AuthorizedUpdateOutcome, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryStats, MemoryTombstone, MemoryUpdate,
        MetadataMigrationOutcome, MetadataMigrationReport, MetadataPatch, Provenance, QueryContext, ScopeDefinition, SearchResult, WriteOutcome,
    },
};

/// Ordered direct context memberships keyed by their memory.
pub type MemoryContextMap = HashMap<MemoryId, Vec<MemoryContext>>;
/// Authorized memory IDs that have at least one direct context membership,
/// regardless of whether the context definition is visible to the caller.
pub type MemoryContextPresence = HashSet<MemoryId>;
/// Authorized memories keyed by ID for batch reads.
pub type MemoryMap = HashMap<MemoryId, Memory>;

/// Read governed context definitions, memberships, and audit history.
pub trait ContextReader: Send + Sync {
    /// Fetch one context when the principal owns it or has an explicit use
    /// grant. Archived definitions remain visible to authorized callers.
    fn get_context(&self, id: &ContextId, principal: &str) -> impl Future<Output = Result<Option<ContextDefinition>, StoreError>> + Send;

    /// Return a page of contexts the principal may use.
    fn list_contexts(&self, principal: &str, include_archived: bool, offset: usize, limit: usize) -> impl Future<Output = Result<Vec<ContextDefinition>, StoreError>> + Send;

    /// Return a page of authorized context definitions with their safe
    /// aliases, fingerprinted identities, and weak hints.
    fn list_context_records(&self, principal: &str, include_archived: bool, offset: usize, limit: usize) -> impl Future<Output = Result<Vec<ContextRecord>, StoreError>> + Send;

    /// Resolve an indexed exact ID, normalized key/alias, or fingerprinted
    /// identity within the caller's authorized context catalog.
    fn find_context_records(&self, principal: &str, include_archived: bool, lookup: &ContextExactLookup) -> impl Future<Output = Result<Vec<ContextRecord>, StoreError>> + Send;

    /// Expand direct selections through ancestors and, when requested,
    /// descendants. Direct selections retain caller order.
    fn expand_context_selection(
        &self,
        context_ids: &[ContextId],
        principal: &str,
        include_descendants: bool,
    ) -> impl Future<Output = Result<Vec<ContextDefinition>, StoreError>> + Send;

    /// Return ordered direct context memberships for an authorized memory.
    fn get_memory_contexts(&self, memory_id: &MemoryId, principal: &str) -> impl Future<Output = Result<Vec<MemoryContext>, StoreError>> + Send;

    /// Return ordered direct context memberships for a batch of authorized
    /// memories without issuing one query per memory.
    fn get_memory_contexts_batch(&self, memory_ids: &[MemoryId], principal: &str) -> impl Future<Output = Result<MemoryContextMap, StoreError>> + Send;

    /// Return authorized memories that have at least one direct membership,
    /// without exposing hidden context definitions.
    fn get_memory_context_presence_batch(&self, memory_ids: &[MemoryId], principal: &str) -> impl Future<Output = Result<MemoryContextPresence, StoreError>> + Send;

    /// Count all memberships on a memory the principal may write, including
    /// memberships whose context definition is not currently visible.
    fn count_memory_contexts_for_write(&self, memory_id: &MemoryId, principal: &str) -> impl Future<Output = Result<Option<usize>, StoreError>> + Send;

    /// Return recent context audit events visible to the principal.
    fn query_context_audit(&self, context_id: &ContextId, principal: &str, limit: usize) -> impl Future<Output = Result<Vec<ContextAuditEvent>, StoreError>> + Send;

    /// Return every configured context kind for the operator TUI.
    fn list_context_kinds(&self) -> impl Future<Output = Result<Vec<ContextKindDefinition>, StoreError>> + Send;

    /// Return operator policies and the selected principal's policy overrides.
    fn list_context_kind_policies(&self, principal: &str) -> impl Future<Output = Result<Vec<ContextKindPolicyRecord>, StoreError>> + Send;

    /// Return the selected principal's anchor overrides.
    fn list_context_anchor_policies(&self, principal: &str) -> impl Future<Output = Result<Vec<ContextAnchorPolicyRecord>, StoreError>> + Send;

    /// Return grants for a context visible to the caller. Grantee names are
    /// exposed only to the owner.
    fn list_context_grants(&self, context_id: &ContextId, principal: &str) -> impl Future<Output = Result<Vec<ContextGrant>, StoreError>> + Send;
}

/// Transactional governed context mutations.
pub trait ContextWriter: Send + Sync {
    /// Create a private context with aliases, identities, hints, parent, and
    /// audit event in one transaction.
    fn create_context(&self, draft: &ContextCreateDraft, audit: &ContextAuditDraft) -> impl Future<Output = Result<ContextDefinition, StoreError>> + Send;

    /// Replace a context's parent after transactional cycle validation.
    fn set_context_parent(
        &self,
        context_id: &ContextId,
        parent_id: Option<&ContextId>,
        principal: &str,
        audit: &ContextAuditDraft,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Archive or reactivate an owned context. Frozen compatibility contexts
    /// cannot be changed through this operation.
    fn set_context_lifecycle(
        &self,
        context_id: &ContextId,
        lifecycle: ContextLifecycle,
        principal: &str,
        audit: &ContextAuditDraft,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Add or replace an explicit context use grant.
    fn grant_context_use(&self, context_id: &ContextId, grantee_principal: &str, principal: &str, audit: &ContextAuditDraft)
    -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Revoke an explicit use grant from an owned context.
    fn revoke_context_use(
        &self,
        context_id: &ContextId,
        grantee_principal: &str,
        principal: &str,
        audit: &ContextAuditDraft,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Replace the complete explicit grant set in one audited transaction.
    fn replace_context_grants(
        &self,
        context_id: &ContextId,
        grantee_principals: &[String],
        principal: &str,
        audit: &ContextAuditDraft,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Replace mutable definition fields, aliases, identities, and resolver
    /// hints in one audited transaction.
    fn update_context_definition(
        &self,
        context_id: &ContextId,
        patch: &ContextDefinitionPatch,
        principal: &str,
        audit: &ContextAuditDraft,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Add or update a TUI-managed context kind.
    fn upsert_context_kind(&self, draft: &ContextKindDraft, principal: &str, audit: &ContextAuditDraft) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Replace one operator or principal kind policy.
    fn upsert_context_kind_policy(&self, draft: &ContextKindPolicyDraft, principal: &str, audit: &ContextAuditDraft) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Replace one principal anchor override.
    fn upsert_context_anchor_policy(&self, draft: &ContextAnchorPolicyDraft, principal: &str, audit: &ContextAuditDraft) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Roll back a just-created, otherwise untouched legacy compatibility
    /// context after an enclosing batch write fails.
    ///
    /// Returns `false` when the context was not created by the matching
    /// principal or acquired any membership, hierarchy, grant, policy, or
    /// definition state. This is an internal transaction-compensation path,
    /// not a public context deletion operation.
    fn rollback_unreferenced_legacy_context(&self, context_id: &ContextId, principal: &str) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Replace the complete ordered direct membership set for a memory and
    /// synchronize legacy scope caches in the same transaction.
    fn replace_memory_contexts(
        &self,
        memory_id: &MemoryId,
        context_ids: &[ContextId],
        principal: &str,
        audit: &ContextAuditDraft,
    ) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;
}

/// Full governed-context persistence contract.
pub trait ContextStore: ContextReader + ContextWriter {}

impl<T: ContextReader + ContextWriter> ContextStore for T {}

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

/// Authorization boundary used while selecting durable re-embedding claims.
///
/// Keeping recovery explicit prevents a missing principal from silently
/// widening caller-triggered maintenance into whole-store work.
#[derive(Debug)]
enum ReembedClaimScope {
    Recovery,
    Authorized(String),
}

impl ReembedClaimScope {
    const fn principal(&self) -> Option<&str> {
        match self {
            Self::Recovery => None,
            Self::Authorized(principal) => Some(principal.as_str()),
        }
    }
}

/// Authorization boundary used while selecting expired memories for cleanup.
///
/// Keeping whole-store cleanup explicit prevents a missing principal from
/// silently widening caller-triggered deletion.
#[derive(Debug)]
enum ExpiredCleanupScope {
    All { actor: String },
    Authorized { actor: String },
}

impl ExpiredCleanupScope {
    const fn actor(&self) -> &str {
        match self {
            Self::All { actor } | Self::Authorized { actor } => actor.as_str(),
        }
    }

    const fn authorization_principal(&self) -> Option<&str> {
        match self {
            Self::All { .. } => None,
            Self::Authorized { actor } => Some(actor.as_str()),
        }
    }
}

/// Minimal fields needed to authorize and tombstone a locked memory row.
#[derive(Debug)]
struct MemoryAuthorizationEnvelope {
    id: MemoryId,
    provenance: Provenance,
    access_policy: AccessPolicy,
}

impl MemoryAuthorizationEnvelope {
    fn has_write_access(&self, principal: &str) -> bool {
        crate::types::write_access_allowed(&self.provenance, &self.access_policy, principal)
    }
}

/// Borrowed view of the fields persisted in an authorization tombstone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryAuthorizationRef<'a> {
    id: &'a MemoryId,
    provenance: &'a Provenance,
    access_policy: &'a AccessPolicy,
}

impl<'a> From<&'a Memory> for MemoryAuthorizationRef<'a> {
    fn from(memory: &'a Memory) -> Self {
        Self {
            id: &memory.id,
            provenance: &memory.provenance,
            access_policy: &memory.access_policy,
        }
    }
}

impl<'a> From<&'a MemoryAuthorizationEnvelope> for MemoryAuthorizationRef<'a> {
    fn from(memory: &'a MemoryAuthorizationEnvelope) -> Self {
        Self {
            id: &memory.id,
            provenance: &memory.provenance,
            access_policy: &memory.access_policy,
        }
    }
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
    /// Active direct governed-context memberships, in primary order. The
    /// ordering is semantically significant for consolidation because ordinal
    /// zero is the compatibility-primary context.
    ///
    /// Populated by consolidation reads. Batch-write callers leave this empty.
    pub context_ids: Vec<ContextId>,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordUseOutcome {
    /// IDs whose activity signal was updated, in first-requested order.
    pub recorded_ids: Vec<MemoryId>,
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

    /// Retrieve authorized memories for a batch of IDs in a bounded number of
    /// backend queries. Missing, expired, and unreadable IDs are omitted.
    fn get_batch(&self, ids: &[MemoryId], principal: Option<&str>) -> impl Future<Output = Result<MemoryMap, StoreError>> + Send;

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
    /// Applies write authorization and canonical governed-context
    /// applicability in storage, then returns up to `limit` memories that
    /// have embeddings.
    /// Each result includes the memory and its embedding vector.
    fn list_with_embeddings(
        &self,
        context_ids: Option<&[ContextId]>,
        legacy_context_ids_any: Option<&[ContextId]>,
        principal: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<MemoryWithEmbedding>, StoreError>> + Send;

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
    /// Returns `(neighbor_memory_id, l2_distance)` pairs from the supplied
    /// canonical consolidation candidate set. Self-matches and superseded
    /// memories are excluded before the bounded nearest-neighbor limit.
    #[expect(
        clippy::too_many_arguments,
        reason = "candidate-bounded neighbor lookup needs source, candidates, vector, threshold, and limit"
    )]
    fn find_embedding_neighbors(
        &self,
        source_memory_id: &MemoryId,
        candidate_ids: &[MemoryId],
        embedding: &[f32],
        max_l2_distance: f64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<EmbeddingNeighbor>, StoreError>> + Send;
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

    /// Store a memory and required metadata in one transaction.
    /// When `supersedes_id` is provided, the older memory is marked superseded
    /// in the same transaction.
    fn store_with_metadata(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
    ) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store a memory, required metadata, optional supersession state, and
    /// audit row in one transaction.
    #[expect(clippy::too_many_arguments, reason = "audited store needs memory, embedding, supersession, metadata, and audit draft")]
    fn store_with_metadata_audited(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<MemoryId, StoreError>> + Send;

    /// Store a memory, required metadata, governed memberships, and both audit
    /// rows in one transaction.
    #[expect(
        clippy::too_many_arguments,
        reason = "governed audited store carries memory, embedding, supersession, metadata, memberships, and audits"
    )]
    fn store_with_metadata_contexts_audited(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
        context_ids: &[ContextId],
        audit: &AuditDraft,
        context_audit: &ContextAuditDraft,
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

    /// Store multiple memories and their required metadata in one transaction.
    /// Each memory may optionally supersede an older memory.
    /// `supersedes` and `metadata` must have the same length as `memories`.
    fn store_batch_with_metadata(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
    ) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store multiple memories, required metadata, optional supersession
    /// state, and matching audit rows in one transaction.
    fn store_batch_with_metadata_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
        audits: &[AuditDraft],
    ) -> impl Future<Output = Result<Vec<MemoryId>, StoreError>> + Send;

    /// Store a batch with required metadata, governed memberships, and audits
    /// in one transaction.
    #[expect(clippy::too_many_arguments, reason = "governed batch store carries memories, supersession, metadata, memberships, and audits")]
    fn store_batch_with_metadata_contexts_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
        context_ids: &[Vec<ContextId>],
        audits: &[AuditDraft],
        context_audits: &[ContextAuditDraft],
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

    /// Claim unembedded memory revisions for process-owned recovery.
    ///
    /// This whole-store operation is reserved for startup and embedding
    /// provider recovery, where the configured provider is an operator-owned
    /// storage boundary. Claimed rows are hidden from subsequent claim
    /// attempts until the lease expires or the claim is released/completed.
    fn claim_for_reembed(&self, limit: usize) -> impl Future<Output = Result<Vec<ReembedClaim>, StoreError>> + Send;

    /// Claim unembedded memory revisions that `principal` may write.
    ///
    /// Authorization is applied before `limit`, so inaccessible rows neither
    /// consume the caller's limit nor receive leases.
    fn claim_for_reembed_authorized(&self, principal: &str, limit: usize) -> impl Future<Output = Result<Vec<ReembedClaim>, StoreError>> + Send;

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

    /// Authorization-aware update, optional metadata patch, and audit row in
    /// one transaction.
    #[expect(clippy::too_many_arguments, reason = "audited revise needs id, update, metadata patch, principal, and audit draft")]
    fn update_authorized_with_metadata_audited(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Authorization-aware update with optional metadata and complete governed
    /// membership replacement in one transaction.
    #[expect(clippy::too_many_arguments, reason = "governed revise carries update, metadata, memberships, principal, and both audits")]
    fn update_authorized_with_metadata_contexts_audited(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        context_ids: Option<&[ContextId]>,
        principal: &str,
        audit: &AuditDraft,
        context_audit: Option<&ContextAuditDraft>,
    ) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Authorization-aware, optimistic-concurrency update with optional
    /// metadata and replacement embedding in one transaction. Replacement
    /// content without a vector is committed as needing re-embedding. Obtain
    /// `expected_revision` from [`Memory::optimistic_revision`].
    #[expect(clippy::too_many_arguments, reason = "atomic TUI revise needs revision, fields, metadata, embedding, principal, and audit")]
    fn update_authorized_if_unmodified_with_metadata_audited(
        &self,
        id: &MemoryId,
        expected_revision: i64,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        embedding: Option<&[f32]>,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Optimistic TUI update with optional complete governed membership
    /// replacement in the same transaction.
    #[expect(
        clippy::too_many_arguments,
        reason = "atomic governed TUI revise carries revision, fields, metadata, memberships, embedding, principal, and audits"
    )]
    fn update_authorized_if_unmodified_with_metadata_contexts_audited(
        &self,
        id: &MemoryId,
        expected_revision: i64,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        context_ids: Option<&[ContextId]>,
        embedding: Option<&[f32]>,
        principal: &str,
        audit: &AuditDraft,
        context_audit: Option<&ContextAuditDraft>,
    ) -> impl Future<Output = Result<AuthorizedUpdateOutcome, StoreError>> + Send;

    /// Authorization-aware delete. Checks write access before removing the memory.
    fn delete_authorized(&self, id: &MemoryId, principal: &str) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;

    /// Authorization-aware delete plus tombstone and audit row in one transaction.
    fn delete_authorized_audited(&self, id: &MemoryId, principal: &str, audit: &AuditDraft) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;

    /// Authorization-aware delete that refuses to remove a memory whose
    /// record revision changed after it was loaded. Obtain `expected_revision`
    /// from [`Memory::optimistic_revision`].
    fn delete_authorized_if_unmodified_audited(
        &self,
        id: &MemoryId,
        expected_revision: i64,
        principal: &str,
        audit: &AuditDraft,
    ) -> impl Future<Output = Result<WriteOutcome, StoreError>> + Send;

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
    /// Remove expired memories `principal` may write.
    ///
    /// The audit row and authorization tombstone for each deleted memory are
    /// committed atomically with that deletion. Inaccessible rows are neither
    /// deleted nor included in the returned count.
    fn evict_expired(&self, principal: &str, audit: &AuditDraft) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Remove every expired memory as explicit whole-store maintenance.
    ///
    /// The caller must enforce the operator boundary before invoking this
    /// store-level capability.
    fn evict_expired_all(&self, principal: &str, audit: &AuditDraft) -> impl Future<Output = Result<u64, StoreError>> + Send;

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

    /// Register or replace an operator-owned private compatibility definition.
    fn register_scope(&self, scope: ScopeDefinition) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// List operator-owned and migrated frozen compatibility definitions.
    fn list_scopes(&self) -> impl Future<Output = Result<Vec<ScopeDefinition>, StoreError>> + Send;

    /// Register or replace a principal-owned private custom context through
    /// the legacy scope compatibility surface.
    fn register_scope_for_principal(&self, scope: ScopeDefinition, principal: &str) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// List legacy compatibility definitions visible to one principal.
    fn list_scopes_for_principal(&self, principal: &str) -> impl Future<Output = Result<Vec<ScopeDefinition>, StoreError>> + Send;

    /// Upsert non-destructive metadata for a memory.
    fn upsert_metadata(&self, metadata: MemoryMetadata) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Upsert non-destructive metadata and audit the memory in one transaction.
    fn upsert_metadata_audited(&self, metadata: MemoryMetadata, audit: &AuditDraft) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Fetch non-destructive metadata for a memory.
    fn get_metadata(&self, memory_id: &MemoryId) -> impl Future<Output = Result<Option<MemoryMetadata>, StoreError>> + Send;

    /// Fetch non-destructive metadata for a batch without issuing one query
    /// per memory.
    fn get_metadata_batch(&self, memory_ids: &[MemoryId]) -> impl Future<Output = Result<HashMap<MemoryId, MemoryMetadata>, StoreError>> + Send;

    /// Return conservative migration/reporting counts for metadata.
    fn metadata_migration_report(&self) -> impl Future<Output = Result<MetadataMigrationReport, StoreError>> + Send;

    /// Add metadata rows for existing memories without rewriting original content.
    fn migrate_metadata(&self, dry_run: bool) -> impl Future<Output = Result<MetadataMigrationOutcome, StoreError>> + Send;

    /// Add metadata rows and audit each inserted row in one transaction.
    fn migrate_metadata_audited(&self, dry_run: bool, audit: &AuditDraft) -> impl Future<Output = Result<MetadataMigrationOutcome, StoreError>> + Send;
}

/// Combined trait for full memory store access: read, write, and admin.
///
/// Implementations must populate and advance [`Memory::record_revision`] for
/// every user-visible record mutation so optimistic writes remain sound.
/// Automatically implemented for any type that implements all three sub-traits.
pub trait MemoryStore: MemoryReader + MemoryWriter + MemoryAdmin + ContextStore {}

impl<T: MemoryReader + MemoryWriter + MemoryAdmin + ContextStore> MemoryStore for T {}

pub(crate) fn merge_metadata_patch(memory_id: MemoryId, patch: &MetadataPatch, existing: Option<&MemoryMetadata>, fallback_scope: Option<&str>, principal: &str) -> MemoryMetadata {
    let scope_key = patch
        .scope_key
        .clone()
        .or_else(|| existing.and_then(|metadata| metadata.scope_key.clone()))
        .or_else(|| fallback_scope.map(ToOwned::to_owned));
    let mut quality_flags = existing.map_or_else(Vec::new, |metadata| metadata.quality_flags.clone());
    quality_flags.retain(|flag| flag != "missing_scope");
    if scope_key
        .as_deref()
        .is_none_or(|key| crate::types::normalize_context_key(key) == crate::context::UNRESOLVED_CONTEXT_KEY)
    {
        quality_flags.push("missing_scope".into());
    }
    MemoryMetadata {
        memory_id,
        scope_key,
        summary: if patch.clear_summary {
            None
        } else {
            patch.summary.clone().or_else(|| existing.and_then(|metadata| metadata.summary.clone()))
        },
        agent_label: if patch.clear_agent_label {
            None
        } else {
            patch.agent_label.clone().or_else(|| existing.and_then(|metadata| metadata.agent_label.clone()))
        },
        created_by_principal: existing.and_then(|metadata| metadata.created_by_principal.clone()).or_else(|| Some(principal.to_owned())),
        quality_flags,
        schema_version: 1,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(memory_id: MemoryId, scope_key: Option<&str>, quality_flags: &[&str]) -> MemoryMetadata {
        MemoryMetadata {
            memory_id,
            scope_key: scope_key.map(ToOwned::to_owned),
            summary: None,
            agent_label: None,
            created_by_principal: Some("owner".into()),
            quality_flags: quality_flags.iter().map(ToString::to_string).collect(),
            schema_version: 1,
        }
    }

    #[test]
    fn merge_metadata_patch_recomputes_missing_scope_from_the_effective_scope() {
        let memory_id = MemoryId::new();
        let existing = metadata(memory_id, Some("project/localhold"), &["missing_scope", "manual"]);

        let merged = merge_metadata_patch(memory_id, &MetadataPatch::default(), Some(&existing), Some("inbox/unresolved"), "owner");

        assert_eq!(merged.scope_key.as_deref(), Some("project/localhold"));
        assert_eq!(merged.quality_flags, vec!["manual"]);
    }

    #[test]
    fn merge_metadata_patch_marks_a_contextless_effective_scope_unresolved() {
        let memory_id = MemoryId::new();

        let merged = merge_metadata_patch(memory_id, &MetadataPatch::default(), None, None, "owner");

        assert_eq!(merged.scope_key, None);
        assert_eq!(merged.quality_flags, vec!["missing_scope"]);
    }
}
