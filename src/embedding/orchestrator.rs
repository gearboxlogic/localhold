//! Embedding orchestrator — enforces the store-then-embed invariant.
//!
//! The [`EmbeddingOrchestrator`] wraps the store and embedding provider,
//! ensuring that every memory write that requires embedding is followed by
//! a background embed task. This makes it structurally impossible to forget
//! the embed step.

use std::{collections::HashSet, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::{
    background_tasks::{BackgroundTaskKind, BackgroundTasks, EmbedAdmission},
    embedding::EmbeddingProvider,
    error::{EngineError, StoreError, ValidationError},
    store::{MemoryStore, MemoryWithEmbedding, ReembedClaim},
    types::{AuditDraft, AuthorizedUpdateOutcome, Memory, MemoryId, MemoryUpdate, V2MemoryMetadata, V2MetadataPatch, WriteOutcome},
};

/// Default maximum concurrent embedding tasks.
const DEFAULT_MAX_CONCURRENT_EMBEDS: usize = 8;

type EmbedKey = (MemoryId, i64);

/// Orchestrates memory writes with automatic background embedding.
///
/// Wraps a store and an embedding provider. Any write that produces content
/// needing an embedding is automatically followed by a background embed task.
/// This eliminates the risk of "forgetting" to embed after a store.
#[derive(Clone)]
pub(crate) struct EmbeddingOrchestrator<S: MemoryStore + Clone + std::fmt::Debug + 'static> {
    store: S,
    embedding: Arc<dyn EmbeddingProvider>,
    background_tasks: Arc<BackgroundTasks>,
    /// Bounds the number of concurrent embedding tasks to prevent fan-out overload.
    embed_semaphore: Arc<Semaphore>,
    /// Coalesces duplicate in-process attempts to embed the same memory revision.
    inflight_embeds: Arc<Mutex<HashSet<EmbedKey>>>,
    /// Tracks durable claims owned by currently running embed tasks so shutdown
    /// timeout can clear leases before aborting those tasks.
    active_claimed_embeds: Arc<Mutex<HashSet<ActiveEmbedClaim>>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ActiveEmbedClaim {
    id: MemoryId,
    embedding_revision: i64,
    claim_token: String,
}

struct InFlightEmbed {
    key: EmbedKey,
    inflight_embeds: Arc<Mutex<HashSet<EmbedKey>>>,
}

struct ActiveEmbedClaimGuard {
    claim: ActiveEmbedClaim,
    active_claimed_embeds: Arc<Mutex<HashSet<ActiveEmbedClaim>>>,
}

impl Drop for InFlightEmbed {
    fn drop(&mut self) {
        let _removed = self.inflight_embeds.lock().remove(&self.key);
    }
}

impl Drop for ActiveEmbedClaimGuard {
    fn drop(&mut self) {
        let _removed = self.active_claimed_embeds.lock().remove(&self.claim);
    }
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> std::fmt::Debug for EmbeddingOrchestrator<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingOrchestrator").field("store", &self.store).finish_non_exhaustive()
    }
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> EmbeddingOrchestrator<S> {
    /// Create a new orchestrator with default concurrency limit.
    pub(crate) fn new(store: S, embedding: Arc<dyn EmbeddingProvider>, background_tasks: Arc<BackgroundTasks>) -> Self {
        Self {
            store,
            embedding,
            background_tasks,
            embed_semaphore: Arc::new(Semaphore::new(DEFAULT_MAX_CONCURRENT_EMBEDS)),
            inflight_embeds: Arc::new(Mutex::new(HashSet::new())),
            active_claimed_embeds: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Drain tracked background tasks, releasing active durable embed claims if shutdown times out.
    pub(crate) async fn shutdown(&self, timeout: std::time::Duration) {
        self.background_tasks
            .shutdown_with_cleanup(timeout, || async { self.release_active_embedding_claims().await })
            .await;
    }

    // -- public orchestrated operations -------------------------------------

    /// Store a memory and spawn a background embedding task.
    ///
    /// When `supersedes` is provided, the referenced memory's `superseded_by`
    /// is atomically set to the new memory's ID within the same transaction.
    ///
    /// This is the single entry point for "store then embed" — callers never
    /// need to remember to spawn the embed task themselves.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` if the persistence layer rejects the write
    /// or if the superseded memory does not exist.
    pub(crate) async fn store_and_embed(&self, admission: &EmbedAdmission, memory: Memory, supersedes: Option<&MemoryId>, audit: &AuditDraft) -> Result<MemoryId, EngineError> {
        let content = memory.content.clone();
        let id = if let Some(supersedes_id) = supersedes {
            self.store.store_with_supersession_audited(&memory, None, supersedes_id, audit).await?
        } else {
            self.store.store_audited(&memory, None, audit).await?
        };
        let _queued = self.spawn_embed_task_or_run_inline(admission, id, content, 0).await;
        Ok(id)
    }

    /// Store a memory and required v2 metadata atomically, then spawn a background embedding task.
    #[expect(
        clippy::too_many_arguments,
        reason = "audited v2 write needs admission, memory, supersession metadata, v2 metadata, and audit draft"
    )]
    pub(crate) async fn store_and_embed_with_metadata(
        &self,
        admission: &EmbedAdmission,
        memory: Memory,
        supersedes: Option<&MemoryId>,
        metadata: &V2MemoryMetadata,
        audit: &AuditDraft,
    ) -> Result<MemoryId, EngineError> {
        let content = memory.content.clone();
        let id = self.store.store_with_v2_metadata_audited(&memory, None, supersedes, metadata, audit).await?;
        let _queued = self.spawn_embed_task_or_run_inline(admission, id, content, 0).await;
        Ok(id)
    }

    /// Store multiple memories atomically and spawn embed tasks for each.
    ///
    /// Validates non-empty and batch size within the caller-provided limit.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if the batch is empty or oversized,
    /// or `EngineError::Store` if the batch write fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "batch write needs admission, memories, supersession metadata, audit drafts, and caller cap"
    )]
    pub(crate) async fn batch_store_and_embed(
        &self,
        admission: &EmbedAdmission,
        memories: Vec<Memory>,
        supersedes_list: &[Option<MemoryId>],
        audits: &[AuditDraft],
        max_batch_size: usize,
    ) -> Result<Vec<MemoryId>, EngineError> {
        if memories.is_empty() {
            return Err(ValidationError::new("memories", "batch cannot be empty").into());
        }
        if memories.len() > max_batch_size {
            return Err(ValidationError::new("memories", format!("batch size {} exceeds maximum of {max_batch_size}", memories.len())).into());
        }

        let has_any_supersession = supersedes_list.iter().any(Option::is_some);

        let mut contents: Vec<Arc<str>> = Vec::with_capacity(memories.len());
        let memories_for_store: Vec<MemoryWithEmbedding> = memories
            .into_iter()
            .map(|memory| {
                contents.push(Arc::from(memory.content.as_str()));
                MemoryWithEmbedding { memory, embedding: None }
            })
            .collect();

        let ids = if has_any_supersession {
            // Store with per-item supersession handling via individual stores
            self.store.store_batch_with_supersession_audited(&memories_for_store, supersedes_list, audits).await?
        } else {
            self.store.store_batch_audited(&memories_for_store, audits).await?
        };

        for (content, &id) in contents.iter().zip(ids.iter()) {
            let _queued = self.spawn_embed_task_shared_or_run_inline(admission, id, Arc::clone(content), 0).await;
        }

        Ok(ids)
    }

    /// Store multiple memories and required v2 metadata atomically, then spawn embed tasks.
    #[expect(
        clippy::too_many_arguments,
        reason = "batch v2 write needs admission, memories, supersession metadata, v2 metadata, and caller cap"
    )]
    pub(crate) async fn batch_store_and_embed_with_metadata(
        &self,
        admission: &EmbedAdmission,
        memories: Vec<Memory>,
        supersedes_list: &[Option<MemoryId>],
        metadata: &[V2MemoryMetadata],
        audits: &[AuditDraft],
        max_batch_size: usize,
    ) -> Result<Vec<MemoryId>, EngineError> {
        if memories.is_empty() {
            return Err(ValidationError::new("memories", "batch cannot be empty").into());
        }
        if memories.len() > max_batch_size {
            return Err(ValidationError::new("memories", format!("batch size {} exceeds maximum of {max_batch_size}", memories.len())).into());
        }

        let mut contents: Vec<Arc<str>> = Vec::with_capacity(memories.len());
        let memories_for_store: Vec<MemoryWithEmbedding> = memories
            .into_iter()
            .map(|memory| {
                contents.push(Arc::from(memory.content.as_str()));
                MemoryWithEmbedding { memory, embedding: None }
            })
            .collect();

        let ids = self
            .store
            .store_batch_with_v2_metadata_audited(&memories_for_store, supersedes_list, metadata, audits)
            .await?;

        for (content, &id) in contents.iter().zip(ids.iter()) {
            let _queued = self.spawn_embed_task_shared_or_run_inline(admission, id, Arc::clone(content), 0).await;
        }

        Ok(ids)
    }

    /// Update a memory with authorization check, spawning a re-embed task
    /// when content changes.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    #[expect(clippy::too_many_arguments, reason = "audited update needs embed admission, id, update, principal, and audit draft")]
    pub(crate) async fn update_and_maybe_reembed(
        &self,
        embed_admission: Option<&EmbedAdmission>,
        id: MemoryId,
        update: &MemoryUpdate,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<AuthorizedUpdateOutcome, EngineError> {
        let new_content = update.content.clone();
        let outcome = self.store.update_authorized_audited(&id, update, principal, audit).await?;
        maybe_reembed_after_update(self, embed_admission, id, new_content, &outcome).await;
        Ok(outcome)
    }

    /// Update a memory plus optional v2 metadata in one store transaction,
    /// then spawn a re-embed task when content changes.
    #[expect(
        clippy::too_many_arguments,
        reason = "audited revise needs embed admission, id, update, metadata patch, principal, and audit draft"
    )]
    pub(crate) async fn update_with_v2_metadata_and_maybe_reembed(
        &self,
        embed_admission: Option<&EmbedAdmission>,
        id: MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&V2MetadataPatch>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<AuthorizedUpdateOutcome, EngineError> {
        let new_content = update.content.clone();
        let outcome = self.store.update_authorized_with_v2_metadata_audited(&id, update, metadata_patch, principal, audit).await?;
        maybe_reembed_after_update(self, embed_admission, id, new_content, &outcome).await;
        Ok(outcome)
    }

    // -- accessors for engine -----------------------------------------------

    /// Borrow the underlying store.
    pub(crate) const fn store(&self) -> &S {
        &self.store
    }

    /// Borrow the embedding provider.
    pub(crate) fn embedding(&self) -> &Arc<dyn EmbeddingProvider> {
        &self.embedding
    }

    /// Borrow the shared background task coordinator.
    pub(crate) const fn background_tasks(&self) -> &Arc<BackgroundTasks> {
        &self.background_tasks
    }

    // -- embed task spawning (shared by all orchestrated operations) ---------

    /// Reserve admission for an operation that must be able to queue embed work.
    pub(crate) fn begin_embed_admission(&self) -> Result<EmbedAdmission, EngineError> {
        self.background_tasks.begin_embed_admission()
    }

    fn begin_inflight_embed(&self, id: MemoryId, expected_revision: i64) -> Option<InFlightEmbed> {
        let key = (id, expected_revision);
        let inserted = {
            let mut inflight = self.inflight_embeds.lock();
            inflight.insert(key)
        };
        if !inserted {
            return None;
        }
        Some(InFlightEmbed {
            key,
            inflight_embeds: Arc::clone(&self.inflight_embeds),
        })
    }

    fn track_active_claim(&self, claim: &ReembedClaim) -> ActiveEmbedClaimGuard {
        let active_claim = ActiveEmbedClaim {
            id: claim.id,
            embedding_revision: claim.embedding_revision,
            claim_token: claim.claim_token.clone(),
        };
        let _inserted = self.active_claimed_embeds.lock().insert(active_claim.clone());
        ActiveEmbedClaimGuard {
            claim: active_claim,
            active_claimed_embeds: Arc::clone(&self.active_claimed_embeds),
        }
    }

    async fn release_active_embedding_claims(&self) {
        let claims: Vec<ActiveEmbedClaim> = self.active_claimed_embeds.lock().iter().cloned().collect();
        for claim in claims {
            release_embedding_claim(&self.store, claim.id, claim.embedding_revision, &claim.claim_token).await;
        }
    }

    pub(crate) async fn spawn_embed_task_or_run_inline(&self, admission: &EmbedAdmission, id: MemoryId, content: String, expected_revision: i64) -> bool {
        self.spawn_embed_task_shared_or_run_inline(admission, id, Arc::<str>::from(content), expected_revision)
            .await
    }

    async fn spawn_embed_task_shared_or_run_inline(&self, admission: &EmbedAdmission, id: MemoryId, content: Arc<str>, expected_revision: i64) -> bool {
        let Some(inflight) = self.begin_inflight_embed(id, expected_revision) else {
            info!(memory_id = %id, expected_revision, "embed task already in flight, skipping duplicate");
            return false;
        };

        let store = self.store.clone();
        let embedding_provider = Arc::clone(&self.embedding);
        let semaphore = Arc::clone(&self.embed_semaphore);
        let spawned_content = Arc::clone(&content);
        if admission.spawn(BackgroundTaskKind::Embed, async move {
            let _inflight = inflight;
            run_embed_task(embedding_provider, store, semaphore, id, spawned_content, expected_revision, None).await;
        }) {
            return true;
        }

        warn!(memory_id = %id, "embed task admission timed out during shutdown; embedding inline");
        let Some(inflight) = self.begin_inflight_embed(id, expected_revision) else {
            info!(memory_id = %id, expected_revision, "embed task already in flight, skipping duplicate");
            return false;
        };
        run_embed_task(
            Arc::clone(&self.embedding),
            self.store.clone(),
            Arc::clone(&self.embed_semaphore),
            id,
            content,
            expected_revision,
            None,
        )
        .await;
        drop(inflight);
        true
    }

    pub(crate) async fn spawn_claimed_embed_task_or_run_inline(&self, admission: &EmbedAdmission, claim: ReembedClaim) -> bool {
        let Some(inflight) = self.begin_inflight_embed(claim.id, claim.embedding_revision) else {
            info!(memory_id = %claim.id, expected_revision = claim.embedding_revision, "embed task already in flight, releasing duplicate claim");
            release_embedding_claim(&self.store, claim.id, claim.embedding_revision, &claim.claim_token).await;
            return false;
        };

        let id = claim.id;
        let expected_revision = claim.embedding_revision;
        let active_claim = self.track_active_claim(&claim);
        let content = Arc::<str>::from(claim.content);
        let claim_token = claim.claim_token;
        let store = self.store.clone();
        let embedding_provider = Arc::clone(&self.embedding);
        let semaphore = Arc::clone(&self.embed_semaphore);
        let spawned_content = Arc::clone(&content);
        let spawned_claim_token = claim_token.clone();
        if admission.spawn(BackgroundTaskKind::Embed, async move {
            let _inflight = inflight;
            let _active_claim = active_claim;
            run_embed_task(embedding_provider, store, semaphore, id, spawned_content, expected_revision, Some(spawned_claim_token)).await;
        }) {
            return true;
        }

        warn!(memory_id = %id, "embed task admission timed out during shutdown; releasing claim");
        release_embedding_claim(&self.store, id, expected_revision, &claim_token).await;
        false
    }
}

