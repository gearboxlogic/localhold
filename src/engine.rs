//! Business logic layer — store operations, embedding orchestration, and task tracking.
//!
//! [`LocalHoldEngine`] owns an [`EmbeddingOrchestrator`] (store + embedding + tasks),
//! a clock, and operational limits. The MCP server delegates all domain operations
//! here and converts the returned `Result<T, EngineError>` into wire-protocol
//! responses.
//!
//! Validation utilities live in [`crate::validation`], consolidation logic in
//! [`crate::consolidation`], and composite scoring in [`crate::scoring`].

use std::sync::Arc;

use tracing::warn;

// Re-export consolidation types for crate-internal consumers.
pub(crate) use crate::consolidation::{ConsolidateResult, DuplicateGroup};
use crate::{
    background_tasks::{BackgroundTaskKind, BackgroundTasks},
    clock::{Clock, SystemClock},
    config::{LimitsConfig, SearchConfig},
    consolidation::{NeighborPair, cosine_to_l2_threshold, find_duplicate_groups_from_pairs, l2_to_cosine},
    embedding::{EmbeddingProvider, limited::ConcurrencyLimitedEmbedding, orchestrator::EmbeddingOrchestrator},
    error::{EngineError, ValidationError},
    scoring::{apply_composite_scoring, seed_retrieval_scores},
    store::{MemoryStore, RecordUseOutcome},
    types::{
        AccessPolicy, AuditAction, AuditDraft, AuditEntry, AuthorizedUpdateOutcome, Confidence, Entity, Importance, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryStats,
        MemoryTombstone, MemoryType, MemoryUpdate, MetadataMigrationOutcome, MetadataMigrationReport, MetadataPatch, Provenance, QueryContext, RedactableField, ScopeDefinition,
        SearchMode, SearchResult, WriteOutcome,
    },
    validation::{
        normalize_entities, normalize_optional_non_empty, ttl_seconds_to_expiry, validate_batch_len, validate_content_length, validate_max_distance, validate_non_blank,
        validate_optional_string_array, validate_string_array, validate_tags,
    },
};

// ---------------------------------------------------------------------------
// Domain types returned/accepted by engine methods
// ---------------------------------------------------------------------------

/// Result of a search operation, carrying both the matches and the search strategy used.
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct SearchOutcome {
    /// Ranked list of matching memories.
    pub results: Vec<SearchResult>,
    /// Which search strategy was used.
    pub search_mode: SearchMode,
    /// Per-result RRF scores and match sources (populated only for hybrid search).
    /// Currently write-only — reserved for future diagnostic/tracing use.
    #[expect(dead_code, reason = "fusion metadata is populated for future diagnostic/tracing use")]
    pub fusion_metadata: Vec<FusionMetadata>,
}

impl SearchOutcome {
    /// Create a `SearchOutcome` for a single retrieval path (no fusion metadata).
    #[must_use]
    #[expect(clippy::missing_const_for_fn, reason = "Vec::new() is not const-stable")]
    fn single_path(results: Vec<SearchResult>, search_mode: SearchMode) -> Self {
        Self {
            results,
            search_mode,
            fusion_metadata: Vec::new(),
        }
    }
}

/// Per-result metadata from hybrid search fusion.
///
/// Currently write-only — reserved for future diagnostic/tracing use.
#[derive(Debug, Clone)]
#[non_exhaustive]
#[expect(dead_code, reason = "fusion metadata is populated for future diagnostic/tracing use")]
pub(crate) struct FusionMetadata {
    /// Which retrieval paths contributed to this result.
    pub match_sources: Vec<SearchMode>,
    /// RRF fusion score (higher = more relevant).
    pub rrf_score: f64,
}

#[derive(Debug, Clone, Copy)]
struct CandidatePoolLimits {
    semantic: usize,
    keyword: usize,
    text: usize,
    fused: usize,
}

enum HybridPathCandidates {
    Single(SearchOutcome),
    Both { semantic: Vec<SearchResult>, keyword: Vec<SearchResult> },
}

fn hybrid_path_candidates(strict: bool, semantic: Vec<SearchResult>, keyword: Vec<SearchResult>) -> HybridPathCandidates {
    if strict {
        return HybridPathCandidates::Both { semantic, keyword };
    }

    match (semantic.is_empty(), keyword.is_empty()) {
        (false, true) => HybridPathCandidates::Single(SearchOutcome::single_path(semantic, SearchMode::Semantic)),
        (true, false) => HybridPathCandidates::Single(SearchOutcome::single_path(keyword, SearchMode::Keyword)),
        _ => HybridPathCandidates::Both { semantic, keyword },
    }
}

/// Request payload for [`LocalHoldEngine::search_memories`], keeping the arg count under 5.
#[derive(Debug)]
pub(crate) struct SearchRequest {
    /// The raw query text.
    pub query: String,
    /// Maximum number of results.
    pub limit: usize,
    /// Filter criteria.
    pub filter: MemoryFilter,
    /// Caller context for access policy enforcement.
    pub ctx: QueryContext,
    /// Optional L2 distance threshold (semantic search only).
    pub max_distance: Option<f64>,
    /// Optional explicit keywords for FTS5 (overrides `query` for the keyword path).
    pub keywords: Option<String>,
    /// Requested search mode. `None` means "use the config default".
    pub search_mode: Option<SearchMode>,
    /// Optional conversational context to improve search quality.
    /// Appended to the query for embedding and used to generate optional FTS5 terms.
    pub context: Option<String>,
}

/// What to re-embed: a single memory or a bulk batch.
#[derive(Debug)]
#[non_exhaustive]
#[expect(variant_size_differences, reason = "Single carries an id + principal; boxing adds indirection for a two-variant enum")]
pub enum ReembedRequest {
    /// Re-embed a single memory by ID, authorized by `principal`.
    Single {
        /// The memory to re-embed.
        id: MemoryId,
        /// Server-resolved principal for write-access check.
        principal: String,
    },
    /// Re-embed up to `limit` memories that lack embeddings.
    Bulk {
        /// Maximum number of memories to queue.
        limit: usize,
    },
}

/// Input fields for building a new memory via [`LocalHoldEngine::build_memory`].
///
/// Keeps the argument count under the `too-many-arguments=5` threshold.
#[derive(Debug)]
pub(crate) struct StoreMemoryInput {
    /// The content to store.
    pub content: String,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Agent that created this memory.
    pub source_agent: Option<String>,
    /// User on whose behalf this memory was stored.
    pub source_user: Option<String>,
    /// Conversation scope for filtering.
    pub source_conversation: Option<String>,
    /// Original conversation (preserved across reassignments).
    pub origin_conversation: Option<String>,
    /// Visibility policy.
    pub access_policy: Option<AccessPolicy>,
    /// Time-to-live in seconds.
    pub ttl_seconds: Option<u64>,
    /// Classification of the memory content.
    pub memory_type: Option<MemoryType>,
    /// Importance score in `[0.0, 1.0]`.
    pub importance: Option<f64>,
    /// Confidence score in `[0.0, 1.0]`.
    pub confidence: Option<f64>,
    /// ID of the memory this new memory supersedes (soft versioning).
    pub supersedes: Option<MemoryId>,
    /// Typed entities to attach to this memory.
    pub entities: Vec<Entity>,
}

/// Bulk update payload — fields that can be updated in bulk (no content, which would need re-embedding).
#[derive(Debug, Clone, Default)]
pub(crate) struct BulkUpdateFields {
    /// New tag set to replace existing tags.
    pub tags: Option<Vec<String>>,
    /// New importance score in the range `[0.0, 1.0]`.
    pub importance: Option<Importance>,
    /// New access policy to replace the existing one.
    pub access_policy: Option<AccessPolicy>,
}

/// Result of a bulk delete operation.
#[derive(Debug)]
pub(crate) struct BulkDeleteResult {
    /// Number of memories successfully deleted.
    pub deleted: u64,
    /// Total memories that matched the filter (before access checks).
    pub matched: u64,
    /// Whether results were capped by `max_list_limit` (more may remain).
    pub capped: bool,
}

/// Result of a bulk update operation.
#[derive(Debug)]
pub(crate) struct BulkUpdateResult {
    /// Number of memories successfully updated.
    pub updated: u64,
    /// Number of memories denied due to access policy.
    pub denied: u64,
    /// Total memories that matched the filter (before access checks).
    pub matched: u64,
    /// Whether results were capped by `max_list_limit` (more may remain).
    pub capped: bool,
}

// ---------------------------------------------------------------------------
// LocalHoldEngine
// ---------------------------------------------------------------------------

/// Core business-logic layer for `LocalHold`.
///
/// Owns an [`EmbeddingOrchestrator`] (store + embedding + background tasks),
/// a clock, and operational limits.
///
/// Generic over the store backend `S`, which must implement the full
/// [`MemoryStore`] trait.
#[derive(Clone)]
pub struct LocalHoldEngine<S: MemoryStore + Clone + std::fmt::Debug + 'static> {
    orchestrator: EmbeddingOrchestrator<S>,
    clock: Arc<dyn Clock>,
    limits: LimitsConfig,
    search_config: SearchConfig,
    reranker: Option<Arc<dyn crate::reranker::RerankerProvider>>,
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> std::fmt::Debug for LocalHoldEngine<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHoldEngine").field("orchestrator", &self.orchestrator).finish_non_exhaustive()
    }
}

#[expect(clippy::multiple_inherent_impl, reason = "separate constructors/accessors from the large operation impl for readability")]
impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> LocalHoldEngine<S> {
    const MAX_CONSOLIDATION_FETCH: usize = 1_000_000;

    // -- constructors -------------------------------------------------------

    /// Create a new engine with the given store, embedding provider, and operational limits.
    #[must_use]
    pub fn new(store: S, embedding: Arc<dyn EmbeddingProvider>, limits: LimitsConfig, search_config: SearchConfig) -> Self {
        Self::new_with_clock(store, embedding, limits, search_config, Arc::new(SystemClock::new()))
    }

    /// Create a new engine with a custom clock (for testing).
    #[must_use]
    pub fn new_with_clock(store: S, embedding: Arc<dyn EmbeddingProvider>, limits: LimitsConfig, search_config: SearchConfig, clock: Arc<dyn Clock>) -> Self {
        let background_tasks = BackgroundTasks::new_with_clock(Arc::clone(&clock));
        let max_concurrent_embedding_requests = limits.max_concurrent_embedding_requests;
        let embedding_batch_size = limits.embedding_batch_size;
        let embedding: Arc<dyn EmbeddingProvider> = Arc::new(ConcurrencyLimitedEmbedding::new(embedding, max_concurrent_embedding_requests));
        Self {
            orchestrator: EmbeddingOrchestrator::new(store, embedding, background_tasks, embedding_batch_size),
            clock,
            limits,
            search_config,
            reranker: None,
        }
    }

    /// Attach a cross-encoder reranker to the engine.
    ///
    /// When set, hybrid search results are reranked using the cross-encoder
    /// before composite scoring. When `None` (default), `Q(d) = H(d)`.
    #[must_use]
    pub fn with_reranker(mut self, reranker: Arc<dyn crate::reranker::RerankerProvider>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    // -- accessors ----------------------------------------------------------

    /// Current wall-clock time from the injected clock.
    #[must_use]
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    /// Operational limits.
    #[must_use]
    pub const fn limits(&self) -> &LimitsConfig {
        &self.limits
    }

    /// Search configuration (for entity-expansion scoring in the server layer).
    #[must_use]
    pub const fn search_config(&self) -> &SearchConfig {
        &self.search_config
    }

    /// Borrow the underlying store (needed by server for legacy-row seeding in tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn store(&self) -> &S {
        self.orchestrator.store()
    }

    // -- task management ----------------------------------------------------

    /// Return the number of in-flight background tasks.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn tracked_task_count(&self) -> usize {
        self.orchestrator.background_tasks().tracked_task_count()
    }

    /// Drain completed tasks and return how many were reaped.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn reap_completed_tasks_for_test(&self) -> usize {
        self.orchestrator.background_tasks().reap_completed_tasks_for_test()
    }

    /// Spawn an arbitrary tracked task (for testing task management).
    #[cfg(test)]
    fn spawn_tracked_task(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.orchestrator.background_tasks().spawn_for_test(future);
    }

    /// Drain all in-flight background tasks (embedding generation).
    /// Times out after [`LimitsConfig::shutdown_timeout_secs`] to prevent
    /// indefinite hangs on unresponsive providers.
    pub async fn shutdown(&self) {
        self.orchestrator.shutdown(std::time::Duration::from_secs(self.limits.shutdown_timeout_secs)).await;
    }

    /// Shut down with a custom timeout (for tests).
    #[cfg(any(test, feature = "testing"))]
    pub async fn shutdown_for_test(&self, timeout: std::time::Duration) {
        self.orchestrator.shutdown(timeout).await;
    }

    // -- audit helper --------------------------------------------------------

    fn audit_draft(&self, action: AuditAction, caller: Option<String>, details: Option<serde_json::Value>) -> AuditDraft {
        AuditDraft {
            action,
            caller_agent: caller,
            timestamp: self.clock.now(),
            details,
        }
    }

    // -- engine methods -----------------------------------------------------
}

impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> LocalHoldEngine<S> {
    /// Build a validated, normalized [`Memory`] from raw input fields.
    ///
    /// Validates content (non-blank, length), tags (count, length, no blanks),
    /// normalizes string fields, and computes TTL-based expiry.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if any input constraint is violated.
    pub(crate) fn build_memory(&self, input: StoreMemoryInput, now: chrono::DateTime<chrono::Utc>) -> Result<Memory, EngineError> {
        validate_non_blank("content", &input.content)?;
        validate_content_length(&input.content, self.limits.max_content_length)?;
        let tags: Vec<String> = input.tags.into_iter().map(|t| t.trim().to_owned()).collect();
        validate_string_array("tags", &tags)?;
        validate_tags("tags", &tags, self.limits.max_tags_per_memory, self.limits.max_tag_length)?;
        crate::validation::validate_entities_with_limits(&input.entities, self.limits.max_entities_per_memory, self.limits.max_entity_field_length)?;
        let source_agent = normalize_optional_non_empty("source_agent", input.source_agent)?;
        let source_user = normalize_optional_non_empty("source_user", input.source_user)?;
        let source_conversation = normalize_optional_non_empty("source_conversation", input.source_conversation)?;
        let origin_conversation = normalize_optional_non_empty("origin_conversation", input.origin_conversation)?;
        let access_policy = input.access_policy.unwrap_or_default();
        let expires_at = input.ttl_seconds.map(|ttl| ttl_seconds_to_expiry(ttl, now)).transpose()?;
        let origin_conversation = origin_conversation.or_else(|| source_conversation.clone());
        let importance = Importance::new(input.importance.unwrap_or(0.5_f64));
        let confidence = Confidence::new(input.confidence.unwrap_or(0.8_f64));
        let entities = normalize_entities(input.entities);

        Ok(Memory {
            id: MemoryId::new(),
            content: input.content,
            tags,
            provenance: Provenance {
                source_agent,
                source_conversation,
                origin_conversation,
                source_user,
            },
            access_policy,
            created_at: now,
            updated_at: now,
            expires_at,
            has_embedding: false,
            memory_type: input.memory_type.unwrap_or_default(),
            importance,
            confidence,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities,
            was_redacted: false,
        })
    }

    /// Store a memory and spawn a background embedding task.
    ///
    /// When `supersedes` is provided, the referenced memory's `superseded_by`
    /// field is atomically set to the new memory's ID.
    ///
    /// Delegates to the [`EmbeddingOrchestrator`] which enforces the
    /// store-then-embed invariant.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` if the persistence layer rejects the write
    /// or if the superseded memory does not exist.
    pub async fn store_memory(&self, memory: Memory, supersedes: Option<&MemoryId>) -> Result<MemoryId, EngineError> {
        let principal = memory.provenance.source_agent.clone();
        let embed_admission = self.orchestrator.begin_embed_admission()?;
        let details = supersedes.map(|s| serde_json::json!({"supersedes": s.to_string()}));
        let audit = self.audit_draft(AuditAction::Store, principal, details);
        let id = self.orchestrator.store_and_embed(&embed_admission, memory, supersedes, &audit).await?;

        Ok(id)
    }

    /// Store a memory and required metadata atomically, then spawn embedding work.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` if the persistence layer rejects the write
    /// or `EngineError::ShuttingDown` if embedding work cannot be admitted.
    pub async fn store_memory_with_metadata(&self, memory: Memory, supersedes: Option<&MemoryId>, metadata: &MemoryMetadata) -> Result<MemoryId, EngineError> {
        let principal = memory.provenance.source_agent.clone();
        let embed_admission = self.orchestrator.begin_embed_admission()?;
        let details = supersedes.map(|s| serde_json::json!({"supersedes": s.to_string()}));
        let audit = self.audit_draft(AuditAction::Store, principal, details);
        let id = self
            .orchestrator
            .store_and_embed_with_metadata(&embed_admission, memory, supersedes, metadata, &audit)
            .await?;

        Ok(id)
    }

    /// Search memories using the requested search mode, with automatic hybrid
    /// (semantic + FTS5 keyword search fused via RRF) as the default.
    ///
    /// Validates query non-blank, `max_distance` finite+non-negative, and clamps
    /// limit to `self.limits.max_search_limit`.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if the query is blank or `max_distance` is invalid,
    /// or `EngineError::Store` if the underlying search query fails.
    pub(crate) async fn search_memories(&self, request: SearchRequest) -> Result<SearchOutcome, EngineError> {
        self.search_memories_inner(request, true).await
    }

    /// Search without recording analytics impressions.
    ///
    /// This is reserved for read-only inspection surfaces such as the TUI.
    pub(crate) async fn search_memories_read_only(&self, request: SearchRequest) -> Result<SearchOutcome, EngineError> {
        self.search_memories_inner(request, false).await
    }

    async fn search_memories_inner(&self, mut request: SearchRequest, record_impressions: bool) -> Result<SearchOutcome, EngineError> {
        validate_non_blank("query", &request.query)?;
        validate_max_distance(request.max_distance)?;
        request.limit = request.limit.min(self.limits.max_search_limit);
        let final_limit = request.limit;
        let search_mode = request.search_mode.unwrap_or(self.search_config.default_mode);
        if final_limit == 0 {
            return Ok(SearchOutcome::single_path(Vec::new(), concrete_zero_limit_search_mode(search_mode)));
        }

        // Save the raw query for the cross-encoder reranker (context-enriched
        // text is only useful for first-stage embeddings, not CE scoring).
        let raw_query = request.query.clone();

        // Extract context for enhanced search, taking ownership so request can be moved.
        let context = request.context.take();
        let embed_query = match &context {
            Some(ctx) if !ctx.trim().is_empty() => format!("{} {ctx}", request.query),
            // Clone is needed here: embed_query is consumed by embedding.embed(),
            // while request.query is consumed by text/FTS fallback paths below.
            _ => request.query.clone(),
        };
        let fts_context = context.as_deref().filter(|c| !c.trim().is_empty());

        let store = self.orchestrator.store();
        let embedding = self.orchestrator.embedding();
        let candidate_limits = self.candidate_pool_limits(final_limit);
        let mut outcome = match search_mode {
            SearchMode::Semantic => {
                let query_emb = embedding
                    .embed(&embed_query)
                    .await
                    .map_err(|e| EngineError::SearchUnavailable(format!("semantic search requires embedding support: {e}")))?;
                let results = store
                    .search_by_embedding(&query_emb, candidate_limits.semantic, &request.filter, &request.ctx, request.max_distance)
                    .await?;
                SearchOutcome::single_path(results, SearchMode::Semantic)
            }
            SearchMode::Keyword => {
                if !store.fts_available() {
                    return Err(EngineError::SearchUnavailable("keyword search requires FTS5 support".into()));
                }
                let fts_query = request.keywords.as_deref().unwrap_or(&request.query);
                let results = store.search_by_fts(fts_query, candidate_limits.keyword, &request.filter, &request.ctx, fts_context).await?;
                SearchOutcome::single_path(results, SearchMode::Keyword)
            }
            SearchMode::Text => {
                let results = store.search_by_text(&request.query, candidate_limits.text, &request.filter, &request.ctx).await?;
                SearchOutcome::single_path(results, SearchMode::Text)
            }
            SearchMode::Auto => self.search_hybrid_with_context(request, &embed_query, fts_context, store, embedding, false).await?,
            SearchMode::Hybrid => self.search_hybrid_with_context(request, &embed_query, fts_context, store, embedding, true).await?,
        };

        // Seed first-stage retrieval scores for all search modes so reranking and
        // final scoring operate on an explicit H(d) signal rather than implicit
        // rank fallbacks.
        seed_retrieval_scores(&mut outcome.results);

        // Apply cross-encoder reranking over top-M candidates (if available).
        if let Some(reranker) = &self.reranker {
            self.apply_reranker(&mut outcome.results, &raw_query, reranker).await;
        }

        // Apply composite scoring / reranking
        let now = self.clock.now();
        apply_composite_scoring(&mut outcome.results, now, &self.search_config);

        // Optional duplicate suppression: penalize near-duplicate results.
        if self.search_config.duplicate_suppression.enabled {
            let result_ids: Vec<MemoryId> = outcome.results.iter().map(|r| r.memory.id).collect();
            if let Ok(embeddings) = store.fetch_embeddings_for_ids(&result_ids).await {
                crate::scoring::apply_duplicate_suppression(&mut outcome.results, &embeddings, self.search_config.duplicate_suppression.lambda, now);
            }
        }

        if outcome.results.len() > final_limit {
            outcome.results.truncate(final_limit);
        }

        // Fire-and-forget impression tracking — do not block the search response.
        // Impressions update impression_count + last_impressed_at for analytics only;
        // they do NOT feed into ranking signals (activity tracking is separate).
        if record_impressions {
            self.record_search_impression(
                outcome.results.iter().map(|r| r.memory.id).collect(),
                BackgroundTaskKind::AccessTracking,
                "failed to record search impression",
            )
            .await;
        }

        Ok(outcome)
    }

    /// Record search impressions, using background work during normal
    /// operation and an inline fallback once shutdown has started.
    async fn record_search_impression(&self, ids: Vec<MemoryId>, kind: BackgroundTaskKind, failure_message: &'static str) {
        if ids.is_empty() {
            return;
        }
        let store = self.orchestrator.store().clone();
        let spawn_ids = ids.clone();
        let spawned = self.orchestrator.background_tasks().spawn_best_effort(kind, async move {
            if let Err(e) = store.record_search_impression(&spawn_ids).await {
                tracing::warn!("{failure_message}: {e}");
            }
        });
        if !spawned {
            let store = self.orchestrator.store().clone();
            if let Err(e) = store.record_search_impression(&ids).await {
                tracing::warn!("{failure_message}: {e}");
            }
        }
    }

    /// Apply cross-encoder reranking to the top-M results in the pool.
    ///
    /// On failure, logs a warning and leaves the first-stage retrieval scores
    /// unchanged so final scoring falls back to H(d) alone.
    async fn apply_reranker(&self, results: &mut [SearchResult], query: &str, reranker: &Arc<dyn crate::reranker::RerankerProvider>) {
        let pool_size = self.search_config.rerank_top_m.min(results.len());
        if pool_size == 0 {
            return;
        }

        // Collect up to pool_size eligible results from the full slice. Hidden
        // redacted content must not be sent to the reranker, but redacted views
        // that explicitly expose content can participate normally.
        let eligible_indices: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.memory.field_visible_in_view(&RedactableField::Content))
            .map(|(i, _)| i)
            .take(pool_size)
            .collect();
        if eligible_indices.is_empty() {
            return;
        }
        let documents: Vec<&str> = eligible_indices.iter().map(|&i| results[i].memory.content.as_str()).collect();

        let scores = match reranker.rerank(query, &documents).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("reranker unavailable, falling back to retrieval score: {e}");
                return;
            }
        };
        for s in &scores {
            if s.index >= eligible_indices.len() {
                continue;
            }
            results[eligible_indices[s.index]].reranker_score = Some(s.score);
        }
    }

    /// Record a real use event for the given memories, updating their decayed
    /// activity mass and `last_used_at` timestamp.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` if the persistence layer rejects the write.
    pub(crate) async fn record_memory_use(&self, ids: Vec<MemoryId>, principal: &str, event_weight: f64) -> Result<RecordUseOutcome, EngineError> {
        if ids.is_empty() {
            return Ok(RecordUseOutcome::default());
        }
        let now = self.clock.now();
        let half_life = self.search_config.activity_half_life_hours;
        let store = self.orchestrator.store().clone();
        Ok(store.record_memory_use(&ids, principal, event_weight, now, half_life).await?)
    }

    /// Hybrid search with context enhancement: attempt both semantic + FTS5 paths,
    /// fuse with RRF. Falls back gracefully when either path is unavailable.
    #[expect(
        clippy::too_many_arguments,
        reason = "hybrid search requires request, embed query, context, store, and provider — all semantically distinct"
    )]
    #[expect(clippy::float_arithmetic, reason = "RRF score normalization requires floating-point division")]
    async fn search_hybrid_with_context(
        &self,
        request: SearchRequest,
        embed_query: &str,
        fts_context: Option<&str>,
        store: &S,
        embedding: &Arc<dyn EmbeddingProvider>,
        strict: bool,
    ) -> Result<SearchOutcome, EngineError> {
        let embed_result = embedding.embed(embed_query).await;
        let fts_available = store.fts_available();
        let candidate_limits = self.candidate_pool_limits(request.limit);

        match (embed_result, fts_available) {
            (Ok(query_emb), true) => {
                // Both available: run both, fuse with RRF.
                let fts_query = request.keywords.as_deref().unwrap_or(&request.query);

                // Ensure each leg fetches enough candidates to fill the
                // downstream rerank/diversity pool.
                let semantic_results = store
                    .search_by_embedding(&query_emb, candidate_limits.semantic, &request.filter, &request.ctx, request.max_distance)
                    .await?;
                let fts_results = store.search_by_fts(fts_query, candidate_limits.keyword, &request.filter, &request.ctx, fts_context).await?;
                let (semantic_results, fts_results) = match hybrid_path_candidates(strict, semantic_results, fts_results) {
                    HybridPathCandidates::Single(outcome) => return Ok(outcome),
                    HybridPathCandidates::Both { semantic, keyword } => (semantic, keyword),
                };

                let fused = crate::fusion::reciprocal_rank_fusion(
                    semantic_results,
                    fts_results,
                    self.search_config.rrf_k,
                    self.search_config.rrf_semantic_weight,
                    self.search_config.rrf_keyword_weight,
                    candidate_limits.fused,
                );

                let fusion_metadata: Vec<FusionMetadata> = fused
                    .iter()
                    .map(|f| FusionMetadata {
                        match_sources: f.match_sources.clone(),
                        rrf_score: f.rrf_score,
                    })
                    .collect();

                // Normalize RRF scores to [0,1] and carry them into SearchResult
                // so composite scoring uses actual fused relevance instead of a
                // rank-based fallback.
                let max_rrf = fused.iter().map(|f| f.rrf_score).fold(0.0_f64, f64::max);
                let results: Vec<SearchResult> = fused
                    .into_iter()
                    .map(|f| {
                        let mut r = f.result;
                        r.retrieval_score = Some(f.rrf_score / (max_rrf + 1e-9_f64));
                        r
                    })
                    .collect();

                Ok(SearchOutcome {
                    results,
                    search_mode: SearchMode::Hybrid,
                    fusion_metadata,
                })
            }
            (Ok(query_emb), false) => {
                if strict {
                    return Err(EngineError::SearchUnavailable("hybrid search requires FTS5 keyword search support".into()));
                }
                // Embedding only (FTS5 unavailable).
                let results = store
                    .search_by_embedding(&query_emb, candidate_limits.semantic, &request.filter, &request.ctx, request.max_distance)
                    .await?;
                Ok(SearchOutcome::single_path(results, SearchMode::Semantic))
            }
            (Err(_), true) => {
                if strict {
                    return Err(EngineError::SearchUnavailable("hybrid search requires embedding support".into()));
                }
                // FTS5 only (embedding unavailable).
                let fts_query = request.keywords.as_deref().unwrap_or(&request.query);
                let results = store.search_by_fts(fts_query, candidate_limits.keyword, &request.filter, &request.ctx, fts_context).await?;
                Ok(SearchOutcome::single_path(results, SearchMode::Keyword))
            }
            (Err(e), false) => {
                if strict {
                    return Err(EngineError::SearchUnavailable(format!(
                        "hybrid search requires both embedding and FTS5 support: embedding error: {e}"
                    )));
                }
                // Both unavailable, fall back to LIKE.
                warn!("embedding + FTS5 unavailable, falling back to text search: {e}");
                let results = store.search_by_text(&request.query, candidate_limits.text, &request.filter, &request.ctx).await?;
                Ok(SearchOutcome::single_path(results, SearchMode::Text))
            }
        }
    }

    fn candidate_pool_limits(&self, final_limit: usize) -> CandidatePoolLimits {
        let base = self.base_candidate_pool_limit(final_limit);
        let max_pool = self.limits.max_candidate_pool_size;
        let semantic = base.max(self.search_config.semantic_candidate_k).min(max_pool);
        let keyword = base.max(self.search_config.keyword_candidate_k).min(max_pool);
        CandidatePoolLimits {
            semantic,
            keyword,
            text: keyword,
            fused: semantic.saturating_add(keyword).min(max_pool),
        }
    }

    fn base_candidate_pool_limit(&self, user_limit: usize) -> usize {
        if user_limit == 0 {
            return 0;
        }
        let mut pool = user_limit;
        if self.reranker.is_some() {
            pool = pool.max(self.search_config.rerank_top_m);
        }
        // When duplicate suppression is enabled, overfetch so that dropping
        // near-duplicates doesn't leave the final result set short.
        if self.search_config.duplicate_suppression.enabled {
            pool = pool.saturating_mul(2);
        }
        pool.min(self.limits.max_candidate_pool_size)
    }

    /// List memories matching the given filter and context.
    ///
    /// Clamps `filter.limit` to `self.limits.max_list_limit`.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn list_memories(&self, mut filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<Memory>, EngineError> {
        filter.limit = filter.limit.map(|l| l.min(self.limits.max_list_limit));
        Ok(self.orchestrator.store().list(filter, ctx).await?)
    }

    /// Retrieve a specific memory by ID, applying access policy.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn get_memory(&self, id: &MemoryId, caller: Option<&str>) -> Result<Option<Memory>, EngineError> {
        Ok(self.orchestrator.store().get(id, caller).await?)
    }

    /// Update a memory with authorization check, spawning a re-embed task when content changes.
    ///
    /// Validates content (non-blank, length) and tags (count, length, no blanks) before
    /// delegating to the store.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` on invalid content/tags,
    /// or `EngineError::Store` on persistence-layer failure.
    pub async fn update_memory(&self, id: MemoryId, mut update: MemoryUpdate, principal: &str) -> Result<AuthorizedUpdateOutcome, EngineError> {
        let embed_admission = update.content.as_ref().map(|_| self.orchestrator.begin_embed_admission()).transpose()?;
        self.prepare_update(&mut update)?;
        let audit = self.audit_draft(AuditAction::Update, Some(principal.to_owned()), None);
        let outcome = self.orchestrator.update_and_maybe_reembed(embed_admission.as_ref(), id, &update, principal, &audit).await?;

        Ok(outcome)
    }

    /// Update a memory and optional metadata patch in one audited store
    /// transaction, spawning a re-embed task when content changes.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` on invalid content/tags/metadata,
    /// or `EngineError::Store` on persistence-layer failure.
    pub async fn update_memory_with_metadata(
        &self,
        id: MemoryId,
        mut update: MemoryUpdate,
        metadata_patch: Option<MetadataPatch>,
        principal: &str,
    ) -> Result<AuthorizedUpdateOutcome, EngineError> {
        let embed_admission = update.content.as_ref().map(|_| self.orchestrator.begin_embed_admission()).transpose()?;
        self.prepare_update(&mut update)?;
        let details = metadata_patch.is_some().then(|| serde_json::json!({"metadata": true}));
        let audit = self.audit_draft(AuditAction::Update, Some(principal.to_owned()), details);
        let outcome = self
            .orchestrator
            .update_with_metadata_and_maybe_reembed(embed_admission.as_ref(), id, &update, metadata_patch.as_ref(), principal, &audit)
            .await?;

        Ok(outcome)
    }

    /// Revise a memory loaded at `expected_updated_at`.
    ///
    /// Fields, metadata, and audit commit only after authorization and
    /// concurrency checks pass. Replacement content is then queued for
    /// background embedding, so rejected drafts are never sent to a provider.
    ///
    /// # Errors
    ///
    /// Returns validation, authorization/store, or concurrency errors without
    /// partially applying the persisted revision.
    #[expect(clippy::too_many_arguments, reason = "interactive revise needs identity, revision, fields, metadata, and principal")]
    pub async fn update_memory_if_unmodified_with_metadata(
        &self,
        id: MemoryId,
        expected_updated_at: chrono::DateTime<chrono::Utc>,
        mut update: MemoryUpdate,
        metadata_patch: Option<MetadataPatch>,
        principal: &str,
    ) -> Result<AuthorizedUpdateOutcome, EngineError> {
        let new_content = update.content.clone();
        let embed_admission = new_content.as_ref().map(|_| self.orchestrator.begin_embed_admission()).transpose()?;
        self.prepare_update(&mut update)?;
        let details = metadata_patch.is_some().then(|| serde_json::json!({"metadata": true, "interactive": true}));
        let audit = self.audit_draft(AuditAction::Update, Some(principal.to_owned()), details);
        let outcome = self
            .orchestrator
            .store()
            .update_authorized_if_unmodified_with_metadata_audited(&id, expected_updated_at, &update, metadata_patch.as_ref(), None, principal, &audit)
            .await?;
        if let (Some(content), Some(revision), Some(admission)) = (new_content, outcome.reembed_revision, embed_admission.as_ref()) {
            let _queued = self.orchestrator.spawn_embed_task_or_run_inline(admission, id, content, revision).await;
        }
        Ok(outcome)
    }

    /// Validate content, tags, and entities fields of an update payload.
    fn validate_update_fields(&self, update: &MemoryUpdate) -> Result<(), EngineError> {
        if let Some(content) = &update.content {
            validate_non_blank("content", content)?;
            validate_content_length(content, self.limits.max_content_length)?;
        }
        validate_optional_string_array("tags", update.tags.as_deref())?;
        if let Some(tags) = &update.tags {
            validate_tags("tags", tags, self.limits.max_tags_per_memory, self.limits.max_tag_length)?;
        }
        if let Some(entities) = &update.entities {
            crate::validation::validate_entities_with_limits(entities, self.limits.max_entities_per_memory, self.limits.max_entity_field_length)?;
        }
        crate::validation::validate_optional_non_empty("source_conversation", update.source_conversation.as_deref())?;
        Ok(())
    }

    /// Validate and normalize importance for an update, mutating in place.
    pub(crate) fn prepare_update(&self, update: &mut MemoryUpdate) -> Result<(), EngineError> {
        // Trim tags before validation so whitespace-padded tags are normalized.
        if let Some(tags) = update.tags.take() {
            update.tags = Some(tags.into_iter().map(|t| t.trim().to_owned()).collect());
        }
        if let Some(source_conversation) = update.source_conversation.take() {
            update.source_conversation = normalize_optional_non_empty("source_conversation", Some(source_conversation))?;
        }
        self.validate_update_fields(update)?;
        // Importance is already clamped at construction via Importance::new(),
        // so no additional clamping needed here.
        if let Some(entities) = update.entities.take() {
            update.entities = Some(normalize_entities(entities));
        }
        Ok(())
    }

    /// Delete a memory with authorization check.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn delete_memory(&self, id: &MemoryId, principal: &str) -> Result<WriteOutcome, EngineError> {
        let audit = self.audit_draft(AuditAction::Delete, Some(principal.to_owned()), None);
        let outcome = self.orchestrator.store().delete_authorized_audited(id, principal, &audit).await?;
        Ok(outcome)
    }

    /// Delete a memory only if its loaded content revision is still current.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence or concurrency failure.
    pub async fn delete_memory_if_unmodified(&self, id: &MemoryId, expected_updated_at: chrono::DateTime<chrono::Utc>, principal: &str) -> Result<WriteOutcome, EngineError> {
        let audit = self.audit_draft(AuditAction::Delete, Some(principal.to_owned()), Some(serde_json::json!({"interactive": true})));
        Ok(self
            .orchestrator
            .store()
            .delete_authorized_if_unmodified_audited(id, expected_updated_at, principal, &audit)
            .await?)
    }

    /// Reassign conversation scope for matching memories the caller may write.
    ///
    /// Validates that `from` and `to` are different.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if `from == to`,
    /// or `EngineError::Store` on persistence-layer failure.
    pub async fn reassign_scope(&self, from: &str, to: &str, origin: Option<&str>, principal: &str) -> Result<u64, EngineError> {
        if from == to {
            return Err(ValidationError::new("from_scope", "from_scope and to_scope must be different").into());
        }
        let details = serde_json::json!({
            "from_scope": from,
            "to_scope": to,
            "origin_conversation": origin,
        });
        let audit = self.audit_draft(AuditAction::Reassign, Some(principal.to_owned()), Some(details));
        let outcome = self.orchestrator.store().reassign_scope_audited(from, to, origin, principal, &audit).await?;
        let count = u64::try_from(outcome.applied_ids.len()).unwrap_or(u64::MAX);

        Ok(count)
    }

    /// Evict all expired memories.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn evict_expired(&self) -> Result<u64, EngineError> {
        Ok(self.orchestrator.store().evict_expired().await?)
    }

    /// Register or replace a scope definition.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn register_scope(&self, scope: ScopeDefinition) -> Result<(), EngineError> {
        Ok(self.orchestrator.store().register_scope(scope).await?)
    }

    /// List registered scope definitions.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn list_scopes(&self) -> Result<Vec<ScopeDefinition>, EngineError> {
        Ok(self.orchestrator.store().list_scopes().await?)
    }

    /// Upsert non-destructive metadata for an existing memory.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn upsert_metadata(&self, metadata: MemoryMetadata, principal: &str) -> Result<(), EngineError> {
        let audit = self.audit_draft(AuditAction::Update, Some(principal.to_owned()), Some(serde_json::json!({"metadata": true})));
        Ok(self.orchestrator.store().upsert_metadata_audited(metadata, &audit).await?)
    }

    /// Fetch non-destructive metadata for a memory.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn get_metadata(&self, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, EngineError> {
        Ok(self.orchestrator.store().get_metadata(memory_id).await?)
    }

    /// Return conservative metadata migration/reporting counts.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn metadata_migration_report(&self) -> Result<MetadataMigrationReport, EngineError> {
        Ok(self.orchestrator.store().metadata_migration_report().await?)
    }

    /// Add metadata rows for existing memories without rewriting original content.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` if the persistence layer rejects the migration pass.
    pub async fn migrate_metadata(&self, registered_scope_keys: &[String], dry_run: bool, principal: &str) -> Result<MetadataMigrationOutcome, EngineError> {
        let audit = self.audit_draft(AuditAction::Update, Some(principal.to_owned()), Some(serde_json::json!({"metadata_migration": true})));
        Ok(self.orchestrator.store().migrate_metadata_audited(registered_scope_keys, dry_run, &audit).await?)
    }

    /// Return aggregate statistics about stored memories.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub async fn count_memories(&self, filter: MemoryFilter, ctx: QueryContext, top_tags_limit: usize) -> Result<MemoryStats, EngineError> {
        let clamped = top_tags_limit.min(self.limits.max_top_tags_limit);
        Ok(self.orchestrator.store().count(filter, ctx, clamped).await?)
    }

    /// Store multiple memories atomically, spawning embed tasks for each.
    ///
    /// Delegates to the [`EmbeddingOrchestrator`] which enforces the
    /// store-then-embed invariant.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if the batch is empty or oversized,
    /// or `EngineError::Store` if the batch write fails.
    pub async fn batch_store(&self, memories: Vec<Memory>, supersedes_list: Vec<Option<MemoryId>>) -> Result<Vec<MemoryId>, EngineError> {
        if memories.len() != supersedes_list.len() {
            return Err(EngineError::Validation(ValidationError::new(
                "supersedes",
                format!("supersedes list length ({}) must match memories length ({})", supersedes_list.len(), memories.len()),
            )));
        }

        // Capture per-memory write principals before the Vec is moved into the orchestrator.
        let principals: Vec<Option<String>> = memories.iter().map(|m| m.provenance.source_agent.clone()).collect();
        let embed_admission = self.orchestrator.begin_embed_admission()?;
        let audits: Vec<AuditDraft> = principals
            .iter()
            .zip(supersedes_list.iter())
            .map(|(principal, sup)| {
                let details = sup.as_ref().map(|s| serde_json::json!({"supersedes": s.to_string()}));
                self.audit_draft(AuditAction::Store, principal.clone(), details)
            })
            .collect();

        let ids = self
            .orchestrator
            .batch_store_and_embed(&embed_admission, memories, &supersedes_list, &audits, self.limits.max_batch_size)
            .await?;

        Ok(ids)
    }

    /// Store multiple memories and required metadata atomically, spawning embed tasks for each.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if the batch inputs are inconsistent
    /// or oversized, `EngineError::Store` if the persistence layer rejects the
    /// write, or `EngineError::ShuttingDown` if embedding work cannot be admitted.
    pub async fn batch_store_with_metadata(
        &self,
        memories: Vec<Memory>,
        supersedes_list: Vec<Option<MemoryId>>,
        metadata: Vec<MemoryMetadata>,
    ) -> Result<Vec<MemoryId>, EngineError> {
        if memories.len() != supersedes_list.len() {
            return Err(EngineError::Validation(ValidationError::new(
                "supersedes",
                format!("supersedes list length ({}) must match memories length ({})", supersedes_list.len(), memories.len()),
            )));
        }
        if memories.len() != metadata.len() {
            return Err(EngineError::Validation(ValidationError::new(
                "metadata",
                format!("metadata length ({}) must match memories length ({})", metadata.len(), memories.len()),
            )));
        }

        let principals: Vec<Option<String>> = memories.iter().map(|m| m.provenance.source_agent.clone()).collect();
        let embed_admission = self.orchestrator.begin_embed_admission()?;
        let audits: Vec<AuditDraft> = principals
            .iter()
            .zip(supersedes_list.iter())
            .map(|(principal, sup)| {
                let details = sup.as_ref().map(|s| serde_json::json!({"supersedes": s.to_string()}));
                self.audit_draft(AuditAction::Store, principal.clone(), details)
            })
            .collect();

        let ids = self
            .orchestrator
            .batch_store_and_embed_with_metadata(&embed_admission, memories, &supersedes_list, &metadata, &audits, self.limits.max_batch_size)
            .await?;

        Ok(ids)
    }

    /// Re-embed one or more memories. Checks embedding provider health first.
    ///
    /// For bulk requests, validates limit within `self.limits.max_reembed_limit`.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Validation` if the bulk limit exceeds the cap,
    /// `EngineError::EmbeddingUnavailable` if the health check fails,
    /// or `EngineError::Store` on persistence-layer failure.
    pub async fn reembed(&self, request: ReembedRequest) -> Result<ReembedOutcome, EngineError> {
        if let ReembedRequest::Bulk { limit } = &request {
            validate_batch_len("limit", *limit, self.limits.max_reembed_limit)?;
        }

        self.orchestrator.embedding().health_check().await.map_err(EngineError::from)?;

        let store = self.orchestrator.store();
        match request {
            ReembedRequest::Single { id, principal } => {
                let Some((content, revision)) = store.get_for_reembed(&id, &principal).await? else {
                    return Ok(ReembedOutcome::NotFound(id));
                };
                let embed_admission = self.orchestrator.begin_embed_admission()?;
                let queued = self.orchestrator.spawn_embed_task_or_run_inline(&embed_admission, id, content, revision).await;
                Ok(ReembedOutcome::Queued(usize::from(queued)))
            }
            ReembedRequest::Bulk { limit } => {
                let embed_admission = self.orchestrator.begin_embed_admission()?;
                let claims = store.claim_for_reembed(limit).await?;
                let count = self.orchestrator.spawn_claimed_embed_batches_or_run_inline(&embed_admission, claims).await;
                Ok(ReembedOutcome::Queued(count))
            }
        }
    }

    /// Bulk-delete memories matching filter criteria, checking write access per memory.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub(crate) async fn bulk_delete(&self, principal: &str, mut filter: MemoryFilter, ctx: QueryContext) -> Result<BulkDeleteResult, EngineError> {
        filter.limit = Some(self.limits.max_list_limit);
        let store = self.orchestrator.store();
        let memories = store.list(filter, ctx).await?;
        let matched = memories.len();

        let all_ids: Vec<MemoryId> = memories.iter().map(|m| m.id).collect();
        let audit = self.audit_draft(AuditAction::BulkDelete, Some(principal.to_owned()), None);
        let outcome = store.bulk_delete_ids_audited(all_ids, principal, &audit).await?;

        let deleted = u64::try_from(outcome.applied_ids.len()).unwrap_or(u64::MAX);
        let matched_u64 = u64::try_from(matched).unwrap_or(u64::MAX);
        Ok(BulkDeleteResult {
            deleted,
            matched: matched_u64,
            capped: matched >= self.limits.max_list_limit,
        })
    }

    /// Bulk-update metadata fields on memories matching filter criteria, checking write access per memory.
    /// Content updates are NOT supported in bulk (would need re-embedding).
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    pub(crate) async fn bulk_update(&self, principal: &str, mut filter: MemoryFilter, ctx: QueryContext, fields: BulkUpdateFields) -> Result<BulkUpdateResult, EngineError> {
        filter.limit = Some(self.limits.max_list_limit);
        let store = self.orchestrator.store();
        let memories = store.list(filter, ctx).await?;
        let matched = memories.len();

        let update = MemoryUpdate {
            tags: fields.tags,
            importance: fields.importance,
            access_policy: fields.access_policy,
            ..Default::default()
        };
        self.validate_update_fields(&update)?;

        let all_ids: Vec<MemoryId> = memories.iter().map(|m| m.id).collect();
        let now = self.clock.now();
        let audit = self.audit_draft(AuditAction::BulkUpdate, Some(principal.to_owned()), None);
        let outcome = store.bulk_update_ids_audited(all_ids, update, principal, now, &audit).await?;

        let updated = u64::try_from(outcome.applied_ids.len()).unwrap_or(u64::MAX);
        let matched_u64 = u64::try_from(matched).unwrap_or(u64::MAX);
        Ok(BulkUpdateResult {
            updated,
            denied: outcome.denied,
            matched: matched_u64,
            capped: matched >= self.limits.max_list_limit,
        })
    }

    // -- Wave 4: consolidation + audit -------------------------------------------

    /// Find groups of near-duplicate memories based on embedding similarity.
    ///
    /// When `dry_run` is `false`, supersedes duplicate members in each group,
    /// keeping the most recently accessed memory as the representative.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::Store` on persistence-layer failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "consolidation requires scope, threshold, limit, dry_run, and caller — all semantically distinct"
    )]
    pub(crate) async fn consolidate_memories(
        &self,
        principal: &str,
        scopes_any: Option<&[String]>,
        similarity_threshold: f64,
        limit: usize,
        dry_run: bool,
    ) -> Result<ConsolidateResult, EngineError> {
        // Validate similarity_threshold is usable before doing any work.
        if !similarity_threshold.is_finite() || !(0.0_f64..=1.0_f64).contains(&similarity_threshold) {
            return Err(ValidationError::new("similarity_threshold", "must be a finite number in [0.0, 1.0]").into());
        }

        let store = self.orchestrator.store();

        // Fetch all non-superseded memories with embeddings. BFS is O(n log n),
        // so we use a very high limit instead of the old quadratic-constrained cap.
        let memories: Vec<_> = store
            .list_with_embeddings(scopes_any, Self::MAX_CONSOLIDATION_FETCH)
            .await?
            .into_iter()
            .filter(|memory| memory.memory.has_write_access(principal))
            .collect();

        if memories.len() < 2 {
            return Ok(ConsolidateResult {
                groups: Vec::new(),
                merged: false,
            });
        }

        // BFS frontier expansion: discover neighbor pairs via ANN index.
        let pairs = self.bfs_discover_pairs(store, &memories, similarity_threshold).await?;

        let groups = find_duplicate_groups_from_pairs(&memories, &pairs, limit);

        if groups.is_empty() || dry_run {
            return Ok(ConsolidateResult { groups, merged: false });
        }

        // Merge: supersede non-representative members.
        for group in &groups {
            self.merge_consolidation_group(store, group, principal).await?;
        }

        Ok(ConsolidateResult { groups, merged: true })
    }

    /// BFS frontier expansion over the ANN similarity graph.
    ///
    /// For each unclustered memory, queries its ANN neighbors within the
    /// similarity threshold, then expands the frontier to newly discovered
    /// neighbors. Produces the sparse edge list for union-find clustering.
    ///
    /// Complexity: O(n log n) — each memory is visited at most once as a
    /// frontier node, triggering one ANN index lookup.
    async fn bfs_discover_pairs(&self, store: &S, memories: &[crate::store::MemoryWithEmbedding], similarity_threshold: f64) -> Result<Vec<NeighborPair>, EngineError> {
        use std::collections::{HashMap, HashSet, VecDeque};

        let max_l2 = cosine_to_l2_threshold(similarity_threshold);
        let neighbor_limit = self.limits.consolidation_neighbor_limit;

        // Build ID → index lookup.
        let id_to_idx: HashMap<MemoryId, usize> = memories.iter().enumerate().map(|(i, m)| (m.memory.id, i)).collect();

        let mut clustered: HashSet<usize> = HashSet::new();
        let mut pairs: Vec<NeighborPair> = Vec::new();

        for (start_idx, mem) in memories.iter().enumerate() {
            if clustered.contains(&start_idx) || mem.embedding.is_none() {
                continue;
            }

            // BFS from this unclustered memory.
            let mut frontier: VecDeque<usize> = VecDeque::new();
            let mut cluster_members: HashSet<usize> = HashSet::new();
            frontier.push_back(start_idx);
            let _: bool = cluster_members.insert(start_idx);

            #[expect(
                clippy::excessive_nesting,
                reason = "BFS loop with let-else guard is inherently nested — extracting would obscure control flow"
            )]
            while let Some(current_idx) = frontier.pop_front() {
                let Some(current_emb) = memories[current_idx].embedding.as_deref() else {
                    continue;
                };
                let neighbors = store.find_embedding_neighbors(current_emb, max_l2, neighbor_limit).await?;
                collect_bfs_neighbors(
                    &neighbors,
                    memories,
                    &id_to_idx,
                    similarity_threshold,
                    current_idx,
                    &mut cluster_members,
                    &mut frontier,
                    &mut pairs,
                );
            }

            clustered.extend(&cluster_members);
        }

        Ok(pairs)
    }

    /// Mark non-representative members of a duplicate group as superseded.
    ///
    /// Checks write access on each member before marking it superseded.
    /// Skips members the caller cannot modify.
    async fn merge_consolidation_group(&self, store: &S, group: &DuplicateGroup, principal: &str) -> Result<(), EngineError> {
        for &member_id in group.member_ids.iter().filter(|&&id| id != group.representative_id) {
            let details = serde_json::json!({
                "representative_id": group.representative_id.to_string(),
                "similarity": group.similarity,
            });
            let audit = self.audit_draft(AuditAction::Consolidate, Some(principal.to_owned()), Some(details));
            let _outcome = match store.mark_superseded_by_authorized_audited(&member_id, &group.representative_id, principal, &audit).await {
                Ok(outcome) => outcome,
                Err(crate::error::StoreError::Conflict(msg)) => {
                    warn!(member_id = %member_id, reason = %msg, "skipping consolidation: memory already superseded");
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
        }
        Ok(())
    }

    /// Query the audit log for a specific memory.
    pub(crate) async fn query_audit_log(&self, memory_id: &MemoryId, limit: usize) -> Result<Vec<AuditEntry>, EngineError> {
        let clamped = limit.min(self.limits.max_history_limit);
        Ok(self.orchestrator.store().query_audit_log(memory_id, clamped).await?)
    }

    /// Fetch the deleted-memory authorization tombstone for a memory ID.
    pub(crate) async fn get_tombstone(&self, memory_id: &MemoryId) -> Result<Option<MemoryTombstone>, EngineError> {
        Ok(self.orchestrator.store().get_tombstone(memory_id).await?)
    }
}

const fn concrete_zero_limit_search_mode(search_mode: SearchMode) -> SearchMode {
    match search_mode {
        SearchMode::Auto | SearchMode::Hybrid => SearchMode::Hybrid,
        SearchMode::Semantic => SearchMode::Semantic,
        SearchMode::Text => SearchMode::Text,
        SearchMode::Keyword => SearchMode::Keyword,
    }
}

/// Collect valid BFS neighbor pairs from an ANN result set.
///
/// Filters out neighbors that are not in the candidate set, already in the
/// current cluster, or below the similarity threshold. Newly discovered
/// neighbors are added to the cluster and frontier for further expansion.
#[expect(
    clippy::too_many_arguments,
    reason = "BFS state requires all these parameters — cluster, frontier, pairs, index, and threshold"
)]
fn collect_bfs_neighbors(
    neighbors: &[crate::store::EmbeddingNeighbor],
    memories: &[crate::store::MemoryWithEmbedding],
    id_to_idx: &std::collections::HashMap<MemoryId, usize>,
    similarity_threshold: f64,
    current_idx: usize,
    cluster_members: &mut std::collections::HashSet<usize>,
    frontier: &mut std::collections::VecDeque<usize>,
    pairs: &mut Vec<NeighborPair>,
) {
    for &(neighbor_id, l2_distance) in neighbors {
        let Some(&neighbor_idx) = id_to_idx.get(&neighbor_id) else {
            continue;
        };
        if cluster_members.contains(&neighbor_idx) {
            continue;
        }
        let cosine = l2_to_cosine(l2_distance);
        if cosine < similarity_threshold {
            continue;
        }
        pairs.push(NeighborPair {
            id_a: memories[current_idx].memory.id,
            id_b: neighbor_id,
            similarity: cosine,
        });
        let _: bool = cluster_members.insert(neighbor_idx);
        frontier.push_back(neighbor_idx);
    }
}

