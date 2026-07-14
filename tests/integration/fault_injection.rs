//! Fault injection infrastructure for chaos testing.
//!
//! Provides [`ChaosStore`] and [`ChaosEmbedding`] wrappers that inject
//! configurable failures into the store and embedding layers, allowing
//! tests to verify resilience under adverse conditions.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use localhold::{
    embedding::{BoxFuture, EmbeddingProvider},
    error::{EmbeddingError, StoreError},
    store::{MemoryAdmin, MemoryReader, MemoryWithEmbedding, MemoryWriter, ReassignScopeOutcome},
    types::{
        AuditDraft, AuthorizedUpdateOutcome, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryStats, MemoryUpdate, MetadataMigrationOutcome, MetadataMigrationReport,
        QueryContext, ScopeDefinition, SearchResult, WriteOutcome,
    },
};
use parking_lot::Mutex;
use rand::RngExt as _;

// ---------------------------------------------------------------------------
// FaultPlan
// ---------------------------------------------------------------------------

/// Describes when and how a fault should be injected.
#[derive(Debug)]
#[non_exhaustive]
#[expect(
    variant_size_differences,
    reason = "Probabilistic variant is intentionally large; boxing adds indirection for a test-only type"
)]
#[expect(clippy::large_enum_variant, reason = "Probabilistic variant is large due to StdRng; acceptable for test-only infrastructure")]
pub(crate) enum FaultPlan {
    /// Never inject a fault -- pass through to the inner implementation.
    None,
    /// Always inject a fault.
    Always,
    /// Fail the next N calls, then succeed.
    CountDown(AtomicUsize),
    /// Fail exactly on the Nth call, then pass through all other calls.
    FailOnCall { call_index: usize, calls: AtomicUsize },
    /// Fail with probability p (0.0..1.0).
    Probabilistic {
        /// Failure probability (0.0 = never, 1.0 = always).
        probability: f64,
        /// Seeded RNG for reproducible fault injection.
        rng: Mutex<rand::rngs::StdRng>,
    },
}

/// Decrement-or-skip helper extracted to avoid excessive nesting inside `should_fail`.
#[expect(clippy::arithmetic_side_effects, reason = "v > 0 is checked before subtraction")]
const fn countdown_mapper(v: usize) -> Option<usize> {
    if v > 0_usize { Some(v - 1_usize) } else { None }
}

impl FaultPlan {
    /// Check whether a fault should be injected on this call.
    /// Returns `Some(StoreError)` if the call should fail, `None` if it should succeed.
    fn should_fail(&self) -> Option<StoreError> {
        match self {
            Self::None => None,
            Self::Always => Some(StoreError::Database("chaos: injected fault".into())),
            Self::CountDown(remaining) => {
                let prev = remaining.fetch_update(Ordering::SeqCst, Ordering::SeqCst, countdown_mapper);
                prev.ok().map(|_| StoreError::Database("chaos: countdown fault".into()))
            }
            Self::FailOnCall { call_index, calls } => {
                let call_number = calls.fetch_add(1_usize, Ordering::SeqCst).saturating_add(1_usize);
                (call_number == *call_index).then(|| StoreError::Database("chaos: targeted fault".into()))
            }
            Self::Probabilistic { probability, rng } => {
                let roll: f64 = rng.lock().random();
                (roll < *probability).then(|| StoreError::Database("chaos: probabilistic fault".into()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ChaosEmbedding
// ---------------------------------------------------------------------------

/// Embedding provider wrapper that injects faults into `embed` calls.
#[non_exhaustive]
pub(crate) struct ChaosEmbedding {
    inner: Arc<dyn EmbeddingProvider>,
    embed_plan: FaultPlan,
}

impl ChaosEmbedding {
    /// Create a new `ChaosEmbedding` wrapping the given provider with the given fault plan.
    pub(crate) fn new(inner: Arc<dyn EmbeddingProvider>, embed_plan: FaultPlan) -> Self {
        Self { inner, embed_plan }
    }
}

impl std::fmt::Debug for ChaosEmbedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaosEmbedding").field("embed_plan", &self.embed_plan).finish_non_exhaustive()
    }
}

impl EmbeddingProvider for ChaosEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        if self.embed_plan.should_fail().is_some() {
            return Box::pin(async { Err(EmbeddingError::Transient("chaos: injected embedding fault".into())) });
        }
        self.inner.embed(text)
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        self.inner.health_check()
    }
}

// ---------------------------------------------------------------------------
// ChaosStore
// ---------------------------------------------------------------------------

/// Store wrapper that injects faults into specific operations.
///
/// Each plan is wrapped in `Arc` so that `ChaosStore` can be `Clone`
/// (required by `LocalHoldEngine<S: Clone>`), with clones sharing the
/// same fault state.
#[non_exhaustive]
pub(crate) struct ChaosStore<S> {
    inner: S,
    store_plan: Arc<FaultPlan>,
    batch_store_plan: Arc<FaultPlan>,
    get_plan: Arc<FaultPlan>,
    search_plan: Arc<FaultPlan>,
    delete_plan: Arc<FaultPlan>,
}

impl<S: Clone> Clone for ChaosStore<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            store_plan: Arc::clone(&self.store_plan),
            batch_store_plan: Arc::clone(&self.batch_store_plan),
            get_plan: Arc::clone(&self.get_plan),
            search_plan: Arc::clone(&self.search_plan),
            delete_plan: Arc::clone(&self.delete_plan),
        }
    }
}