/// Embed content and store the resulting vector, with retry for transient failures.
#[expect(clippy::too_many_arguments, reason = "embed execution needs provider, store, id, content, revision, and optional claim token")]
async fn embed_and_store<S: MemoryStore>(
    embedding_provider: Arc<dyn EmbeddingProvider>,
    store: S,
    id: MemoryId,
    content: &str,
    expected_revision: i64,
    claim_token: Option<String>,
) {
    let emb = match embedding_provider.embed(content).await {
        Ok(emb) => emb,
        Err(e) => {
            warn!(memory_id = %id, error = %e, "embedding failed after retries");
            if let Some(token) = claim_token.as_deref() {
                release_embedding_claim(&store, id, expected_revision, token).await;
            }
            return;
        }
    };
    match store.set_embedding(&id, &emb, expected_revision).await {
        Ok(()) => {
            info!(memory_id = %id, "embedded memory");
        }
        Err(StoreError::Conflict(reason)) => {
            info!(memory_id = %id, reason = %reason, "revision mismatch, skipping");
            if let Some(token) = claim_token.as_deref() {
                release_embedding_claim(&store, id, expected_revision, token).await;
            }
        }
        Err(e) => {
            warn!(memory_id = %id, error = %e, "failed to store embedding");
            if let Some(token) = claim_token.as_deref() {
                release_embedding_claim(&store, id, expected_revision, token).await;
            }
        }
    }
}