/// Outcome of a [`LocalHoldEngine::reembed`] call.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReembedOutcome {
    /// The specified number of memories were queued for re-embedding.
    Queued(usize),
    /// The requested single memory was not found or not authorized.
    NotFound(MemoryId),
}

// ---------------------------------------------------------------------------
// Tests (task management tests moved from server/tests.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[expect(unused_results, reason = "test setup and assertions discard many results intentionally")]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use chrono::TimeZone as _;
    use parking_lot::Mutex;

    use super::*;
    use crate::{
        clock::MockClock,
        config::LimitsConfig,
        embedding::{BoxFuture, EmbeddingProvider, NoopEmbedding},
        error::EmbeddingError,
        store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
    };

    struct HealthyEmbedding;

    impl EmbeddingProvider for HealthyEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async {
                let mut embedding = vec![0.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
                embedding[0] = 1.0;
                Ok(embedding)
            })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct CountingEmbedding {
        embed_calls: AtomicUsize,
    }

    struct BatchCountingEmbedding {
        batch_sizes: Mutex<Vec<usize>>,
        single_calls: AtomicUsize,
    }

    impl BatchCountingEmbedding {
        const fn new() -> Self {
            Self {
                batch_sizes: Mutex::new(Vec::new()),
                single_calls: AtomicUsize::new(0),
            }
        }

        fn sorted_batch_sizes(&self) -> Vec<usize> {
            let mut sizes = self.batch_sizes.lock().clone();
            sizes.sort_unstable();
            sizes
        }

        fn embedding() -> Vec<f32> {
            let mut embedding = vec![0.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
            embedding[0] = 1.0;
            embedding
        }
    }

    impl EmbeddingProvider for BatchCountingEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            let _previous = self.single_calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(Self::embedding()) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }

        fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            self.batch_sizes.lock().push(texts.len());
            Box::pin(async move { Ok(texts.iter().map(|_text| Self::embedding()).collect()) })
        }
    }

    struct InputIsolatingEmbedding {
        batch_calls: AtomicUsize,
        single_calls: AtomicUsize,
    }

    impl InputIsolatingEmbedding {
        const fn new() -> Self {
            Self {
                batch_calls: AtomicUsize::new(0),
                single_calls: AtomicUsize::new(0),
            }
        }

        fn embed_sync(text: &str) -> Result<Vec<f32>, EmbeddingError> {
            if text == "invalid" {
                Err(EmbeddingError::Permanent("invalid input".into()))
            } else {
                Ok(BatchCountingEmbedding::embedding())
            }
        }
    }

    impl EmbeddingProvider for InputIsolatingEmbedding {
        fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            let _previous = self.single_calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move { Self::embed_sync(text) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }

        fn embed_batch<'a>(&'a self, _texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            let _previous = self.batch_calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Err(EmbeddingError::Permanent("batch contains invalid input".into())) })
        }
    }

    impl CountingEmbedding {
        const fn new() -> Self {
            Self { embed_calls: AtomicUsize::new(0) }
        }

        fn embed_call_count(&self) -> usize {
            self.embed_calls.load(Ordering::Acquire)
        }
    }

    impl EmbeddingProvider for CountingEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            self.embed_calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { Ok(vec![1.0_f32, 0.0, 0.0]) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct BlockingHealthCheckEmbedding {
        started: AtomicBool,
        started_notify: tokio::sync::Notify,
        release_health_check: tokio::sync::Notify,
    }

    impl BlockingHealthCheckEmbedding {
        fn new() -> Self {
            Self {
                started: AtomicBool::new(false),
                started_notify: tokio::sync::Notify::new(),
                release_health_check: tokio::sync::Notify::new(),
            }
        }

        async fn wait_until_started(&self) {
            while !self.started.load(Ordering::Acquire) {
                self.started_notify.notified().await;
            }
        }
    }

    impl EmbeddingProvider for BlockingHealthCheckEmbedding {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Ok(vec![1.0_f32, 0.0, 0.0]) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async move {
                self.started.store(true, Ordering::Release);
                self.started_notify.notify_waiters();
                self.release_health_check.notified().await;
                Ok(())
            })
        }
    }

    fn make_engine() -> LocalHoldEngine<SqliteStore> {
        let store = SqliteStore::in_memory().unwrap();
        let embedding = Arc::new(NoopEmbedding::new());
        LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
    }

    fn make_engine_with_embedding(embedding: Arc<dyn EmbeddingProvider>) -> LocalHoldEngine<SqliteStore> {
        let store = SqliteStore::in_memory().unwrap();
        LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
    }

    fn make_engine_with_store(store: SqliteStore, embedding: Arc<dyn EmbeddingProvider>) -> LocalHoldEngine<SqliteStore> {
        LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
    }

    fn make_engine_with_limits(limits: LimitsConfig) -> LocalHoldEngine<SqliteStore> {
        let store = SqliteStore::in_memory().unwrap();
        let embedding = Arc::new(NoopEmbedding::new());
        LocalHoldEngine::new(store, embedding, limits, SearchConfig::default())
    }

    fn make_engine_with_search_config(search_config: SearchConfig) -> LocalHoldEngine<SqliteStore> {
        let store = SqliteStore::in_memory().unwrap();
        let embedding = Arc::new(NoopEmbedding::new());
        LocalHoldEngine::new(store, embedding, LimitsConfig::default(), search_config)
    }

    fn fixed_id(value: &str) -> MemoryId {
        value.parse().unwrap()
    }

    fn test_input(content: &str) -> StoreMemoryInput {
        StoreMemoryInput {
            content: content.into(),
            tags: vec![],
            source_agent: Some("test-agent".into()),
            source_user: None,
            source_conversation: None,
            origin_conversation: None,
            access_policy: None,
            ttl_seconds: None,
            memory_type: None,
            importance: None,
            confidence: None,
            supersedes: None,
            entities: vec![],
        }
    }

    async fn begin_shutdown(engine: &LocalHoldEngine<SqliteStore>) -> (tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        engine.spawn_tracked_task(async move {
            #[expect(clippy::let_underscore_must_use, reason = "test task only holds shutdown open")]
            let _ = rx.await;
        });

        let shutdown_engine = engine.clone();
        let shutdown = tokio::spawn(async move {
            shutdown_engine.shutdown_for_test(Duration::from_secs(1)).await;
        });

        while !engine.orchestrator.background_tasks().is_shutting_down() {
            tokio::task::yield_now().await;
        }

        (tx, shutdown)
    }

    // -- task management tests -----------------------------------------------

    #[tokio::test]
    async fn tracked_task_reap_hook_clears_finished_tasks() {
        let engine = make_engine();
        let (tx, rx) = tokio::sync::oneshot::channel();

        engine.spawn_tracked_task(async move {
            #[expect(clippy::let_underscore_must_use, reason = "test task that just waits for a signal; recv error is non-actionable")]
            let _ = rx.await;
        });
        assert_eq!(engine.tracked_task_count(), 1);

        tx.send(()).unwrap();
        let mut reaped = engine.reap_completed_tasks_for_test();
        while reaped == 0 {
            tokio::task::yield_now().await;
            reaped = engine.reap_completed_tasks_for_test();
        }
        assert_eq!(reaped, 1);
        assert_eq!(engine.tracked_task_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_reaps_panicked_background_tasks() {
        let engine = make_engine();
        #[expect(clippy::panic, reason = "intentionally panicking task to test shutdown reaping")]
        engine.spawn_tracked_task(async { panic!("background panic for test") });
        engine.shutdown().await;
        assert_eq!(engine.tracked_task_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_timeout_drops_stuck_background_tasks() {
        let clock = Arc::new(MockClock::new());
        let store = SqliteStore::in_memory_with_clock(Arc::<MockClock>::clone(&clock)).unwrap();
        let engine = LocalHoldEngine::new_with_clock(
            store,
            Arc::new(NoopEmbedding::new()),
            LimitsConfig::default(),
            SearchConfig::default(),
            Arc::<MockClock>::clone(&clock),
        );
        engine.spawn_tracked_task(async move {
            std::future::pending::<()>().await;
        });
        let shutdown = engine.shutdown_for_test(Duration::from_millis(5));
        tokio::pin!(shutdown);
        assert!(futures::poll!(shutdown.as_mut()).is_pending());
        clock.advance(chrono::TimeDelta::milliseconds(5));
        shutdown.await;
        assert_eq!(engine.tracked_task_count(), 0);
    }

    #[tokio::test]
    async fn store_memory_rejected_while_shutdown_in_progress() {
        let engine = make_engine();
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let memory = engine.build_memory(test_input("shutdown write"), engine.now()).unwrap();
        let err = engine.store_memory(memory, None).await.unwrap_err();
        assert!(matches!(err, EngineError::ShuttingDown));

        tx.send(()).unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn batch_store_rejected_while_shutdown_in_progress() {
        let engine = make_engine();
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let memories = vec![engine.build_memory(test_input("shutdown batch"), engine.now()).unwrap()];
        let err = engine.batch_store(memories, vec![None]).await.unwrap_err();
        assert!(matches!(err, EngineError::ShuttingDown));

        tx.send(()).unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn update_memory_with_content_rejected_while_shutdown_in_progress() {
        let engine = make_engine();
        let id = engine.store_memory(engine.build_memory(test_input("before"), engine.now()).unwrap(), None).await.unwrap();
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let err = engine
            .update_memory(
                id,
                MemoryUpdate {
                    content: Some("after".into()),
                    ..Default::default()
                },
                "test-agent",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::ShuttingDown));

        tx.send(()).unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn tag_only_update_still_allowed_during_shutdown() {
        let engine = make_engine();
        let id = engine.store_memory(engine.build_memory(test_input("before"), engine.now()).unwrap(), None).await.unwrap();
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let outcome = engine
            .update_memory(
                id,
                MemoryUpdate {
                    tags: Some(vec!["updated".into()]),
                    ..Default::default()
                },
                "test-agent",
            )
            .await
            .unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Applied);

        tx.send(()).unwrap();
        shutdown.await.unwrap();

        let history = engine.query_audit_log(&id, 10).await.unwrap();
        assert!(history.iter().any(|entry| entry.action == AuditAction::Update));
    }

    #[tokio::test]
    async fn delete_still_audited_during_shutdown() {
        let engine = make_engine();
        let id = engine.store_memory(engine.build_memory(test_input("before"), engine.now()).unwrap(), None).await.unwrap();
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let outcome = engine.delete_memory(&id, "test-agent").await.unwrap();
        assert_eq!(outcome, WriteOutcome::Applied);

        tx.send(()).unwrap();
        shutdown.await.unwrap();

        let history = engine.query_audit_log(&id, 10).await.unwrap();
        assert!(history.iter().any(|entry| entry.action == AuditAction::Delete));
    }

    #[tokio::test]
    async fn search_access_tracking_still_records_during_shutdown() {
        let engine = make_engine();
        let id = engine
            .store_memory(engine.build_memory(test_input("searchable needle"), engine.now()).unwrap(), None)
            .await
            .unwrap();
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let outcome = engine
            .search_memories(SearchRequest {
                query: "needle".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Text),
                context: None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.results.len(), 1);

        let stored = engine.get_memory(&id, None).await.unwrap().unwrap();
        assert_eq!(stored.impression_count, 1);
        assert!(stored.last_impressed_at.is_some());

        tx.send(()).unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn reembed_rejected_while_shutdown_in_progress() {
        let engine = make_engine_with_embedding(Arc::new(HealthyEmbedding));
        let (tx, shutdown) = begin_shutdown(&engine).await;

        let err = engine.reembed(ReembedRequest::Bulk { limit: 1 }).await.unwrap_err();
        assert!(matches!(err, EngineError::ShuttingDown));

        tx.send(()).unwrap();
        shutdown.await.unwrap();
    }

    #[tokio::test]
    async fn reembed_health_check_does_not_block_shutdown_timeout() {
        let embedding = Arc::new(BlockingHealthCheckEmbedding::new());
        let embedding_waiter = Arc::clone(&embedding);
        let clock = Arc::new(MockClock::new());
        let store = SqliteStore::in_memory_with_clock(Arc::<MockClock>::clone(&clock)).unwrap();
        let engine = LocalHoldEngine::new_with_clock(store, embedding, LimitsConfig::default(), SearchConfig::default(), Arc::<MockClock>::clone(&clock));

        let reembed_engine = engine.clone();
        let reembed = tokio::spawn(async move { reembed_engine.reembed(ReembedRequest::Bulk { limit: 1 }).await });

        embedding_waiter.wait_until_started().await;

        let shutdown = engine.shutdown_for_test(Duration::from_millis(10));
        tokio::pin!(shutdown);
        assert!(
            futures::poll!(shutdown.as_mut()).is_ready(),
            "health checks outside tracked embedding work must not hold shutdown open"
        );

        reembed.abort();
        let aborted = reembed.await.unwrap_err();
        assert!(aborted.is_cancelled());
    }

    // -- build_memory tests (#9) ---------------------------------------------

    #[test]
    fn build_memory_success() {
        let engine = make_engine();
        let now = engine.now();
        let input = StoreMemoryInput {
            content: "test content".into(),
            tags: vec!["tag1".into()],
            source_agent: Some("agent-a".into()),
            source_user: Some("user-a".into()),
            source_conversation: Some("conv-1".into()),
            origin_conversation: None,
            access_policy: None,
            ttl_seconds: Some(3600),
            memory_type: None,
            importance: None,
            confidence: None,
            supersedes: None,
            entities: vec![],
        };
        let memory = engine.build_memory(input, now).unwrap();
        assert_eq!(memory.content, "test content");
        assert_eq!(memory.tags, vec!["tag1"]);
        assert_eq!(memory.provenance.source_agent.as_deref(), Some("agent-a"));
        assert_eq!(memory.provenance.source_user.as_deref(), Some("user-a"));
        assert_eq!(memory.provenance.source_conversation.as_deref(), Some("conv-1"));
        // origin_conversation defaults from source_conversation
        assert_eq!(memory.provenance.origin_conversation.as_deref(), Some("conv-1"));
        assert!(memory.expires_at.is_some());
        assert!(!memory.has_embedding);
    }

    #[test]
    fn build_memory_trims_provenance_fields() {
        let engine = make_engine();
        let mut input = test_input("trimmed provenance");
        input.source_agent = Some("  agent-a  ".into());
        input.source_user = Some("  user-a  ".into());
        input.source_conversation = Some("  project-1  ".into());
        input.origin_conversation = Some("  conv-a  ".into());

        let memory = engine.build_memory(input, engine.now()).unwrap();

        assert_eq!(memory.provenance.source_agent.as_deref(), Some("agent-a"));
        assert_eq!(memory.provenance.source_user.as_deref(), Some("user-a"));
        assert_eq!(memory.provenance.source_conversation.as_deref(), Some("project-1"));
        assert_eq!(memory.provenance.origin_conversation.as_deref(), Some("conv-a"));
    }

    #[test]
    fn build_memory_rejects_blank_content() {
        let engine = make_engine();
        let input = test_input("   ");
        let err = engine.build_memory(input, engine.now()).unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[test]
    fn build_memory_rejects_excess_tags() {
        let limits = LimitsConfig {
            max_tags_per_memory: 2,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);
        let mut input = test_input("content");
        input.tags = vec!["a".into(), "b".into(), "c".into()];
        let err = engine.build_memory(input, engine.now()).unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[test]
    fn build_memory_rejects_ttl_overflow() {
        let engine = make_engine();
        let mut input = test_input("content");
        input.ttl_seconds = Some(u64::MAX);
        let err = engine.build_memory(input, engine.now()).unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[test]
    fn build_memory_rejects_blank_source_agent() {
        let engine = make_engine();
        let mut input = test_input("content");
        input.source_agent = Some("   ".into());
        let err = engine.build_memory(input, engine.now()).unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[test]
    fn build_memory_rejects_blank_tag() {
        let engine = make_engine();
        let mut input = test_input("content");
        input.tags = vec!["good".into(), "  ".into()];
        let err = engine.build_memory(input, engine.now()).unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    // -- store_memory tests (#9) ---------------------------------------------

    #[tokio::test]
    async fn store_memory_success() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("test content"), engine.now()).unwrap();
        let id = engine.store_memory(memory, None).await.unwrap();
        let retrieved = engine.get_memory(&id, None).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "test content");
    }

    // -- search_memories tests (#9) ------------------------------------------

    #[tokio::test]
    async fn search_memories_fallback_to_text() {
        let store = SqliteStore::in_memory().unwrap();
        store.set_fts_available_for_test(false);
        let engine = make_engine_with_store(store, Arc::new(NoopEmbedding::new()));
        let memory = engine.build_memory(test_input("the quick brown fox"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let outcome = engine
            .search_memories(SearchRequest {
                query: "quick".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Auto),
                context: None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.search_mode, SearchMode::Text);
        assert_eq!(outcome.results.len(), 1);
    }

    #[tokio::test]
    async fn explicit_semantic_rejects_embedding_fallback() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("searchable fallback candidate"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let err = engine
            .search_memories(SearchRequest {
                query: "searchable".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Semantic),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::SearchUnavailable(_)));
    }

    #[tokio::test]
    async fn explicit_hybrid_rejects_embedding_fallback() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("searchable fallback candidate"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let err = engine
            .search_memories(SearchRequest {
                query: "searchable".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Hybrid),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::SearchUnavailable(_)));
    }

    #[tokio::test]
    async fn explicit_hybrid_rejects_missing_fts_backend() {
        let store = SqliteStore::in_memory().unwrap();
        store.set_fts_available_for_test(false);
        let engine = make_engine_with_store(store, Arc::new(HealthyEmbedding));

        let memory = engine.build_memory(test_input("vector searchable"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let err = engine
            .search_memories(SearchRequest {
                query: "vector".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Hybrid),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::SearchUnavailable(_)));
    }

    #[tokio::test]
    async fn explicit_keyword_rejects_missing_fts_backend() {
        let store = SqliteStore::in_memory().unwrap();
        store.set_fts_available_for_test(false);
        let engine = make_engine_with_store(store, Arc::new(NoopEmbedding::new()));

        let memory = engine.build_memory(test_input("keyword searchable"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let err = engine
            .search_memories(SearchRequest {
                query: "keyword".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Keyword),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::SearchUnavailable(_)));
    }

    #[tokio::test]
    async fn search_memories_rejects_blank_query() {
        let engine = make_engine();
        let err = engine
            .search_memories(SearchRequest {
                query: "   ".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Semantic),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[tokio::test]
    async fn search_memories_rejects_invalid_max_distance() {
        let engine = make_engine();
        let err = engine
            .search_memories(SearchRequest {
                query: "test".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: Some(f64::NEG_INFINITY),
                keywords: None,
                search_mode: Some(SearchMode::Semantic),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));

        let err = engine
            .search_memories(SearchRequest {
                query: "test".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: Some(-1.0_f64),
                keywords: None,
                search_mode: Some(SearchMode::Semantic),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[tokio::test]
    async fn search_memories_clamps_limit() {
        let limits = LimitsConfig {
            max_search_limit: 5,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);

        // Store 10 memories
        for i in 0_i32..10_i32 {
            let memory = engine.build_memory(test_input(&format!("needle content {i}")), engine.now()).unwrap();
            engine.store_memory(memory, None).await.unwrap();
        }

        let outcome = engine
            .search_memories(SearchRequest {
                query: "needle".into(),
                limit: 100,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Auto),
                context: None,
            })
            .await
            .unwrap();
        assert!(outcome.results.len() <= 5);
    }

    #[test]
    fn candidate_pool_limits_apply_path_depths_and_configured_cap() {
        let limits = LimitsConfig {
            max_candidate_pool_size: 75,
            ..LimitsConfig::default()
        };
        let search_config = SearchConfig {
            semantic_candidate_k: 100,
            keyword_candidate_k: 20,
            ..SearchConfig::default()
        };
        let store = SqliteStore::in_memory().unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(NoopEmbedding::new()), limits, search_config);

        let candidate_limits = engine.candidate_pool_limits(5);

        assert_eq!(candidate_limits.semantic, 75);
        assert_eq!(candidate_limits.keyword, 20);
        assert_eq!(candidate_limits.text, 20);
        assert_eq!(candidate_limits.fused, 75);
    }

    #[test]
    fn candidate_pool_limits_fuse_union_before_composite_scoring() {
        let limits = LimitsConfig {
            max_candidate_pool_size: 100,
            ..LimitsConfig::default()
        };
        let search_config = SearchConfig {
            semantic_candidate_k: 30,
            keyword_candidate_k: 40,
            ..SearchConfig::default()
        };
        let store = SqliteStore::in_memory().unwrap();
        let engine = LocalHoldEngine::new(store, Arc::new(NoopEmbedding::new()), limits, search_config);

        let candidate_limits = engine.candidate_pool_limits(5);

        assert_eq!(candidate_limits.semantic, 30);
        assert_eq!(candidate_limits.keyword, 40);
        assert_eq!(candidate_limits.fused, 70);
    }

    #[tokio::test]
    async fn search_memories_text_candidate_pool_allows_composite_promotion() {
        let search_config = SearchConfig {
            keyword_candidate_k: 2,
            relevance_weight: 50.0,
            importance_weight: 50.0,
            freshness_weight: 0.0,
            activity_weight: 0.0,
            confidence_weight: 0.0,
            ..SearchConfig::default()
        };
        let engine = make_engine_with_search_config(search_config);
        let now = engine.now();

        let mut older_important = engine.build_memory(test_input("needle important"), now).unwrap();
        older_important.created_at = now - chrono::Duration::hours(1);
        older_important.updated_at = older_important.created_at;
        older_important.importance = Importance::new(1.0);
        engine.store_memory(older_important, None).await.unwrap();

        let mut newer_unimportant = engine.build_memory(test_input("needle unimportant"), now).unwrap();
        newer_unimportant.created_at = now;
        newer_unimportant.updated_at = now;
        newer_unimportant.importance = Importance::new(0.0);
        engine.store_memory(newer_unimportant, None).await.unwrap();

        let outcome = engine
            .search_memories(SearchRequest {
                query: "needle".into(),
                limit: 1,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Text),
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].memory.content, "needle important");
    }

    #[tokio::test]
    async fn search_memories_hybrid_candidate_pool_allows_composite_promotion() {
        let mut query_emb = vec![0.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
        query_emb[0] = 1.0;
        let store = SqliteStore::in_memory().unwrap();
        let now = chrono::Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let provenance = Provenance {
            source_agent: Some("test-agent".into()),
            ..Provenance::default()
        };

        let mut low_raw_rank = Memory::new_for_test("needle low raw rank".into(), Vec::new(), provenance.clone(), AccessPolicy::Public);
        low_raw_rank.id = fixed_id("01J0000000000000000000000A");
        low_raw_rank.created_at = now;
        low_raw_rank.updated_at = now;
        low_raw_rank.importance = Importance::new(0.0);
        store.store(&low_raw_rank, Some(&query_emb)).await.unwrap();

        let mut high_composite = Memory::new_for_test("needle high composite".into(), Vec::new(), provenance, AccessPolicy::Public);
        high_composite.id = fixed_id("01J0000000000000000000000B");
        high_composite.created_at = now - chrono::Duration::hours(1);
        high_composite.updated_at = high_composite.created_at;
        high_composite.importance = Importance::new(1.0);
        store.store(&high_composite, Some(&query_emb)).await.unwrap();

        let search_config = SearchConfig {
            semantic_candidate_k: 2,
            keyword_candidate_k: 2,
            relevance_weight: 1.0,
            importance_weight: 99.0,
            freshness_weight: 0.0,
            activity_weight: 0.0,
            confidence_weight: 0.0,
            ..SearchConfig::default()
        };
        let engine = LocalHoldEngine::new(store, Arc::new(HealthyEmbedding), LimitsConfig::default(), search_config);

        let outcome = engine
            .search_memories(SearchRequest {
                query: "needle".into(),
                limit: 1,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Hybrid),
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].memory.id, fixed_id("01J0000000000000000000000B"));
    }

    #[tokio::test]
    async fn search_memories_zero_limit_skips_embedding_work() {
        let embedding = Arc::new(CountingEmbedding::new());
        let embedding_clone: Arc<CountingEmbedding> = Arc::clone(&embedding);
        let embedding_provider: Arc<dyn EmbeddingProvider> = embedding_clone;
        let engine = make_engine_with_embedding(embedding_provider);

        let outcome = engine
            .search_memories(SearchRequest {
                query: "needle".into(),
                limit: 0,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: None,
                keywords: None,
                search_mode: Some(SearchMode::Auto),
                context: Some("extra semantic context".into()),
            })
            .await
            .unwrap();

        assert!(outcome.results.is_empty());
        assert_eq!(outcome.search_mode, SearchMode::Hybrid);
        assert_eq!(embedding.embed_call_count(), 0);
    }

    // -- update_memory tests (#9) --------------------------------------------

    #[tokio::test]
    async fn update_memory_applied_with_reembed() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("before"), engine.now()).unwrap();
        let id = engine.store_memory(memory, None).await.unwrap();

        let update = MemoryUpdate {
            content: Some("after".into()),
            ..Default::default()
        };
        let outcome = engine.update_memory(id, update, "test-agent").await.unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Applied);

        let retrieved = engine.get_memory(&id, None).await.unwrap().unwrap();
        assert_eq!(retrieved.content, "after");
    }

    #[tokio::test]
    async fn update_memory_denied() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("owner data"), engine.now()).unwrap();
        let id = engine.store_memory(memory, None).await.unwrap();

        let update = MemoryUpdate {
            content: Some("tampered".into()),
            ..Default::default()
        };
        let outcome = engine.update_memory(id, update, "other-agent").await.unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Denied);
    }

    #[tokio::test]
    async fn update_memory_not_found() {
        let engine = make_engine();
        let fake_id = MemoryId::new();
        let update = MemoryUpdate {
            content: Some("test".into()),
            ..Default::default()
        };
        let outcome = engine.update_memory(fake_id, update, "test-agent").await.unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::NotFound);
    }

    #[tokio::test]
    async fn update_memory_rejects_blank_content() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("original"), engine.now()).unwrap();
        let id = engine.store_memory(memory, None).await.unwrap();

        let update = MemoryUpdate {
            content: Some("   ".into()),
            ..Default::default()
        };
        let err = engine.update_memory(id, update, "test-agent").await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[tokio::test]
    async fn update_memory_rejects_excess_tags() {
        let limits = LimitsConfig {
            max_tags_per_memory: 2,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);
        let memory = engine.build_memory(test_input("original"), engine.now()).unwrap();
        let id = engine.store_memory(memory, None).await.unwrap();

        let update = MemoryUpdate {
            tags: Some(vec!["a".into(), "b".into(), "c".into()]),
            ..Default::default()
        };
        let err = engine.update_memory(id, update, "test-agent").await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    // -- batch_store tests (#9) ----------------------------------------------

    #[tokio::test]
    async fn batch_store_success() {
        let engine = make_engine();
        let now = engine.now();
        let memories: Vec<Memory> = (0_i32..3_i32).map(|i| engine.build_memory(test_input(&format!("item {i}")), now).unwrap()).collect();

        let supersedes = vec![None; memories.len()];
        let ids = engine.batch_store(memories, supersedes).await.unwrap();
        assert_eq!(ids.len(), 3);

        for id in &ids {
            let mem = engine.get_memory(id, None).await.unwrap();
            assert!(mem.is_some());
        }
    }

    #[tokio::test]
    async fn batch_store_uses_configured_embedding_chunks() {
        let store = SqliteStore::in_memory().unwrap();
        let provider = Arc::new(BatchCountingEmbedding::new());
        let limits = LimitsConfig {
            max_batch_size: 10,
            embedding_batch_size: 2,
            ..LimitsConfig::default()
        };
        let engine_provider: Arc<dyn EmbeddingProvider> = Arc::<BatchCountingEmbedding>::clone(&provider);
        let engine = LocalHoldEngine::new(store.clone(), engine_provider, limits, SearchConfig::default());
        let now = engine.now();
        let memories: Vec<Memory> = (0_i32..5_i32).map(|i| engine.build_memory(test_input(&format!("item {i}")), now).unwrap()).collect();

        let ids = engine.batch_store(memories, vec![None; 5]).await.unwrap();
        engine.shutdown_for_test(Duration::from_secs(1)).await;

        assert_eq!(provider.sorted_batch_sizes(), vec![1, 2, 2]);
        assert_eq!(provider.single_calls.load(Ordering::Acquire), 0);
        for id in ids {
            assert!(store.get(&id, None).await.unwrap().unwrap().has_embedding);
        }
    }

    #[tokio::test]
    async fn batch_store_isolates_permanent_input_failures() {
        let store = SqliteStore::in_memory().unwrap();
        let provider = Arc::new(InputIsolatingEmbedding::new());
        let engine_provider: Arc<dyn EmbeddingProvider> = Arc::<InputIsolatingEmbedding>::clone(&provider);
        let engine = LocalHoldEngine::new(store.clone(), engine_provider, LimitsConfig::default(), SearchConfig::default());
        let now = engine.now();
        let memories: Vec<Memory> = ["valid first", "invalid", "valid second"]
            .into_iter()
            .map(|content| engine.build_memory(test_input(content), now).unwrap())
            .collect();

        let ids = engine.batch_store(memories, vec![None; 3]).await.unwrap();
        engine.shutdown_for_test(Duration::from_secs(1)).await;

        assert_eq!(provider.batch_calls.load(Ordering::Acquire), 1);
        assert_eq!(provider.single_calls.load(Ordering::Acquire), 3);
        assert!(store.get(&ids[0], None).await.unwrap().unwrap().has_embedding);
        assert!(!store.get(&ids[1], None).await.unwrap().unwrap().has_embedding);
        assert!(store.get(&ids[2], None).await.unwrap().unwrap().has_embedding);
    }

    #[tokio::test]
    async fn batch_store_rejects_empty() {
        let engine = make_engine();
        let err = engine.batch_store(vec![], vec![]).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[tokio::test]
    async fn batch_store_rejects_oversized() {
        let limits = LimitsConfig {
            max_batch_size: 2,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);
        let now = engine.now();
        let memories: Vec<Memory> = (0_i32..3_i32).map(|i| engine.build_memory(test_input(&format!("item {i}")), now).unwrap()).collect();

        let supersedes = vec![None; memories.len()];
        let err = engine.batch_store(memories, supersedes).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    // -- reembed tests (#9) --------------------------------------------------

    #[tokio::test]
    async fn reembed_bulk_health_check_failure() {
        let engine = make_engine(); // NoopEmbedding fails health check
        let err = engine.reembed(ReembedRequest::Bulk { limit: 10 }).await.unwrap_err();
        assert!(matches!(err, EngineError::Embedding(_)));
    }

    #[tokio::test]
    async fn reembed_single_health_check_failure() {
        let engine = make_engine();
        let err = engine
            .reembed(ReembedRequest::Single {
                id: MemoryId::new(),
                principal: "agent".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Embedding(_)));
    }

    #[tokio::test]
    async fn reembed_bulk_rejects_over_limit() {
        let limits = LimitsConfig {
            max_reembed_limit: 5,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);
        let err = engine.reembed(ReembedRequest::Bulk { limit: 10 }).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[tokio::test]
    async fn reembed_bulk_uses_configured_embedding_chunks() {
        let store = SqliteStore::in_memory().unwrap();
        let mut ids = Vec::new();
        for index in 0_i32..5_i32 {
            let memory = Memory::new_for_test(format!("backlog {index}"), Vec::new(), Provenance::default(), AccessPolicy::Public);
            ids.push(store.store(&memory, None).await.unwrap());
        }
        let provider = Arc::new(BatchCountingEmbedding::new());
        let limits = LimitsConfig {
            max_reembed_limit: 10,
            embedding_batch_size: 2,
            ..LimitsConfig::default()
        };
        let engine_provider: Arc<dyn EmbeddingProvider> = Arc::<BatchCountingEmbedding>::clone(&provider);
        let engine = LocalHoldEngine::new(store.clone(), engine_provider, limits, SearchConfig::default());

        let outcome = engine.reembed(ReembedRequest::Bulk { limit: 5 }).await.unwrap();
        assert!(matches!(outcome, ReembedOutcome::Queued(5)));
        engine.shutdown_for_test(Duration::from_secs(1)).await;

        assert_eq!(provider.sorted_batch_sizes(), vec![1, 2, 2]);
        assert_eq!(provider.single_calls.load(Ordering::Acquire), 0);
        for id in ids {
            assert!(store.get(&id, None).await.unwrap().unwrap().has_embedding);
        }
    }

    // -- reassign_scope tests (#9) -------------------------------------------

    #[tokio::test]
    async fn reassign_scope_rejects_same_scope() {
        let engine = make_engine();
        let err = engine.reassign_scope("conv-1", "conv-1", None, "caller").await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[tokio::test]
    async fn reassign_scope_success() {
        let engine = make_engine();
        let mut input = test_input("scoped memory");
        input.source_conversation = Some("project-1".into());
        input.origin_conversation = Some("conv-a".into());
        let memory = engine.build_memory(input, engine.now()).unwrap();
        let id = engine.store_memory(memory, None).await.unwrap();

        let count = engine.reassign_scope("project-1", "new-scope", None, "test-agent").await.unwrap();
        assert_eq!(count, 1);

        let mem = engine.get_memory(&id, None).await.unwrap().unwrap();
        assert_eq!(mem.provenance.source_conversation.as_deref(), Some("new-scope"));

        // Drain fire-and-forget audit tasks before checking the log.
        engine.shutdown_for_test(Duration::from_secs(2)).await;

        let history = engine.query_audit_log(&id, 10).await.unwrap();
        assert!(
            history
                .iter()
                .any(|entry| entry.action == AuditAction::Reassign && entry.caller_agent.as_deref() == Some("test-agent")),
            "reassign audit should be recorded on the moved memory"
        );
    }

    #[tokio::test]
    async fn reassign_scope_skips_non_writable_memories() {
        let engine = make_engine();

        let mut owned = test_input("owned");
        owned.source_agent = Some("caller".into());
        owned.source_conversation = Some("project-1".into());
        let owned_id = engine.store_memory(engine.build_memory(owned, engine.now()).unwrap(), None).await.unwrap();

        let mut denied = test_input("denied");
        denied.source_agent = Some("other".into());
        denied.source_conversation = Some("project-1".into());
        let denied_id = engine.store_memory(engine.build_memory(denied, engine.now()).unwrap(), None).await.unwrap();

        let count = engine.reassign_scope("project-1", "project-2", None, "caller").await.unwrap();
        assert_eq!(count, 1);

        let owned_after = engine.get_memory(&owned_id, None).await.unwrap().unwrap();
        assert_eq!(owned_after.provenance.source_conversation.as_deref(), Some("project-2"));

        let denied_after = engine.get_memory(&denied_id, None).await.unwrap().unwrap();
        assert_eq!(denied_after.provenance.source_conversation.as_deref(), Some("project-1"));
    }

    #[tokio::test]
    async fn query_audit_log_clamps_limit() {
        let limits = LimitsConfig {
            max_history_limit: 2,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);
        let id = engine
            .store_memory(engine.build_memory(test_input("history target"), engine.now()).unwrap(), None)
            .await
            .unwrap();

        engine
            .update_memory(
                id,
                MemoryUpdate {
                    tags: Some(vec!["updated".into()]),
                    ..Default::default()
                },
                "test-agent",
            )
            .await
            .unwrap();
        let _outcome = engine.delete_memory(&id, "test-agent").await.unwrap();
        engine.shutdown_for_test(Duration::from_secs(2)).await;

        let history = engine.query_audit_log(&id, 999).await.unwrap();
        assert_eq!(history.len(), 2);
    }

    // -- search_memories max_distance upper bound (#SE4) ----------------------

    #[tokio::test]
    async fn search_memories_rejects_excessive_max_distance() {
        let engine = make_engine();
        let err = engine
            .search_memories(SearchRequest {
                query: "test".into(),
                limit: 10,
                filter: MemoryFilter::default(),
                ctx: QueryContext::default(),
                max_distance: Some(11.0_f64),
                keywords: None,
                search_mode: Some(SearchMode::Semantic),
                context: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    // -- count_memories clamp test (#SE2) ------------------------------------

    #[tokio::test]
    async fn count_memories_clamps_top_tags_limit() {
        let limits = LimitsConfig {
            max_top_tags_limit: 5,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);

        // Store a memory so count returns something
        let memory = engine.build_memory(test_input("tagged content"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        // Request 200, but limit is 5 — should not error, just clamp
        let stats = engine.count_memories(MemoryFilter::default(), QueryContext::default(), 200).await.unwrap();
        // The query ran successfully; we cannot directly assert the clamped value
        // was passed to the store, but the absence of an error confirms it didn't
        // blow up with an unreasonable limit.
        assert!(stats.total >= 1);
    }

    // -- list_memories clamp test (#9) ---------------------------------------

    #[tokio::test]
    async fn list_memories_clamps_limit() {
        let limits = LimitsConfig {
            max_list_limit: 2,
            ..LimitsConfig::default()
        };
        let engine = make_engine_with_limits(limits);

        for i in 0_i32..5_i32 {
            let memory = engine.build_memory(test_input(&format!("item {i}")), engine.now()).unwrap();
            engine.store_memory(memory, None).await.unwrap();
        }

        let filter = MemoryFilter {
            limit: Some(100),
            ..Default::default()
        };
        let memories = engine.list_memories(filter, QueryContext::default()).await.unwrap();
        assert!(memories.len() <= 2);
    }

    // -- Wave 2: Supersession tracking engine tests --

    #[tokio::test]
    async fn store_with_supersedes_sets_superseded_by() {
        let engine = make_engine();
        let now = engine.now();
        let mem_a = engine.build_memory(test_input("old fact"), now).unwrap();
        let id_a = engine.store_memory(mem_a, None).await.unwrap();

        let mem_b = engine.build_memory(test_input("new fact"), now).unwrap();
        let id_b = engine.store_memory(mem_b, Some(&id_a)).await.unwrap();

        let retrieved_a = engine.get_memory(&id_a, None).await.unwrap().unwrap();
        assert_eq!(retrieved_a.superseded_by, Some(id_b));
    }

    #[tokio::test]
    async fn superseded_memory_hidden_from_list_by_default() {
        let engine = make_engine();
        let now = engine.now();
        let mem_a = engine.build_memory(test_input("old fact"), now).unwrap();
        let id_a = engine.store_memory(mem_a, None).await.unwrap();

        let mem_b = engine.build_memory(test_input("new fact"), now).unwrap();
        engine.store_memory(mem_b, Some(&id_a)).await.unwrap();

        let results = engine.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();
        assert!(!results.iter().any(|m| m.id == id_a), "superseded memory should be hidden from list");
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn superseded_memory_visible_with_include_superseded() {
        let engine = make_engine();
        let now = engine.now();
        let mem_a = engine.build_memory(test_input("old fact"), now).unwrap();
        let id_a = engine.store_memory(mem_a, None).await.unwrap();

        let mem_b = engine.build_memory(test_input("new fact"), now).unwrap();
        engine.store_memory(mem_b, Some(&id_a)).await.unwrap();

        let filter = MemoryFilter {
            include_superseded: Some(true),
            ..Default::default()
        };
        let results = engine.list_memories(filter, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 2, "both memories should be visible with include_superseded=true");
    }

    // -- RR-120: consolidation engine-level tests ----------------------------

    #[tokio::test]
    async fn consolidate_rejects_negative_threshold() {
        let engine = make_engine();
        let err = engine.consolidate_memories("test-agent", None, -0.1_f64, 10, false).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)), "negative threshold should be rejected");
    }

    #[tokio::test]
    async fn consolidate_rejects_threshold_above_one() {
        let engine = make_engine();
        let err = engine.consolidate_memories("test-agent", None, 1.1_f64, 10, false).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)), "threshold > 1.0 should be rejected");
    }

    #[tokio::test]
    async fn consolidate_rejects_nan_threshold() {
        let engine = make_engine();
        let err = engine.consolidate_memories("test-agent", None, f64::NAN, 10, false).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)), "NaN threshold should be rejected");
    }

    #[tokio::test]
    async fn consolidate_rejects_infinite_threshold() {
        let engine = make_engine();
        let err = engine.consolidate_memories("test-agent", None, f64::INFINITY, 10, false).await.unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)), "infinite threshold should be rejected");
    }

    #[tokio::test]
    async fn consolidate_dry_run_does_not_merge() {
        let engine = make_engine();
        // With NoopEmbedding, no memories will have embeddings, so no groups will form.
        // The key test is that dry_run=true returns merged=false.
        let result = engine.consolidate_memories("test-agent", None, 0.9_f64, 10, true).await.unwrap();
        assert!(!result.merged, "dry_run should produce merged=false");
    }

    #[tokio::test]
    async fn consolidate_empty_store_returns_no_groups() {
        let engine = make_engine();
        let result = engine.consolidate_memories("test-agent", None, 0.9_f64, 10, false).await.unwrap();
        assert!(result.groups.is_empty(), "empty store should produce no groups");
        assert!(!result.merged, "empty store should not merge");
    }

    #[tokio::test]
    async fn consolidate_single_memory_returns_no_groups() {
        let engine = make_engine();
        let memory = engine.build_memory(test_input("single memory"), engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let result = engine.consolidate_memories("test-agent", None, 0.9_f64, 10, false).await.unwrap();
        // NoopEmbedding means no embeddings, so list_with_embeddings returns nothing.
        assert!(result.groups.is_empty());
    }

    #[tokio::test]
    async fn consolidate_threshold_boundary_zero_accepts() {
        let engine = make_engine();
        let result = engine.consolidate_memories("test-agent", None, 0.0_f64, 10, true).await.unwrap();
        assert!(!result.merged);
    }

    #[tokio::test]
    async fn consolidate_threshold_boundary_one_accepts() {
        let engine = make_engine();
        let result = engine.consolidate_memories("test-agent", None, 1.0_f64, 10, true).await.unwrap();
        assert!(!result.merged);
    }

    #[tokio::test]
    async fn consolidate_ignores_memories_caller_cannot_write() {
        let store = SqliteStore::in_memory().unwrap();
        let engine = make_engine_with_store(store.clone(), Arc::new(NoopEmbedding::new()));
        let mut caller_owned = engine.build_memory(test_input("caller-owned"), engine.now()).unwrap();
        caller_owned.provenance.source_agent = Some("caller".into());

        let mut hidden = engine.build_memory(test_input("hidden"), engine.now()).unwrap();
        hidden.provenance.source_agent = Some("other".into());
        hidden.access_policy = AccessPolicy::Restricted { allowed: vec!["other".into()] };

        let mut emb = vec![0.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
        emb[0] = 1.0;
        let caller_id = store.store(&caller_owned, Some(&emb)).await.unwrap();
        let hidden_id = store.store(&hidden, Some(&emb)).await.unwrap();

        let result = engine.consolidate_memories("caller", None, 0.9, 10, false).await.unwrap();
        assert!(result.groups.is_empty(), "non-writable memories should not participate in consolidation");
        assert!(!result.merged);

        let caller_after = store.get(&caller_id, Some("caller")).await.unwrap().unwrap();
        assert!(caller_after.superseded_by.is_none());
        let hidden_after = store.get(&hidden_id, Some("other")).await.unwrap().unwrap();
        assert!(hidden_after.superseded_by.is_none());
    }

    // -- Fix regression: tags are trimmed on storage (#4) ---------------------

    #[tokio::test]
    async fn build_memory_trims_tags() {
        let engine = make_engine();
        let mut input = test_input("tag trimming");
        input.tags = vec!["  spaced  ".into(), "clean".into()];
        let memory = engine.build_memory(input, engine.now()).unwrap();
        assert_eq!(memory.tags, vec!["spaced", "clean"]);
    }

    // -- Fix regression: prepare_update trims tags (#4) -----------------------

    #[tokio::test]
    async fn prepare_update_trims_tags() {
        let engine = make_engine();
        let mut update = MemoryUpdate {
            tags: Some(vec!["  padded  ".into()]),
            ..Default::default()
        };
        engine.prepare_update(&mut update).unwrap();
        assert_eq!(update.tags.as_deref(), Some(&["padded".to_owned()][..]));
    }

    // -- Fix regression: bulk_delete includes owner's restricted memories (#2) -

    #[tokio::test]
    async fn bulk_delete_includes_restricted_owned_by_caller() {
        let engine = make_engine();
        let mut input = test_input("restricted owned");
        input.source_agent = Some("owner-agent".into());
        input.access_policy = Some(AccessPolicy::Restricted { allowed: vec![] });
        let memory = engine.build_memory(input, engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let filter = MemoryFilter::default();
        let ctx = QueryContext {
            principal: Some("owner-agent".into()),
        };
        let result = engine.bulk_delete("owner-agent", filter, ctx).await.unwrap();
        assert_eq!(result.deleted, 1, "owner should be able to bulk-delete own restricted memory");
    }

    // -- Fix regression: bulk_update includes owner's restricted memories (#2) -

    #[tokio::test]
    async fn bulk_update_includes_restricted_owned_by_caller() {
        let engine = make_engine();
        let mut input = test_input("restricted owned");
        input.source_agent = Some("owner-agent".into());
        input.access_policy = Some(AccessPolicy::Restricted { allowed: vec![] });
        let memory = engine.build_memory(input, engine.now()).unwrap();
        engine.store_memory(memory, None).await.unwrap();

        let filter = MemoryFilter::default();
        let ctx = QueryContext {
            principal: Some("owner-agent".into()),
        };
        let fields = BulkUpdateFields {
            tags: Some(vec!["updated".into()]),
            importance: None,
            access_policy: None,
        };
        let result = engine.bulk_update("owner-agent", filter, ctx, fields).await.unwrap();
        assert_eq!(result.updated, 1, "owner should be able to bulk-update own restricted memory");
    }
}