impl<S: std::fmt::Debug> std::fmt::Debug for ChaosStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaosStore")
            .field("inner", &self.inner)
            .field("store_plan", &self.store_plan)
            .field("batch_store_plan", &self.batch_store_plan)
            .field("get_plan", &self.get_plan)
            .field("search_plan", &self.search_plan)
            .field("delete_plan", &self.delete_plan)
            .finish()
    }
}

impl<S: MemoryReader + Send + Sync> MemoryReader for ChaosStore<S> {
    async fn get(&self, id: &MemoryId, principal: Option<&str>) -> Result<Option<Memory>, StoreError> {
        if let Some(err) = self.get_plan.should_fail() {
            return Err(err);
        }
        self.inner.get(id, principal).await
    }

    async fn search_by_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: &MemoryFilter,
        ctx: &QueryContext,
        max_distance: Option<f64>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if let Some(err) = self.search_plan.should_fail() {
            return Err(err);
        }
        self.inner.search_by_embedding(embedding, limit, filter, ctx, max_distance).await
    }

    async fn search_by_text(&self, query: &str, limit: usize, filter: &MemoryFilter, ctx: &QueryContext) -> Result<Vec<SearchResult>, StoreError> {
        if let Some(err) = self.search_plan.should_fail() {
            return Err(err);
        }
        self.inner.search_by_text(query, limit, filter, ctx).await
    }

    async fn search_by_fts(&self, query: &str, limit: usize, filter: &MemoryFilter, ctx: &QueryContext, context: Option<&str>) -> Result<Vec<SearchResult>, StoreError> {
        if let Some(err) = self.search_plan.should_fail() {
            return Err(err);
        }
        self.inner.search_by_fts(query, limit, filter, ctx, context).await
    }

    async fn list(&self, filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<Memory>, StoreError> {
        self.inner.list(filter, ctx).await
    }

    async fn count(&self, filter: MemoryFilter, ctx: QueryContext, top_tags_limit: usize) -> Result<MemoryStats, StoreError> {
        self.inner.count(filter, ctx, top_tags_limit).await
    }

    async fn list_for_reembed(&self, limit: usize) -> Result<Vec<(MemoryId, String, i64)>, StoreError> {
        self.inner.list_for_reembed(limit).await
    }

    async fn get_for_reembed(&self, id: &MemoryId, principal: &str) -> Result<Option<(String, i64)>, StoreError> {
        self.inner.get_for_reembed(id, principal).await
    }

    async fn list_with_embeddings(&self, scopes_any: Option<&[String]>, limit: usize) -> Result<Vec<MemoryWithEmbedding>, StoreError> {
        self.inner.list_with_embeddings(scopes_any, limit).await
    }

    async fn query_audit_log(&self, memory_id: &MemoryId, limit: usize) -> Result<Vec<localhold::types::AuditEntry>, StoreError> {
        self.inner.query_audit_log(memory_id, limit).await
    }

    async fn get_tombstone(&self, memory_id: &MemoryId) -> Result<Option<localhold::types::MemoryTombstone>, StoreError> {
        self.inner.get_tombstone(memory_id).await
    }

    async fn fetch_embeddings_for_ids(&self, ids: &[MemoryId]) -> Result<std::collections::HashMap<MemoryId, Vec<f32>>, StoreError> {
        self.inner.fetch_embeddings_for_ids(ids).await
    }

    async fn find_embedding_neighbors(&self, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<(MemoryId, f64)>, StoreError> {
        self.inner.find_embedding_neighbors(embedding, max_l2_distance, limit).await
    }
}

impl<S: MemoryWriter + Send + Sync> MemoryWriter for ChaosStore<S> {
    async fn store(&self, memory: &Memory, embedding: Option<&[f32]>) -> Result<MemoryId, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store(memory, embedding).await
    }

    async fn store_audited(&self, memory: &Memory, embedding: Option<&[f32]>, audit: &AuditDraft) -> Result<MemoryId, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_audited(memory, embedding, audit).await
    }

    async fn store_with_supersession(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId) -> Result<MemoryId, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_with_supersession(memory, embedding, supersedes_id).await
    }

    async fn store_with_supersession_audited(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId, audit: &AuditDraft) -> Result<MemoryId, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_with_supersession_audited(memory, embedding, supersedes_id, audit).await
    }

    async fn store_with_metadata(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: Option<&MemoryId>, metadata: &MemoryMetadata) -> Result<MemoryId, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_with_metadata(memory, embedding, supersedes_id, metadata).await
    }

    async fn store_with_metadata_audited(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
        audit: &AuditDraft,
    ) -> Result<MemoryId, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_with_metadata_audited(memory, embedding, supersedes_id, metadata, audit).await
    }

    async fn store_batch(&self, memories: &[MemoryWithEmbedding]) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(err) = self.batch_store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_batch(memories).await
    }

    async fn store_batch_audited(&self, memories: &[MemoryWithEmbedding], audits: &[AuditDraft]) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(err) = self.batch_store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_batch_audited(memories, audits).await
    }

    async fn store_batch_with_supersession(&self, memories: &[MemoryWithEmbedding], supersedes: &[Option<MemoryId>]) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(err) = self.batch_store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_batch_with_supersession(memories, supersedes).await
    }

    async fn store_batch_with_supersession_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        audits: &[AuditDraft],
    ) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(err) = self.batch_store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_batch_with_supersession_audited(memories, supersedes, audits).await
    }

    async fn store_batch_with_metadata(&self, memories: &[MemoryWithEmbedding], supersedes: &[Option<MemoryId>], metadata: &[MemoryMetadata]) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(err) = self.batch_store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_batch_with_metadata(memories, supersedes, metadata).await
    }

    async fn store_batch_with_metadata_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
        audits: &[AuditDraft],
    ) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(err) = self.batch_store_plan.should_fail() {
            return Err(err);
        }
        self.inner.store_batch_with_metadata_audited(memories, supersedes, metadata, audits).await
    }

    async fn update(&self, id: &MemoryId, update: &MemoryUpdate) -> Result<bool, StoreError> {
        self.inner.update(id, update).await
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        if let Some(err) = self.delete_plan.should_fail() {
            return Err(err);
        }
        self.inner.delete(id).await
    }

    async fn set_embedding(&self, id: &MemoryId, embedding: &[f32], expected_revision: i64) -> Result<(), StoreError> {
        self.inner.set_embedding(id, embedding, expected_revision).await
    }

    async fn claim_for_reembed(&self, limit: usize) -> Result<Vec<localhold::store::ReembedClaim>, StoreError> {
        self.inner.claim_for_reembed(limit).await
    }

    async fn release_embedding_claim(&self, id: &MemoryId, expected_revision: i64, claim_token: &str) -> Result<bool, StoreError> {
        self.inner.release_embedding_claim(id, expected_revision, claim_token).await
    }

    async fn update_authorized(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.inner.update_authorized(id, update, principal).await
    }

    async fn update_authorized_audited(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str, audit: &AuditDraft) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.inner.update_authorized_audited(id, update, principal, audit).await
    }

    async fn update_authorized_with_metadata_audited(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&localhold::types::MetadataPatch>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.inner.update_authorized_with_metadata_audited(id, update, metadata_patch, principal, audit).await
    }

    async fn update_authorized_if_unmodified_with_metadata_audited(
        &self,
        id: &MemoryId,
        expected_revision: i64,
        update: &MemoryUpdate,
        metadata_patch: Option<&localhold::types::MetadataPatch>,
        embedding: Option<&[f32]>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        if let Some(err) = self.store_plan.should_fail() {
            return Err(err);
        }
        self.inner
            .update_authorized_if_unmodified_with_metadata_audited(id, expected_revision, update, metadata_patch, embedding, principal, audit)
            .await
    }

    async fn delete_authorized(&self, id: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        if let Some(err) = self.delete_plan.should_fail() {
            return Err(err);
        }
        self.inner.delete_authorized(id, principal).await
    }

    async fn delete_authorized_audited(&self, id: &MemoryId, principal: &str, audit: &AuditDraft) -> Result<WriteOutcome, StoreError> {
        if let Some(err) = self.delete_plan.should_fail() {
            return Err(err);
        }
        self.inner.delete_authorized_audited(id, principal, audit).await
    }

    async fn delete_authorized_if_unmodified_audited(&self, id: &MemoryId, expected_revision: i64, principal: &str, audit: &AuditDraft) -> Result<WriteOutcome, StoreError> {
        if let Some(err) = self.delete_plan.should_fail() {
            return Err(err);
        }
        self.inner.delete_authorized_if_unmodified_audited(id, expected_revision, principal, audit).await
    }

    async fn bulk_delete_ids(&self, ids: Vec<MemoryId>, principal: &str) -> Result<localhold::store::BulkAuthOutcome, StoreError> {
        self.inner.bulk_delete_ids(ids, principal).await
    }

    async fn bulk_delete_ids_audited(&self, ids: Vec<MemoryId>, principal: &str, audit: &AuditDraft) -> Result<localhold::store::BulkAuthOutcome, StoreError> {
        self.inner.bulk_delete_ids_audited(ids, principal, audit).await
    }

    async fn bulk_update_ids(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<localhold::store::BulkAuthOutcome, StoreError> {
        self.inner.bulk_update_ids(ids, update, principal, now).await
    }

    async fn bulk_update_ids_audited(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
        audit: &AuditDraft,
    ) -> Result<localhold::store::BulkAuthOutcome, StoreError> {
        self.inner.bulk_update_ids_audited(ids, update, principal, now, audit).await
    }

    async fn record_search_impression(&self, ids: &[MemoryId]) -> Result<(), StoreError> {
        self.inner.record_search_impression(ids).await
    }

    async fn record_memory_use(
        &self,
        ids: &[MemoryId],
        principal: &str,
        event_weight: f64,
        now: chrono::DateTime<chrono::Utc>,
        activity_half_life_hours: f64,
    ) -> Result<localhold::store::RecordUseOutcome, StoreError> {
        self.inner.record_memory_use(ids, principal, event_weight, now, activity_half_life_hours).await
    }

    async fn write_audit_entry(
        &self,
        memory_id: &MemoryId,
        action: localhold::types::AuditAction,
        principal: Option<&str>,
        timestamp: chrono::DateTime<chrono::Utc>,
        details: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        self.inner.write_audit_entry(memory_id, action, principal, timestamp, details).await
    }

    async fn mark_superseded_by(&self, id: &MemoryId, superseded_by: &MemoryId) -> Result<bool, StoreError> {
        self.inner.mark_superseded_by(id, superseded_by).await
    }

    async fn mark_superseded_by_authorized(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.inner.mark_superseded_by_authorized(id, superseded_by, principal).await
    }

    async fn mark_superseded_by_authorized_audited(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str, audit: &AuditDraft) -> Result<WriteOutcome, StoreError> {
        self.inner.mark_superseded_by_authorized_audited(id, superseded_by, principal, audit).await
    }
}