async fn release_embedding_claim<S: MemoryStore>(store: &S, id: MemoryId, expected_revision: i64, claim_token: &str) {
    if let Err(e) = store.release_embedding_claim(&id, expected_revision, claim_token).await {
        warn!(memory_id = %id, error = %e, "failed to release embedding claim");
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "embed execution needs provider, store, semaphore, id, content, and revision — all semantically distinct"
)]
async fn run_embed_task<S: MemoryStore>(
    embedding_provider: Arc<dyn EmbeddingProvider>,
    store: S,
    semaphore: Arc<Semaphore>,
    id: MemoryId,
    content: Arc<str>,
    expected_revision: i64,
    claim_token: Option<String>,
) {
    let Ok(_guard) = semaphore.acquire().await else {
        tracing::error!("embed semaphore closed unexpectedly, skipping embed task");
        if let Some(token) = claim_token.as_deref() {
            release_embedding_claim(&store, id, expected_revision, token).await;
        }
        return;
    };
    embed_and_store(embedding_provider, store, id, &content, expected_revision, claim_token).await;
}

/// Spawn a re-embed task when content changed and the update was applied.
async fn maybe_reembed_after_update<S: MemoryStore + Clone + std::fmt::Debug + 'static>(
    orchestrator: &EmbeddingOrchestrator<S>,
    embed_admission: Option<&EmbedAdmission>,
    id: MemoryId,
    new_content: Option<String>,
    outcome: &AuthorizedUpdateOutcome,
) {
    if outcome.outcome != WriteOutcome::Applied {
        return;
    }
    if let (Some(admission), Some(content), Some(revision)) = (embed_admission, new_content, outcome.reembed_revision) {
        let _queued = orchestrator.spawn_embed_task_or_run_inline(admission, id, content, revision).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc as StdArc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;
    use crate::{
        background_tasks::BackgroundTasks,
        error::EmbeddingError,
        store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
        types::{AccessPolicy, Importance, Memory, MemoryType, Provenance},
    };

    struct FixedSizeProvider;

    impl EmbeddingProvider for FixedSizeProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> crate::embedding::BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Ok(vec![1.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS]) })
        }

        fn health_check(&self) -> crate::embedding::BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct BlockingProvider {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        embed_calls: AtomicUsize,
    }

    impl BlockingProvider {
        fn new() -> Self {
            Self {
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                embed_calls: AtomicUsize::new(0),
            }
        }
    }

    impl EmbeddingProvider for BlockingProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> crate::embedding::BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async move {
                let _previous = self.embed_calls.fetch_add(1, Ordering::Relaxed);
                self.started.notify_waiters();
                self.release.notified().await;
                Ok(vec![1.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS])
            })
        }

        fn health_check(&self) -> crate::embedding::BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn test_memory(content: &str) -> Memory {
        Memory {
            id: MemoryId::new(),
            content: content.into(),
            tags: vec![],
            provenance: Provenance::default(),
            access_policy: AccessPolicy::Public,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::default(),
            confidence: crate::types::Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    #[tokio::test]
    async fn spawn_embed_task_runs_inline_when_admitted_spawn_times_out() {
        let store = SqliteStore::in_memory().unwrap();
        let background_tasks = BackgroundTasks::new();
        let orchestrator = EmbeddingOrchestrator::new(store.clone(), Arc::new(FixedSizeProvider), Arc::clone(&background_tasks));
        let admission = background_tasks.begin_embed_admission().unwrap();

        background_tasks.shutdown(Duration::from_millis(10)).await;

        let memory = Memory {
            id: MemoryId::new(),
            content: "late inline embed".into(),
            tags: vec![],
            provenance: Provenance::default(),
            access_policy: AccessPolicy::Public,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::default(),
            confidence: crate::types::Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        };
        let id = store.store(&memory, None).await.unwrap();

        let _queued = orchestrator.spawn_embed_task_or_run_inline(&admission, id, memory.content.clone(), 0).await;

        let after = store.get(&id, None).await.unwrap().unwrap();
        assert!(after.has_embedding, "inline fallback should preserve the store-then-embed invariant");
    }

    #[tokio::test]
    async fn duplicate_embed_for_same_revision_is_coalesced() {
        let store = SqliteStore::in_memory().unwrap();
        let background_tasks = BackgroundTasks::new();
        let provider = StdArc::new(BlockingProvider::new());
        let provider_for_orchestrator = StdArc::clone(&provider);
        let orchestrator = EmbeddingOrchestrator::new(store.clone(), provider_for_orchestrator, StdArc::clone(&background_tasks));
        let admission = background_tasks.begin_embed_admission().unwrap();

        let memory = Memory {
            id: MemoryId::new(),
            content: "coalesce embed".into(),
            tags: vec![],
            provenance: Provenance::default(),
            access_policy: AccessPolicy::Public,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::default(),
            confidence: crate::types::Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        };
        let id = store.store(&memory, None).await.unwrap();

        let started = provider.started.notified();
        tokio::pin!(started);
        let _already_registered = started.as_mut().enable();
        assert!(orchestrator.spawn_embed_task_or_run_inline(&admission, id, memory.content.clone(), 0).await);
        started.await;

        assert!(!orchestrator.spawn_embed_task_or_run_inline(&admission, id, memory.content.clone(), 0).await);

        provider.release.notify_waiters();
        background_tasks.shutdown(Duration::from_secs(1)).await;

        assert_eq!(provider.embed_calls.load(Ordering::Relaxed), 1);
        let after = store.get(&id, None).await.unwrap().unwrap();
        assert!(after.has_embedding);
    }

    #[tokio::test]
    async fn duplicate_claimed_embed_releases_claim_and_reports_not_queued() {
        let store = SqliteStore::in_memory().unwrap();
        let background_tasks = BackgroundTasks::new();
        let provider = StdArc::new(BlockingProvider::new());
        let provider_for_orchestrator = StdArc::clone(&provider);
        let orchestrator = EmbeddingOrchestrator::new(store.clone(), provider_for_orchestrator, StdArc::clone(&background_tasks));
        let admission = background_tasks.begin_embed_admission().unwrap();

        let memory = test_memory("duplicate claimed embed");
        let id = store.store(&memory, None).await.unwrap();

        let started = provider.started.notified();
        tokio::pin!(started);
        let _already_registered = started.as_mut().enable();
        assert!(orchestrator.spawn_embed_task_or_run_inline(&admission, id, memory.content.clone(), 0).await);
        started.await;

        let claim = store.claim_for_reembed(1).await.unwrap().pop().unwrap();
        let original_token = claim.claim_token.clone();
        assert!(!orchestrator.spawn_claimed_embed_task_or_run_inline(&admission, claim).await);

        let available = store.claim_for_reembed(1).await.unwrap();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].id, id);
        assert_ne!(available[0].claim_token, original_token);

        provider.release.notify_waiters();
        drop(admission);
        background_tasks.shutdown(Duration::from_secs(1)).await;

        assert_eq!(provider.embed_calls.load(Ordering::Relaxed), 1);
        let after = store.get(&id, None).await.unwrap().unwrap();
        assert!(after.has_embedding);
    }

    #[tokio::test]
    async fn shutdown_timeout_releases_active_claimed_embed() {
        let store = SqliteStore::in_memory().unwrap();
        let background_tasks = BackgroundTasks::new();
        let provider = StdArc::new(BlockingProvider::new());
        let provider_for_orchestrator = StdArc::clone(&provider);
        let orchestrator = EmbeddingOrchestrator::new(store.clone(), provider_for_orchestrator, StdArc::clone(&background_tasks));
        let admission = background_tasks.begin_embed_admission().unwrap();

        let memory = test_memory("shutdown claimed embed");
        let id = store.store(&memory, None).await.unwrap();
        let claim = store.claim_for_reembed(1).await.unwrap().pop().unwrap();
        let original_token = claim.claim_token.clone();

        let started = provider.started.notified();
        tokio::pin!(started);
        let _already_registered = started.as_mut().enable();
        assert!(orchestrator.spawn_claimed_embed_task_or_run_inline(&admission, claim).await);
        started.await;
        drop(admission);

        orchestrator.shutdown(Duration::from_millis(10)).await;

        let available = store.claim_for_reembed(1).await.unwrap();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].id, id);
        assert_ne!(available[0].claim_token, original_token);
    }
}