impl<S: MemoryAdmin + Send + Sync> MemoryAdmin for ChaosStore<S> {
    async fn evict_expired(&self) -> Result<u64, StoreError> {
        self.inner.evict_expired().await
    }

    async fn reassign_scope(&self, from_scope: &str, to_scope: &str, origin_conversation: Option<&str>, principal: &str) -> Result<ReassignScopeOutcome, StoreError> {
        self.inner.reassign_scope(from_scope, to_scope, origin_conversation, principal).await
    }

    async fn reassign_scope_audited(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<ReassignScopeOutcome, StoreError> {
        self.inner.reassign_scope_audited(from_scope, to_scope, origin_conversation, principal, audit).await
    }

    async fn register_scope(&self, scope: ScopeDefinition) -> Result<(), StoreError> {
        self.inner.register_scope(scope).await
    }

    async fn list_scopes(&self) -> Result<Vec<ScopeDefinition>, StoreError> {
        self.inner.list_scopes().await
    }

    async fn upsert_metadata(&self, metadata: MemoryMetadata) -> Result<(), StoreError> {
        self.inner.upsert_metadata(metadata).await
    }

    async fn upsert_metadata_audited(&self, metadata: MemoryMetadata, audit: &AuditDraft) -> Result<(), StoreError> {
        self.inner.upsert_metadata_audited(metadata, audit).await
    }

    async fn get_metadata(&self, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, StoreError> {
        self.inner.get_metadata(memory_id).await
    }

    async fn metadata_migration_report(&self) -> Result<MetadataMigrationReport, StoreError> {
        self.inner.metadata_migration_report().await
    }

    async fn migrate_metadata(&self, registered_scope_keys: &[String], dry_run: bool) -> Result<MetadataMigrationOutcome, StoreError> {
        self.inner.migrate_metadata(registered_scope_keys, dry_run).await
    }

    async fn migrate_metadata_audited(&self, registered_scope_keys: &[String], dry_run: bool, audit: &AuditDraft) -> Result<MetadataMigrationOutcome, StoreError> {
        self.inner.migrate_metadata_audited(registered_scope_keys, dry_run, audit).await
    }
}

// ---------------------------------------------------------------------------
// Constructor helpers
// ---------------------------------------------------------------------------

/// Create a `ChaosStore` where `store` always fails.
pub(crate) fn chaos_store_always_fail_store<S>(inner: S) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::Always),
        batch_store_plan: Arc::new(FaultPlan::Always),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` where `store` fails the first `count` calls.
pub(crate) fn chaos_store_countdown_store<S>(inner: S, count: usize) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        batch_store_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` where batch stores always fail and a single store
/// fails exactly on `call_index`.
pub(crate) fn chaos_store_fail_batch_and_store_call<S>(inner: S, call_index: usize) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::FailOnCall {
            call_index,
            calls: AtomicUsize::new(0_usize),
        }),
        batch_store_plan: Arc::new(FaultPlan::Always),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` where single-item stores fail exactly on `call_index`
/// and batch stores pass through.
pub(crate) fn chaos_store_fail_on_store_call<S>(inner: S, call_index: usize) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::FailOnCall {
            call_index,
            calls: AtomicUsize::new(0_usize),
        }),
        batch_store_plan: Arc::new(FaultPlan::None),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` where `search` always fails.
pub(crate) fn chaos_store_always_fail_search<S>(inner: S) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::None),
        batch_store_plan: Arc::new(FaultPlan::None),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::Always),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` where `get` fails the first `count` calls.
pub(crate) fn chaos_store_countdown_get<S>(inner: S, count: usize) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::None),
        batch_store_plan: Arc::new(FaultPlan::None),
        get_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` with probabilistic store failures.
pub(crate) fn chaos_store_probabilistic_store<S>(inner: S, probability: f64, seed: u64) -> ChaosStore<S> {
    use rand::SeedableRng as _;
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::Probabilistic {
            probability,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed)),
        }),
        batch_store_plan: Arc::new(FaultPlan::Probabilistic {
            probability,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed.wrapping_add(1_u64))),
        }),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` with probabilistic search failures.
pub(crate) fn chaos_store_probabilistic_search<S>(inner: S, probability: f64, seed: u64) -> ChaosStore<S> {
    use rand::SeedableRng as _;
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::None),
        batch_store_plan: Arc::new(FaultPlan::None),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::Probabilistic {
            probability,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed)),
        }),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` with multiple probabilistic fault plans.
pub(crate) fn chaos_store_multi_probabilistic<S>(inner: S, store_prob: f64, search_prob: f64, get_prob: f64, seed: u64) -> ChaosStore<S> {
    use rand::SeedableRng as _;
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::Probabilistic {
            probability: store_prob,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed)),
        }),
        batch_store_plan: Arc::new(FaultPlan::Probabilistic {
            probability: store_prob,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed.wrapping_add(3_u64))),
        }),
        get_plan: Arc::new(FaultPlan::Probabilistic {
            probability: get_prob,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed.wrapping_add(1_u64))),
        }),
        search_plan: Arc::new(FaultPlan::Probabilistic {
            probability: search_prob,
            rng: Mutex::new(rand::rngs::StdRng::seed_from_u64(seed.wrapping_add(2_u64))),
        }),
        delete_plan: Arc::new(FaultPlan::None),
    }
}

/// Create a `ChaosStore` where all operation plans fail for the first `count` calls (per plan).
///
/// Each plan has an independent counter, so e.g. the first `count` store calls fail
/// independently of the first `count` get calls.
#[expect(dead_code, reason = "available for future chaos test scenarios that need all-plan countdowns")]
pub(crate) fn chaos_store_countdown_all<S>(inner: S, count: usize) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        batch_store_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        get_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        search_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
        delete_plan: Arc::new(FaultPlan::CountDown(AtomicUsize::new(count))),
    }
}

/// Create a `ChaosStore` with no faults (passthrough).
pub(crate) fn chaos_store_passthrough<S>(inner: S) -> ChaosStore<S> {
    ChaosStore {
        inner,
        store_plan: Arc::new(FaultPlan::None),
        batch_store_plan: Arc::new(FaultPlan::None),
        get_plan: Arc::new(FaultPlan::None),
        search_plan: Arc::new(FaultPlan::None),
        delete_plan: Arc::new(FaultPlan::None),
    }
}
