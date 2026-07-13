//! `PostgresStore` connection lifecycle, schema bootstrap, and backend-local CRUD helpers.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};
mod sqlx {
    pub(crate) use sqlx_core::{query::query, query_builder::QueryBuilder, query_scalar::query_scalar, row::Row, sql_str::AssertSqlSafe, transaction::Transaction, types};
}

use sqlx::{AssertSqlSafe, QueryBuilder, Row as _, Transaction, types::Json};
use sqlx_postgres::{PgPool, PgPoolOptions, PgRow, Postgres};

use super::{
    BulkAuthOutcome, EmbeddingMap, EmbeddingNeighbor, EmbeddingProfile, MemoryAdmin, MemoryReader, MemoryWithEmbedding, MemoryWriter, ReassignScopeOutcome, RecordUseOutcome,
    ReembedClaim, merge_metadata_patch,
    migration::reject_retired_postgres_schema,
    query::{
        DEFAULT_LIST_LIMIT, MAX_SCAN_ROWS, MAX_VEC_CANDIDATES, OVERFETCH_FACTOR, apply_access_policy_for_filter, escape_like, normalize_filter, sort_by_distance, usize_to_i64,
    },
    update_audit_draft_for_locked_memory,
    vector::{VectorBatch, VectorHit, validate_embedding_vector},
};
use crate::{
    clock::{Clock, SystemClock},
    config::PostgresDatabaseConfig,
    error::{ParseEnumError, StoreError},
    scoring::decay_mass,
    types::{
        AccessLevel, AccessPolicy, AuditAction, AuditDraft, AuditEntry, AuthorizedUpdateOutcome, Entity, LARGE_CONTENT_WARNING_THRESHOLD_BYTES, Memory, MemoryFilter, MemoryId,
        MemoryMetadata, MemoryStats, MemoryTombstone, MemoryType, MemoryUpdate, MetadataMigrationOutcome, MetadataMigrationReport, MetadataPatch, Provenance, QueryContext,
        ScopeDefinition, SearchResult, WriteOutcome,
    },
};

const CREATE_VECTOR_EXTENSION: &str = "CREATE EXTENSION IF NOT EXISTS vector";
const UNRESOLVED_SCOPE: &str = "inbox/unresolved";
const POSTGRES_COUNT_PAGE_SIZE: usize = 500;
const EMBEDDING_CLAIM_LEASE_SECS: i64 = 300;
const EMBEDDING_PROFILE_ADVISORY_LOCK: i64 = 5_499_250_768_369_920_844;

const CREATE_MIGRATIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS localhold_migrations (
        version    BIGINT PRIMARY KEY,
        name       TEXT NOT NULL UNIQUE,
        applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
    )
";

const POSTGRES_SCHEMA_STATEMENTS: &[&str] = &[
    "
    CREATE TABLE IF NOT EXISTS memories (
        id                 TEXT PRIMARY KEY,
        content            TEXT NOT NULL,
        tags               JSONB NOT NULL,
        provenance         JSONB NOT NULL,
        access_policy      JSONB NOT NULL,
        created_at         TIMESTAMPTZ NOT NULL,
        expires_at         TIMESTAMPTZ,
        has_embedding      BOOLEAN NOT NULL DEFAULT FALSE,
        embedding_revision BIGINT NOT NULL DEFAULT 0,
        memory_type        TEXT NOT NULL DEFAULT 'semantic',
        importance         DOUBLE PRECISION NOT NULL DEFAULT 0.5,
        impression_count   BIGINT NOT NULL DEFAULT 0,
        last_impressed_at  TIMESTAMPTZ,
        superseded_by      TEXT REFERENCES memories(id) ON DELETE SET NULL,
        activity_mass      DOUBLE PRECISION NOT NULL DEFAULT 0.0,
        last_used_at       TIMESTAMPTZ,
        updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        confidence         DOUBLE PRECISION NOT NULL DEFAULT 0.8,
        embedding_claimed_at TIMESTAMPTZ,
        embedding_claim_token TEXT
    )
    ",
    "CREATE INDEX IF NOT EXISTS idx_memories_created_at ON memories(created_at DESC)",
    "CREATE INDEX IF NOT EXISTS idx_memories_expires_at ON memories(expires_at) WHERE expires_at IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS idx_memories_has_embedding ON memories(has_embedding)",
    "CREATE INDEX IF NOT EXISTS idx_memories_memory_type ON memories(memory_type)",
    "CREATE INDEX IF NOT EXISTS idx_memories_superseded_by ON memories(superseded_by) WHERE superseded_by IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS idx_memories_tags_gin ON memories USING GIN (tags)",
    "CREATE INDEX IF NOT EXISTS idx_memories_source_agent ON memories ((provenance->>'source_agent'))",
    "CREATE INDEX IF NOT EXISTS idx_memories_source_conversation ON memories ((provenance->>'source_conversation'))",
    "CREATE INDEX IF NOT EXISTS idx_memories_origin_conversation ON memories ((provenance->>'origin_conversation'))",
    "CREATE INDEX IF NOT EXISTS idx_memories_effective_origin_conversation ON memories (COALESCE(provenance->>'origin_conversation', provenance->>'source_conversation'))",
    "CREATE INDEX IF NOT EXISTS idx_memories_access_type ON memories ((access_policy->>'type'))",
    "CREATE INDEX IF NOT EXISTS idx_memories_content_fts ON memories USING GIN (to_tsvector('simple', content))",
    "
    CREATE TABLE IF NOT EXISTS memory_entities (
        memory_id   TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
        entity      TEXT NOT NULL,
        entity_type TEXT NOT NULL,
        PRIMARY KEY (memory_id, entity, entity_type)
    )
    ",
    "CREATE INDEX IF NOT EXISTS idx_memory_entities_entity ON memory_entities(entity)",
    "CREATE INDEX IF NOT EXISTS idx_memory_entities_entity_type ON memory_entities(entity_type)",
    "
    CREATE TABLE IF NOT EXISTS memory_audit_log (
        id           BIGSERIAL PRIMARY KEY,
        memory_id    TEXT NOT NULL,
        action       TEXT NOT NULL,
        caller_agent TEXT,
        timestamp    TIMESTAMPTZ NOT NULL,
        details      JSONB
    )
    ",
    "CREATE INDEX IF NOT EXISTS idx_audit_log_memory_id ON memory_audit_log(memory_id)",
    "CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON memory_audit_log(timestamp DESC)",
    "
    CREATE TABLE IF NOT EXISTS memory_tombstones (
        memory_id            TEXT PRIMARY KEY,
        provenance           JSONB NOT NULL,
        access_policy        JSONB NOT NULL,
        deleted_at           TIMESTAMPTZ NOT NULL,
        deleted_by_principal TEXT
    )
    ",
    "CREATE INDEX IF NOT EXISTS idx_memory_tombstones_deleted_at ON memory_tombstones(deleted_at DESC)",
    "
    CREATE TABLE IF NOT EXISTS scope_registry (
        scope_key    TEXT PRIMARY KEY,
        display_name TEXT NOT NULL,
        description  TEXT,
        aliases      JSONB NOT NULL DEFAULT '[]'::jsonb,
        matchers     JSONB NOT NULL DEFAULT '[]'::jsonb,
        parent       TEXT,
        related      JSONB NOT NULL DEFAULT '[]'::jsonb,
        updated_at   TIMESTAMPTZ NOT NULL
    )
    ",
    "
    CREATE TABLE IF NOT EXISTS memory_metadata (
        memory_id            TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
        scope_key            TEXT,
        summary              TEXT,
        agent_label          TEXT,
        created_by_principal TEXT,
        quality_flags        JSONB NOT NULL DEFAULT '[]'::jsonb,
        schema_version       BIGINT NOT NULL DEFAULT 1,
        migrated_at          TIMESTAMPTZ,
        updated_at           TIMESTAMPTZ NOT NULL
    )
    ",
    "CREATE INDEX IF NOT EXISTS idx_memory_metadata_scope_key ON memory_metadata(scope_key)",
    "
    CREATE TABLE IF NOT EXISTS embedding_profile (
        singleton  SMALLINT PRIMARY KEY CHECK (singleton = 1),
        provider   TEXT NOT NULL,
        endpoint   TEXT NOT NULL,
        model      TEXT NOT NULL,
        dimensions BIGINT NOT NULL CHECK (dimensions > 0)
    )
    ",
];

const MEMORY_COLUMNS: &str = "
    id,
    content,
    tags,
    provenance,
    access_policy,
    created_at,
    expires_at,
    has_embedding,
    memory_type,
    importance,
    impression_count,
    last_impressed_at,
    superseded_by,
    activity_mass,
    last_used_at,
    updated_at,
    confidence
";

#[derive(Debug)]
struct PostgresInner {
    pool: PgPool,
    embedding_dimensions: usize,
    clock: Arc<dyn Clock>,
    active_embedding_profile: parking_lot::RwLock<Option<EmbeddingProfile>>,
}

/// `PostgreSQL`-backed memory store bootstrap.
#[derive(Clone, Debug)]
pub struct PostgresStore {
    inner: Arc<PostgresInner>,
}

impl PostgresStore {
    /// Latest schema migration recorded by this binary.
    pub const CURRENT_SCHEMA_VERSION: i64 = 2;

    /// Open a `PostgreSQL` connection pool and optionally initialize schema.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the pool cannot connect, migration DDL fails,
    /// or an existing vector table has incompatible dimensions.
    pub async fn open(config: &PostgresDatabaseConfig, embedding_dimensions: usize) -> Result<Self, StoreError> {
        Self::open_with_clock(config, embedding_dimensions, Arc::new(SystemClock::new())).await
    }

    async fn open_with_clock(config: &PostgresDatabaseConfig, embedding_dimensions: usize, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        validate_bootstrap_inputs(config, embedding_dimensions)?;
        let pool = PgPoolOptions::new().max_connections(config.max_connections).connect(&config.url).await?;
        reject_retired_postgres_schema(&pool, true).await?;
        if config.auto_migrate {
            init_schema(&pool, embedding_dimensions).await?;
        }
        Ok(Self {
            inner: Arc::new(PostgresInner {
                pool,
                embedding_dimensions,
                clock,
                active_embedding_profile: parking_lot::RwLock::new(None),
            }),
        })
    }

    /// Configured embedding dimensions for this store.
    #[must_use]
    pub fn embedding_dimensions(&self) -> usize {
        self.inner.embedding_dimensions
    }

    /// Underlying `PostgreSQL` connection pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    pub(crate) fn clock_now(&self) -> DateTime<Utc> {
        self.inner.clock.now()
    }

    /// Verify that configured embeddings belong to the database's vector space.
    ///
    /// # Errors
    ///
    /// Returns an error when database access fails or the configured profile
    /// does not match stored vectors.
    pub async fn verify_embedding_profile(&self, profile: &EmbeddingProfile) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        lock_embedding_profile(&mut tx).await?;
        if let Some(existing) = read_embedding_profile_tx(&mut tx).await? {
            if existing != *profile {
                return Err(profile_mismatch(&existing, profile));
            }
        } else {
            let vector_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_embeddings").fetch_one(&mut *tx).await?;
            if vector_count > 0 {
                return Err(StoreError::Conflict(
                    "existing embeddings have no recorded provider/model identity; run `hold embeddings reindex --yes` before starting with an active embedding provider".into(),
                ));
            }
            upsert_embedding_profile_executor(&mut tx, profile).await?;
        }
        tx.commit().await?;
        *self.inner.active_embedding_profile.write() = Some(profile.clone());
        Ok(())
    }

    /// Clear all vectors and stamp the configured vector-space identity.
    ///
    /// Memory content and metadata are preserved. The normal startup recovery
    /// worker rebuilds vectors after the server starts.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or its vector table
    /// cannot be reset atomically.
    pub async fn reindex_embeddings(config: &PostgresDatabaseConfig, profile: &EmbeddingProfile) -> Result<(), StoreError> {
        validate_bootstrap_inputs(config, profile.dimensions)?;
        let pool = PgPoolOptions::new().max_connections(config.max_connections).connect(&config.url).await?;
        execute_statement(&pool, CREATE_VECTOR_EXTENSION).await?;
        execute_statement(&pool, CREATE_MIGRATIONS_TABLE).await?;
        execute_statements(&pool, POSTGRES_SCHEMA_STATEMENTS).await?;
        migrate_embedding_claim_columns(&pool).await?;

        let mut tx = pool.begin().await?;
        lock_embedding_profile(&mut tx).await?;
        let _dropped = sqlx::query("DROP TABLE IF EXISTS memory_embeddings").execute(&mut *tx).await?;
        let _updated = sqlx::query("UPDATE memories SET has_embedding = FALSE, embedding_claimed_at = NULL, embedding_claim_token = NULL")
            .execute(&mut *tx)
            .await?;
        let ddl = memory_embeddings_ddl(profile.dimensions)?;
        let _created = sqlx::query(AssertSqlSafe(ddl)).execute(&mut *tx).await?;
        upsert_embedding_profile_executor(&mut tx, profile).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn store_impl(&self, memory: &Memory, embedding: Option<&[f32]>) -> Result<MemoryId, StoreError> {
        self.store_audited_impl(memory, embedding, None).await
    }

    pub(crate) async fn store_audited_impl(&self, memory: &Memory, embedding: Option<&[f32]>, audit: Option<&AuditDraft>) -> Result<MemoryId, StoreError> {
        if let Some(embedding) = embedding {
            validate_embedding_dimensions(embedding, self.embedding_dimensions())?;
        }

        let mut tx = self.pool().begin().await?;
        insert_memory_with_embedding(&mut tx, memory, embedding).await?;
        if let Some(audit) = audit {
            insert_audit_draft_tx(&mut tx, &memory.id, audit).await?;
        }
        tx.commit().await?;
        Ok(memory.id)
    }

    pub(crate) async fn store_with_supersession_impl(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId) -> Result<MemoryId, StoreError> {
        self.store_with_supersession_audited_impl(memory, embedding, supersedes_id, None).await
    }

    pub(crate) async fn store_with_supersession_audited_impl(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: &MemoryId,
        audit: Option<&AuditDraft>,
    ) -> Result<MemoryId, StoreError> {
        if let Some(embedding) = embedding {
            validate_embedding_dimensions(embedding, self.embedding_dimensions())?;
        }

        let mut tx = self.pool().begin().await?;
        validate_superseded_exists(&mut tx, supersedes_id).await?;
        insert_memory_with_embedding(&mut tx, memory, embedding).await?;
        let _marked = mark_superseded_tx(&mut tx, supersedes_id, &memory.id).await?;
        if let Some(audit) = audit {
            insert_audit_draft_tx(&mut tx, &memory.id, audit).await?;
        }
        tx.commit().await?;
        Ok(memory.id)
    }

    pub(crate) async fn store_with_metadata_impl(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
    ) -> Result<MemoryId, StoreError> {
        self.store_with_metadata_audited_impl(memory, embedding, supersedes_id, metadata, None).await
    }

    #[expect(clippy::too_many_arguments, reason = "audited store needs memory, embedding, supersession, metadata, and audit draft")]
    pub(crate) async fn store_with_metadata_audited_impl(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
        audit: Option<&AuditDraft>,
    ) -> Result<MemoryId, StoreError> {
        validate_metadata_memory_id(&memory.id, metadata)?;
        if let Some(embedding) = embedding {
            validate_embedding_dimensions(embedding, self.embedding_dimensions())?;
        }

        let mut tx = self.pool().begin().await?;
        if let Some(supersedes_id) = supersedes_id {
            validate_superseded_exists(&mut tx, supersedes_id).await?;
        }
        insert_memory_with_embedding(&mut tx, memory, embedding).await?;
        if let Some(supersedes_id) = supersedes_id {
            let _marked = mark_superseded_tx(&mut tx, supersedes_id, &memory.id).await?;
        }
        upsert_metadata_tx(&mut tx, metadata, self.clock_now()).await?;
        if let Some(audit) = audit {
            insert_audit_draft_tx(&mut tx, &memory.id, audit).await?;
        }
        tx.commit().await?;
        Ok(memory.id)
    }

    pub(crate) async fn store_batch_impl(&self, memories: &[MemoryWithEmbedding]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_audited_impl(memories, None).await
    }

    pub(crate) async fn store_batch_audited_impl(&self, memories: &[MemoryWithEmbedding], audits: Option<&[AuditDraft]>) -> Result<Vec<MemoryId>, StoreError> {
        if let Some(audits) = audits
            && audits.len() != memories.len()
        {
            return Err(audit_len_mismatch(memories.len(), audits.len()));
        }
        validate_batch_embedding_dimensions(memories, self.embedding_dimensions())?;

        let mut tx = self.pool().begin().await?;
        let mut ids = Vec::with_capacity(memories.len());
        for (idx, memory_with_embedding) in memories.iter().enumerate() {
            insert_memory_with_embedding(&mut tx, &memory_with_embedding.memory, memory_with_embedding.embedding.as_deref()).await?;
            if let Some(audits) = audits {
                let audit = audits.get(idx).ok_or_else(|| audit_len_mismatch(memories.len(), audits.len()))?;
                insert_audit_draft_tx(&mut tx, &memory_with_embedding.memory.id, audit).await?;
            }
            ids.push(memory_with_embedding.memory.id);
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub(crate) async fn store_batch_with_supersession_impl(&self, memories: &[MemoryWithEmbedding], supersedes: &[Option<MemoryId>]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_supersession_audited_impl(memories, supersedes, None).await
    }

    pub(crate) async fn store_batch_with_supersession_audited_impl(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        audits: Option<&[AuditDraft]>,
    ) -> Result<Vec<MemoryId>, StoreError> {
        if supersedes.len() != memories.len() {
            return Err(supersedes_len_mismatch(memories.len(), supersedes.len()));
        }
        if let Some(audits) = audits
            && audits.len() != memories.len()
        {
            return Err(audit_len_mismatch(memories.len(), audits.len()));
        }
        validate_batch_embedding_dimensions(memories, self.embedding_dimensions())?;

        let mut tx = self.pool().begin().await?;
        let mut ids = Vec::with_capacity(memories.len());
        for (idx, memory_with_embedding) in memories.iter().enumerate() {
            if let Some(supersedes_id) = supersedes.get(idx).and_then(|id| *id) {
                validate_superseded_exists(&mut tx, &supersedes_id).await?;
                insert_memory_with_embedding(&mut tx, &memory_with_embedding.memory, memory_with_embedding.embedding.as_deref()).await?;
                let _marked = mark_superseded_tx(&mut tx, &supersedes_id, &memory_with_embedding.memory.id).await?;
            } else {
                insert_memory_with_embedding(&mut tx, &memory_with_embedding.memory, memory_with_embedding.embedding.as_deref()).await?;
            }
            if let Some(audits) = audits {
                let audit = audits.get(idx).ok_or_else(|| audit_len_mismatch(memories.len(), audits.len()))?;
                insert_audit_draft_tx(&mut tx, &memory_with_embedding.memory.id, audit).await?;
            }
            ids.push(memory_with_embedding.memory.id);
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub(crate) async fn store_batch_with_metadata_impl(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
    ) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_metadata_audited_impl(memories, supersedes, metadata, None).await
    }

    pub(crate) async fn store_batch_with_metadata_audited_impl(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
        audits: Option<&[AuditDraft]>,
    ) -> Result<Vec<MemoryId>, StoreError> {
        if metadata.len() != memories.len() {
            return Err(metadata_len_mismatch(memories.len(), metadata.len()));
        }
        if supersedes.len() != memories.len() {
            return Err(supersedes_len_mismatch(memories.len(), supersedes.len()));
        }
        if let Some(audits) = audits
            && audits.len() != memories.len()
        {
            return Err(audit_len_mismatch(memories.len(), audits.len()));
        }
        for (memory_with_embedding, item_metadata) in memories.iter().zip(metadata) {
            validate_metadata_memory_id(&memory_with_embedding.memory.id, item_metadata)?;
        }
        validate_batch_embedding_dimensions(memories, self.embedding_dimensions())?;

        let mut tx = self.pool().begin().await?;
        let now = self.clock_now();
        let mut ids = Vec::with_capacity(memories.len());
        for (idx, memory_with_embedding) in memories.iter().enumerate() {
            if let Some(supersedes_id) = supersedes.get(idx).and_then(|id| *id) {
                validate_superseded_exists(&mut tx, &supersedes_id).await?;
                insert_memory_with_embedding(&mut tx, &memory_with_embedding.memory, memory_with_embedding.embedding.as_deref()).await?;
                let _marked = mark_superseded_tx(&mut tx, &supersedes_id, &memory_with_embedding.memory.id).await?;
            } else {
                insert_memory_with_embedding(&mut tx, &memory_with_embedding.memory, memory_with_embedding.embedding.as_deref()).await?;
            }
            let item_metadata = metadata.get(idx).ok_or_else(|| metadata_len_mismatch(memories.len(), metadata.len()))?;
            upsert_metadata_tx(&mut tx, item_metadata, now).await?;
            if let Some(audits) = audits {
                let audit = audits.get(idx).ok_or_else(|| audit_len_mismatch(memories.len(), audits.len()))?;
                insert_audit_draft_tx(&mut tx, &memory_with_embedding.memory.id, audit).await?;
            }
            ids.push(memory_with_embedding.memory.id);
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub(crate) async fn get_impl(&self, id: &MemoryId, principal: Option<&str>) -> Result<Option<Memory>, StoreError> {
        let Some(mut memory) = fetch_memory_by_id(self.pool(), id).await? else {
            return Ok(None);
        };
        if memory.expires_at.is_some_and(|expires_at| self.clock_now() >= expires_at) {
            return Ok(None);
        }
        memory.entities = fetch_entities(self.pool(), id).await?;
        Ok(memory.apply_access_policy(principal))
    }

    pub(crate) async fn list_impl(&self, filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<Memory>, StoreError> {
        let limit = filter.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 {
            return Ok(Vec::new());
        }

        let filter = normalize_filter(filter);
        let now = self.clock_now();
        let caller = ctx.principal;
        let mut results = Vec::with_capacity(limit);
        let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);
        let mut offset = 0_usize;
        let visible_ctx = PostgresVisibleRowsContext {
            filter: &filter,
            caller: caller.as_deref(),
            now,
        };

        loop {
            let page = PostgresFilterPage {
                filter: &filter,
                caller: caller.as_deref(),
                now,
                page_size,
                offset,
            };
            let rows = fetch_filtered_memory_rows(self.pool(), &page).await?;
            let row_count = rows.len();
            if append_visible_memory_rows(self.pool(), rows, &visible_ctx, limit, &mut results).await? {
                return Ok(results);
            }

            if row_count < page_size {
                break;
            }
            offset = offset.saturating_add(page_size);
        }
        Ok(results)
    }

    pub(crate) async fn count_impl(&self, filter: MemoryFilter, ctx: QueryContext, top_tags_limit: usize) -> Result<MemoryStats, StoreError> {
        let filter = normalize_filter(filter);
        let now = self.clock_now();
        let caller = ctx.principal;

        let mut offset = 0_usize;
        let mut stats = PostgresStatsAccumulator::default();
        let visible_ctx = PostgresVisibleRowsContext {
            filter: &filter,
            caller: caller.as_deref(),
            now,
        };

        loop {
            let page = PostgresFilterPage {
                filter: &filter,
                caller: caller.as_deref(),
                now,
                page_size: POSTGRES_COUNT_PAGE_SIZE,
                offset,
            };
            let rows = fetch_filtered_memory_rows(self.pool(), &page).await?;
            let row_count = rows.len();
            record_visible_memory_rows(self.pool(), rows, &visible_ctx, &mut stats).await?;

            if row_count < POSTGRES_COUNT_PAGE_SIZE {
                break;
            }
            offset = offset.saturating_add(POSTGRES_COUNT_PAGE_SIZE);
        }

        let expired_raw = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memories WHERE expires_at IS NOT NULL AND expires_at <= $1")
            .bind(now)
            .fetch_one(self.pool())
            .await?;
        let storage_raw = sqlx::query_scalar::<_, i64>("SELECT pg_database_size(current_database())::bigint")
            .fetch_one(self.pool())
            .await?;
        let PostgresStatsAccumulator {
            total,
            with_embedding,
            tag_counts,
            agent_counts,
            memory_type_counts,
            oldest,
            newest,
            scope_set,
            superseded_count,
        } = stats;
        let mut by_tag = tag_counts.into_iter().collect::<Vec<_>>();
        by_tag.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        by_tag.truncate(top_tags_limit);
        let mut by_agent_label = agent_counts.into_iter().collect::<Vec<_>>();
        by_agent_label.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut by_memory_type = memory_type_counts.into_iter().collect::<Vec<_>>();
        by_memory_type.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        Ok(MemoryStats {
            total,
            with_embedding,
            without_embedding: total.saturating_sub(with_embedding),
            expired: nonnegative_i64_to_u64(expired_raw)?,
            by_tag,
            by_agent_label,
            storage_bytes: Some(nonnegative_i64_to_u64(storage_raw)?),
            oldest_memory: oldest,
            newest_memory: newest,
            scope_count: u64::try_from(scope_set.len()).unwrap_or(u64::MAX),
            by_memory_type,
            superseded_count,
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "semantic search requires embedding, limit, filter, caller context, and max-distance threshold — all semantically distinct"
    )]
    pub(crate) async fn search_by_embedding_impl(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: MemoryFilter,
        ctx: QueryContext,
        max_distance: Option<f64>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        validate_embedding_dimensions(embedding, self.embedding_dimensions())?;

        let embedding = embedding.to_vec();
        let filter = normalize_filter(filter);
        let now = self.clock_now();
        let caller = ctx.principal;
        let mut results = Vec::with_capacity(limit);
        let mut seen_ids = HashSet::with_capacity(limit.saturating_mul(OVERFETCH_FACTOR));
        let mut fetch_size = limit.saturating_mul(OVERFETCH_FACTOR);
        let search_ctx = PostgresEmbeddingSearchContext {
            filter: &filter,
            caller: caller.as_deref(),
            now,
            limit,
            max_distance,
        };

        loop {
            let candidate_limit = fetch_size.min(MAX_VEC_CANDIDATES);
            let batch = search_vector_batch(self.pool(), &embedding, candidate_limit).await?;
            let returned = batch.returned_count;
            let new_hits = batch.hits.into_iter().filter(|hit| seen_ids.insert(hit.memory_id)).collect::<Vec<_>>();
            collect_vector_results(self.pool(), new_hits, &search_ctx, &mut results).await?;

            if results.len() >= limit || returned < fetch_size || fetch_size >= MAX_VEC_CANDIDATES {
                break;
            }
            fetch_size = fetch_size.saturating_mul(2);
        }

        sort_by_distance(&mut results);
        results.truncate(limit);
        Ok(results)
    }

    pub(crate) async fn search_by_text_impl(&self, query: &str, limit: usize, filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<SearchResult>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let like_pattern = format!("%{}%", escape_like(query));
        let filter = normalize_filter(filter);
        let now = self.clock_now();
        let caller = ctx.principal;
        let search_ctx = PostgresSearchContext {
            filter: &filter,
            caller: caller.as_deref(),
            now,
            limit,
            rank_column: None,
        };
        let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);
        let mut results = Vec::with_capacity(limit);
        let mut offset = 0_usize;

        loop {
            let page = PostgresFilterPage {
                filter: &filter,
                caller: caller.as_deref(),
                now,
                page_size,
                offset,
            };
            let rows = fetch_text_search_rows(self.pool(), &like_pattern, &page).await?;
            let row_count = rows.len();
            append_search_rows_to_results(self.pool(), rows, &search_ctx, &mut results).await?;
            if results.len() >= limit || row_count < page_size {
                break;
            }
            offset = offset.saturating_add(page_size);
            if offset >= MAX_SCAN_ROWS {
                break;
            }
        }

        Ok(results)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "FTS search requires query, limit, filter, caller context, and optional search context — all semantically distinct"
    )]
    pub(crate) async fn search_by_fts_impl(
        &self,
        query: &str,
        limit: usize,
        filter: MemoryFilter,
        ctx: QueryContext,
        _context: Option<&str>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if query.split_whitespace().next().is_none() {
            return self.search_by_text_impl(query, limit, filter, ctx).await;
        }

        let filter = normalize_filter(filter);
        let now = self.clock_now();
        let caller = ctx.principal;
        let search_ctx = PostgresSearchContext {
            filter: &filter,
            caller: caller.as_deref(),
            now,
            limit,
            rank_column: Some("rank"),
        };
        let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);
        let page_limit = usize_to_i64(page_size, "PostgreSQL FTS page size")?;
        let mut results = Vec::with_capacity(limit);
        let mut offset = 0_usize;

        loop {
            let page_offset = usize_to_i64(offset, "PostgreSQL FTS offset")?;
            let rows = sqlx::query(AssertSqlSafe(format!(
                "
                WITH q AS (SELECT websearch_to_tsquery('simple', $1) AS tsq)
                SELECT {MEMORY_COLUMNS}, ts_rank_cd(to_tsvector('simple', content), q.tsq)::double precision AS rank
                FROM memories, q
                WHERE q.tsq @@ to_tsvector('simple', content)
                ORDER BY rank DESC, created_at DESC, id DESC
                LIMIT $2 OFFSET $3
                "
            )))
            .bind(query)
            .bind(page_limit)
            .bind(page_offset)
            .fetch_all(self.pool())
            .await?;

            let row_count = rows.len();
            append_search_rows_to_results(self.pool(), rows, &search_ctx, &mut results).await?;
            if results.len() >= limit || row_count < page_size {
                break;
            }
            offset = offset.saturating_add(page_size);
            if offset >= MAX_SCAN_ROWS {
                break;
            }
        }

        Ok(results)
    }

    pub(crate) async fn list_for_reembed_impl(&self, limit: usize) -> Result<Vec<(MemoryId, String, i64)>, StoreError> {
        let limit = usize_to_i64(limit, "reembed limit")?;
        let rows = sqlx::query(
            "
            SELECT id, content, embedding_revision
            FROM memories
            WHERE has_embedding = FALSE
            ORDER BY created_at ASC, id ASC
            LIMIT $1
            ",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let id_str: String = row.try_get("id")?;
                Ok((parse_memory_id(&id_str, "id")?, row.try_get("content")?, row.try_get("embedding_revision")?))
            })
            .collect()
    }

    pub(crate) async fn claim_for_reembed_impl(&self, limit: usize) -> Result<Vec<ReembedClaim>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit_i64 = usize_to_i64(limit, "reembed limit")?;
        let now = self.clock_now();
        let expired_before = now
            .checked_sub_signed(chrono::Duration::seconds(EMBEDDING_CLAIM_LEASE_SECS))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        let claim_token = MemoryId::new().to_string();
        let mut tx = self.pool().begin().await?;
        let rows = sqlx::query(
            "
            SELECT id, content, embedding_revision
            FROM memories
            WHERE has_embedding = FALSE
              AND (embedding_claimed_at IS NULL OR embedding_claimed_at <= $1)
            ORDER BY created_at ASC, id ASC
            LIMIT $2
            FOR UPDATE SKIP LOCKED
            ",
        )
        .bind(expired_before)
        .bind(limit_i64)
        .fetch_all(&mut *tx)
        .await?;

        let mut claims = Vec::with_capacity(rows.len());
        for row in rows {
            let id_str: String = row.try_get("id")?;
            let content: String = row.try_get("content")?;
            let embedding_revision: i64 = row.try_get("embedding_revision")?;
            let result = sqlx::query(
                "
                UPDATE memories
                SET embedding_claimed_at = $1,
                    embedding_claim_token = $2
                WHERE id = $3
                  AND has_embedding = FALSE
                  AND embedding_revision = $4
                  AND (embedding_claimed_at IS NULL OR embedding_claimed_at <= $5)
                ",
            )
            .bind(now)
            .bind(&claim_token)
            .bind(&id_str)
            .bind(embedding_revision)
            .bind(expired_before)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() == 0 {
                continue;
            }
            claims.push(ReembedClaim {
                id: parse_memory_id(&id_str, "id")?,
                content,
                embedding_revision,
                claim_token: claim_token.clone(),
            });
        }
        tx.commit().await?;
        Ok(claims)
    }

    pub(crate) async fn release_embedding_claim_impl(&self, id: &MemoryId, expected_revision: i64, claim_token: &str) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "
            UPDATE memories
            SET embedding_claimed_at = NULL,
                embedding_claim_token = NULL
            WHERE id = $1
              AND has_embedding = FALSE
              AND embedding_revision = $2
              AND embedding_claim_token = $3
            ",
        )
        .bind(id.to_string())
        .bind(expected_revision)
        .bind(claim_token)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn get_for_reembed_impl(&self, id: &MemoryId, principal: &str) -> Result<Option<(String, i64)>, StoreError> {
        let Some(existing) = fetch_memory_by_id(self.pool(), id).await? else {
            return Ok(None);
        };
        if !existing.has_write_access(principal) {
            return Ok(None);
        }
        let revision: i64 = sqlx::query_scalar("SELECT embedding_revision FROM memories WHERE id = $1")
            .bind(id.to_string())
            .fetch_one(self.pool())
            .await?;
        Ok(Some((existing.content, revision)))
    }

    pub(crate) async fn record_search_impression_impl(&self, ids: &[MemoryId]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = self.clock_now();
        let mut tx = self.pool().begin().await?;
        for id in ids {
            let _result = sqlx::query(
                "
                UPDATE memories
                SET impression_count = impression_count + 1,
                    last_impressed_at = $1
                WHERE id = $2
                ",
            )
            .bind(now)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    #[expect(clippy::too_many_arguments, reason = "ids + principal + weight + now + half_life are all semantically distinct")]
    #[expect(clippy::float_arithmetic, reason = "decayed mass + event weight is the core activity update formula")]
    pub(crate) async fn record_memory_use_impl(
        &self,
        ids: &[MemoryId],
        principal: &str,
        event_weight: f64,
        now: DateTime<Utc>,
        activity_half_life_hours: f64,
    ) -> Result<RecordUseOutcome, StoreError> {
        if ids.is_empty() {
            return Ok(RecordUseOutcome::default());
        }

        let mut seen = HashSet::new();
        let ids = ids.iter().filter(|id| seen.insert(**id)).copied().collect::<Vec<_>>();
        let mut tx = self.pool().begin().await?;
        let mut outcome = RecordUseOutcome::default();
        for id in ids {
            let Some(memory) = fetch_memory_by_id_for_update_tx(&mut tx, &id).await? else {
                outcome.not_found = outcome.not_found.saturating_add(1);
                continue;
            };
            if memory.expires_at.is_some_and(|expires_at| now >= expires_at) || memory.check_access_level(Some(principal)) != AccessLevel::Full {
                outcome.denied = outcome.denied.saturating_add(1);
                continue;
            }

            let decayed = decay_mass(memory.activity_mass, memory.last_used_at, now, activity_half_life_hours);
            let new_mass = decayed + event_weight;
            let result = sqlx::query("UPDATE memories SET activity_mass = $1, last_used_at = $2 WHERE id = $3")
                .bind(new_mass)
                .bind(now)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await?;
            if result.rows_affected() > 0 {
                outcome.recorded = outcome.recorded.saturating_add(1);
            } else {
                outcome.not_found = outcome.not_found.saturating_add(1);
            }
        }
        tx.commit().await?;
        Ok(outcome)
    }

    pub(crate) async fn fetch_embeddings_for_ids_impl(&self, ids: &[MemoryId]) -> Result<EmbeddingMap, StoreError> {
        fetch_embeddings_for_ids(self.pool(), ids).await
    }

    pub(crate) async fn list_with_embeddings_impl(&self, scopes_any: Option<&[String]>, limit: usize) -> Result<Vec<MemoryWithEmbedding>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = usize_to_i64(limit, "list-with-embeddings limit")?;
        let rows = if let Some(scope_keys) = scopes_any.filter(|keys| !keys.is_empty()) {
            sqlx::query(AssertSqlSafe(format!(
                "
                SELECT {MEMORY_COLUMNS}
                FROM memories
                WHERE has_embedding = TRUE
                  AND superseded_by IS NULL
                  AND provenance->>'source_conversation' = ANY($1)
                ORDER BY created_at DESC, id DESC
                LIMIT $2
                "
            )))
            .bind(scope_keys.to_vec())
            .bind(limit)
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query(AssertSqlSafe(format!(
                "
                SELECT {MEMORY_COLUMNS}
                FROM memories
                WHERE has_embedding = TRUE
                  AND superseded_by IS NULL
                ORDER BY created_at DESC, id DESC
                LIMIT $1
                "
            )))
            .bind(limit)
            .fetch_all(self.pool())
            .await?
        };
        let memories = rows.iter().map(row_to_memory).collect::<Result<Vec<_>, _>>()?;
        let ids = memories.iter().map(|memory| memory.id).collect::<Vec<_>>();
        let mut embeddings = fetch_embeddings_for_ids(self.pool(), &ids).await?;
        let mut results = Vec::with_capacity(memories.len());
        for memory in memories {
            if let Some(embedding) = embeddings.remove(&memory.id) {
                results.push(MemoryWithEmbedding {
                    memory,
                    embedding: Some(embedding),
                });
            } else {
                tracing::warn!(memory_id = %memory.id, "memory has has_embedding=true but no PostgreSQL vector row");
            }
        }
        Ok(results)
    }

    pub(crate) async fn find_embedding_neighbors_impl(&self, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<EmbeddingNeighbor>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        validate_embedding_dimensions(embedding, self.embedding_dimensions())?;
        let vector = pgvector_literal(embedding);
        let limit = usize_to_i64(limit, "neighbor limit")?;
        let rows = sqlx::query(
            "
            SELECT e.memory_id, (e.embedding <-> $1::vector)::double precision AS distance
            FROM memory_embeddings AS e
            JOIN memories AS m ON m.id = e.memory_id
            WHERE m.superseded_by IS NULL
              AND (e.embedding <-> $1::vector) <= $2
            ORDER BY e.embedding <-> $1::vector
            LIMIT $3
            ",
        )
        .bind(vector)
        .bind(max_l2_distance)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().filter_map(row_to_vector_hit).map(|hit| (hit.memory_id, hit.distance)).collect())
    }

    pub(crate) async fn delete_impl(&self, id: &MemoryId) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, id).await? else {
            tx.commit().await?;
            return Ok(false);
        };
        insert_tombstone_tx(&mut tx, &existing, self.clock_now(), None).await?;
        let deleted = delete_memory_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(deleted)
    }

    pub(crate) async fn evict_expired_impl(&self) -> Result<u64, StoreError> {
        let now = self.clock_now();
        let mut tx = self.pool().begin().await?;
        let rows = sqlx::query(
            "
            SELECT id
            FROM memories
            WHERE expires_at IS NOT NULL AND expires_at <= $1
            ORDER BY expires_at ASC, id ASC
            FOR UPDATE
            ",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;
        let mut deleted = 0_u64;
        for row in rows {
            let id_str: String = row.try_get("id")?;
            let id = parse_memory_id(&id_str, "id")?;
            let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, &id).await? else {
                continue;
            };
            insert_tombstone_tx(&mut tx, &existing, now, None).await?;
            if delete_memory_tx(&mut tx, &id).await? {
                deleted = deleted.saturating_add(1);
            }
        }
        tx.commit().await?;
        Ok(deleted)
    }

    pub(crate) async fn update_impl(&self, id: &MemoryId, update: &MemoryUpdate) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let outcome = apply_update_tx(&mut tx, id, update, self.clock_now()).await?;
        tx.commit().await?;
        Ok(outcome.outcome == WriteOutcome::Applied)
    }

    pub(crate) async fn set_embedding_impl(&self, id: &MemoryId, embedding: &[f32], expected_revision: i64) -> Result<(), StoreError> {
        validate_embedding_dimensions(embedding, self.embedding_dimensions())?;

        let mut tx = self.pool().begin().await?;
        let active_profile = self.inner.active_embedding_profile.read().clone();
        if let Some(profile) = active_profile {
            ensure_embedding_profile_matches_tx(&mut tx, &profile).await?;
        }
        let current_revision: Option<i64> = sqlx::query_scalar("SELECT embedding_revision FROM memories WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
        let Some(current_revision) = current_revision else {
            return Err(StoreError::NotFound(format!("memory not found: {id}")));
        };
        if current_revision != expected_revision {
            return Err(StoreError::Conflict(format!(
                "embedding revision mismatch for {id}: expected {expected_revision}, current {current_revision}"
            )));
        }

        insert_embedding(&mut tx, id, embedding).await?;
        let result = sqlx::query(
            "UPDATE memories
             SET has_embedding = TRUE,
                 embedding_claimed_at = NULL,
                 embedding_claim_token = NULL
             WHERE id = $1 AND embedding_revision = $2",
        )
        .bind(id.to_string())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Conflict(format!("embedding revision changed while writing embedding for {id}")));
        }

        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn update_authorized_impl(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.update_authorized_audited_impl(id, update, principal, None).await
    }

    pub(crate) async fn update_authorized_audited_impl(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        let mut tx = self.pool().begin().await?;
        let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, id).await? else {
            tx.commit().await?;
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::NotFound,
                reembed_revision: None,
            });
        };
        if !existing.has_write_access(principal) {
            tx.commit().await?;
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::Denied,
                reembed_revision: None,
            });
        }

        let outcome = apply_update_tx(&mut tx, id, update, self.clock_now()).await?;
        if outcome.outcome == WriteOutcome::Applied
            && let Some(audit) = audit
        {
            let audit = update_audit_draft_for_locked_memory(audit, update, &existing);
            insert_audit_draft_tx(&mut tx, &existing.id, &audit).await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }

    #[expect(clippy::too_many_arguments, reason = "audited revise needs id, update, metadata patch, principal, and audit draft")]
    pub(crate) async fn update_authorized_with_metadata_audited_impl(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        let mut tx = self.pool().begin().await?;
        let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, id).await? else {
            tx.commit().await?;
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::NotFound,
                reembed_revision: None,
            });
        };
        if !existing.has_write_access(principal) {
            tx.commit().await?;
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::Denied,
                reembed_revision: None,
            });
        }

        let outcome = apply_update_tx(&mut tx, id, update, self.clock_now()).await?;
        if outcome.outcome == WriteOutcome::Applied {
            if let Some(patch) = metadata_patch {
                let existing_metadata = get_metadata_tx(&mut tx, id).await?;
                let metadata = merge_metadata_patch(*id, patch, existing_metadata.as_ref(), existing.provenance.source_conversation.as_deref(), principal);
                upsert_metadata_tx(&mut tx, &metadata, self.clock_now()).await?;
            }
            if let Some(audit) = audit {
                let audit = update_audit_draft_for_locked_memory(audit, update, &existing);
                insert_audit_draft_tx(&mut tx, &existing.id, &audit).await?;
            }
        }
        tx.commit().await?;
        Ok(outcome)
    }

    pub(crate) async fn delete_authorized_impl(&self, id: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.delete_authorized_audited_impl(id, principal, None).await
    }

    pub(crate) async fn delete_authorized_audited_impl(&self, id: &MemoryId, principal: &str, audit: Option<&AuditDraft>) -> Result<WriteOutcome, StoreError> {
        let mut tx = self.pool().begin().await?;
        let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, id).await? else {
            tx.commit().await?;
            return Ok(WriteOutcome::NotFound);
        };
        if !existing.has_write_access(principal) {
            tx.commit().await?;
            return Ok(WriteOutcome::Denied);
        }

        insert_tombstone_tx(&mut tx, &existing, self.clock_now(), Some(principal)).await?;
        let _deleted = delete_memory_tx(&mut tx, id).await?;
        if let Some(audit) = audit {
            insert_audit_draft_tx(&mut tx, id, audit).await?;
        }
        tx.commit().await?;
        Ok(WriteOutcome::Applied)
    }

    pub(crate) async fn bulk_delete_ids_impl(&self, ids: Vec<MemoryId>, principal: &str) -> Result<BulkAuthOutcome, StoreError> {
        self.bulk_delete_ids_audited_impl(ids, principal, None).await
    }

    pub(crate) async fn bulk_delete_ids_audited_impl(&self, ids: Vec<MemoryId>, principal: &str, audit: Option<&AuditDraft>) -> Result<BulkAuthOutcome, StoreError> {
        if ids.is_empty() {
            return Ok(BulkAuthOutcome {
                applied_ids: Vec::new(),
                denied: 0,
            });
        }

        let mut tx = self.pool().begin().await?;
        let mut applied_ids = Vec::new();
        let mut denied = 0_u64;
        for id in ids {
            let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, &id).await? else {
                continue;
            };
            if !existing.has_write_access(principal) {
                denied = denied.saturating_add(1);
                continue;
            }
            insert_tombstone_tx(&mut tx, &existing, self.clock_now(), Some(principal)).await?;
            if delete_memory_tx(&mut tx, &id).await? {
                insert_optional_audit_draft_tx(&mut tx, &id, audit).await?;
                applied_ids.push(id);
            }
        }
        tx.commit().await?;
        Ok(BulkAuthOutcome { applied_ids, denied })
    }

    pub(crate) async fn bulk_update_ids_impl(&self, ids: Vec<MemoryId>, update: MemoryUpdate, principal: &str, now: DateTime<Utc>) -> Result<BulkAuthOutcome, StoreError> {
        self.bulk_update_ids_audited_impl(ids, update, principal, now, None).await
    }

    #[expect(clippy::too_many_arguments, reason = "audited bulk update needs ids, update, principal, timestamp, and audit draft")]
    pub(crate) async fn bulk_update_ids_audited_impl(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: DateTime<Utc>,
        audit: Option<&AuditDraft>,
    ) -> Result<BulkAuthOutcome, StoreError> {
        if ids.is_empty() {
            return Ok(BulkAuthOutcome {
                applied_ids: Vec::new(),
                denied: 0,
            });
        }

        let mut tx = self.pool().begin().await?;
        let mut applied_ids = Vec::new();
        let mut denied = 0_u64;
        for id in ids {
            let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, &id).await? else {
                continue;
            };
            if !existing.has_write_access(principal) {
                denied = denied.saturating_add(1);
                continue;
            }
            let outcome = apply_update_tx(&mut tx, &id, &update, now).await?;
            if outcome.outcome == WriteOutcome::Applied {
                insert_optional_audit_draft_tx(&mut tx, &id, audit).await?;
                applied_ids.push(id);
            }
        }
        tx.commit().await?;
        Ok(BulkAuthOutcome { applied_ids, denied })
    }

    pub(crate) async fn mark_superseded_by_impl(&self, id: &MemoryId, superseded_by: &MemoryId) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let marked = mark_superseded_tx(&mut tx, id, superseded_by).await?;
        tx.commit().await?;
        Ok(marked)
    }

    pub(crate) async fn mark_superseded_by_authorized_impl(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.mark_superseded_by_authorized_audited_impl(id, superseded_by, principal, None).await
    }

    pub(crate) async fn mark_superseded_by_authorized_audited_impl(
        &self,
        id: &MemoryId,
        superseded_by: &MemoryId,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<WriteOutcome, StoreError> {
        let mut tx = self.pool().begin().await?;
        let Some(existing) = fetch_memory_by_id_for_update_tx(&mut tx, id).await? else {
            tx.commit().await?;
            return Ok(WriteOutcome::NotFound);
        };
        if !existing.has_write_access(principal) {
            tx.commit().await?;
            return Ok(WriteOutcome::Denied);
        }

        let marked = mark_superseded_tx(&mut tx, id, superseded_by).await?;
        if marked && let Some(audit) = audit {
            insert_audit_draft_tx(&mut tx, id, audit).await?;
        }
        tx.commit().await?;
        Ok(if marked { WriteOutcome::Applied } else { WriteOutcome::NotFound })
    }

    pub(crate) async fn reassign_scope_impl(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
    ) -> Result<ReassignScopeOutcome, StoreError> {
        self.reassign_scope_audited_impl(from_scope, to_scope, origin_conversation, principal, None).await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "scope reassignment keeps selection, authorization, metadata, and audit update in one transaction"
    )]
    #[expect(clippy::too_many_arguments, reason = "audited reassign needs scope pair, optional origin, principal, and audit draft")]
    pub(crate) async fn reassign_scope_audited_impl(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
        audit: Option<&AuditDraft>,
    ) -> Result<ReassignScopeOutcome, StoreError> {
        let mut tx = self.pool().begin().await?;
        let rows = if let Some(origin) = origin_conversation {
            sqlx::query(
                "
                SELECT id
                FROM memories
                WHERE provenance->>'source_conversation' = $1
                  AND COALESCE(provenance->>'origin_conversation', provenance->>'source_conversation') = $2
                ORDER BY created_at ASC, id ASC
                ",
            )
            .bind(from_scope)
            .bind(origin)
            .fetch_all(&mut *tx)
            .await?
        } else {
            sqlx::query(
                "
                SELECT id
                FROM memories
                WHERE provenance->>'source_conversation' = $1
                ORDER BY created_at ASC, id ASC
                ",
            )
            .bind(from_scope)
            .fetch_all(&mut *tx)
            .await?
        };

        let mut applied_ids = Vec::new();
        for row in rows {
            let id_str: String = row.try_get("id")?;
            let id = parse_memory_id(&id_str, "id")?;
            let Some(memory) = fetch_memory_by_id_for_update_tx(&mut tx, &id).await? else {
                continue;
            };
            if !memory.has_write_access(principal) {
                continue;
            }

            let _result = sqlx::query(
                "
                UPDATE memories
                SET provenance = jsonb_set(
                        jsonb_set(
                            provenance,
                            ARRAY['origin_conversation'],
                            to_jsonb(COALESCE(provenance->>'origin_conversation', provenance->>'source_conversation')),
                            true
                        ),
                        ARRAY['source_conversation'],
                        to_jsonb($1::text),
                        true
                    ),
                    updated_at = $2
                WHERE id = $3
                ",
            )
            .bind(to_scope)
            .bind(self.clock_now())
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
            let _metadata = sqlx::query(
                "
                UPDATE memory_metadata
                SET scope_key = $1, updated_at = $2
                WHERE memory_id = $3
                ",
            )
            .bind(to_scope)
            .bind(self.clock_now())
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
            if let Some(audit) = audit {
                insert_audit_draft_tx(&mut tx, &id, audit).await?;
            }
            applied_ids.push(id);
        }
        tx.commit().await?;
        Ok(ReassignScopeOutcome { applied_ids })
    }

    pub(crate) async fn write_audit_entry_impl(&self, memory_id: &MemoryId, entry: &AuditEntry) -> Result<(), StoreError> {
        let _result = sqlx::query(
            "
            INSERT INTO memory_audit_log (memory_id, action, caller_agent, timestamp, details)
            VALUES ($1, $2, $3, $4, $5)
            ",
        )
        .bind(memory_id.to_string())
        .bind(entry.action.to_string())
        .bind(entry.caller_agent.clone())
        .bind(entry.timestamp)
        .bind(entry.details.clone().map(Json))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub(crate) async fn query_audit_log_impl(&self, memory_id: &MemoryId, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        let limit = usize_to_i64(limit, "audit log limit")?;
        let rows = sqlx::query(
            "
            SELECT action, caller_agent, timestamp, details
            FROM memory_audit_log
            WHERE memory_id = $1
            ORDER BY id ASC
            LIMIT $2
            ",
        )
        .bind(memory_id.to_string())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let action: String = row.try_get("action")?;
                let details: Option<Json<serde_json::Value>> = row.try_get("details")?;
                Ok(AuditEntry {
                    action: action.parse().map_err(|e: ParseEnumError| StoreError::Serialization(Box::new(e)))?,
                    caller_agent: row.try_get("caller_agent")?,
                    timestamp: row.try_get("timestamp")?,
                    details: details.map(|value| value.0),
                })
            })
            .collect()
    }

    pub(crate) async fn get_tombstone_impl(&self, memory_id: &MemoryId) -> Result<Option<MemoryTombstone>, StoreError> {
        let row = sqlx::query(
            "
            SELECT memory_id, provenance, access_policy, deleted_at, deleted_by_principal
            FROM memory_tombstones
            WHERE memory_id = $1
            ",
        )
        .bind(memory_id.to_string())
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(row_to_tombstone).transpose()
    }

    pub(crate) async fn register_scope_impl(&self, scope: ScopeDefinition) -> Result<(), StoreError> {
        let _result = sqlx::query(
            "
            INSERT INTO scope_registry (
                scope_key, display_name, description, aliases, matchers, parent, related, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (scope_key) DO UPDATE SET
                display_name = excluded.display_name,
                description = excluded.description,
                aliases = excluded.aliases,
                matchers = excluded.matchers,
                parent = excluded.parent,
                related = excluded.related,
                updated_at = excluded.updated_at
            ",
        )
        .bind(scope.scope_key)
        .bind(scope.display_name)
        .bind(scope.description)
        .bind(Json(scope.aliases))
        .bind(Json(scope.matchers))
        .bind(scope.parent)
        .bind(Json(scope.related))
        .bind(self.clock_now())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub(crate) async fn list_scopes_impl(&self) -> Result<Vec<ScopeDefinition>, StoreError> {
        let rows = sqlx::query(
            "
            SELECT scope_key, display_name, description, aliases, matchers, parent, related
            FROM scope_registry
            ORDER BY scope_key
            ",
        )
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let aliases: Json<Vec<String>> = row.try_get("aliases")?;
                let matchers: Json<Vec<String>> = row.try_get("matchers")?;
                let related: Json<Vec<String>> = row.try_get("related")?;
                Ok(ScopeDefinition {
                    scope_key: row.try_get("scope_key")?,
                    display_name: row.try_get("display_name")?,
                    description: row.try_get("description")?,
                    aliases: aliases.0,
                    matchers: matchers.0,
                    parent: row.try_get("parent")?,
                    related: related.0,
                })
            })
            .collect()
    }

    pub(crate) async fn upsert_metadata_impl(&self, metadata: MemoryMetadata) -> Result<(), StoreError> {
        self.upsert_metadata_audited_impl(metadata, None).await
    }

    pub(crate) async fn upsert_metadata_audited_impl(&self, metadata: MemoryMetadata, audit: Option<&AuditDraft>) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        upsert_metadata_tx(&mut tx, &metadata, self.clock_now()).await?;
        insert_optional_audit_draft_tx(&mut tx, &metadata.memory_id, audit).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn get_metadata_impl(&self, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, StoreError> {
        let row = sqlx::query(
            "
            SELECT memory_id, scope_key, summary, agent_label, created_by_principal, quality_flags, schema_version
            FROM memory_metadata
            WHERE memory_id = $1
            ",
        )
        .bind(memory_id.to_string())
        .fetch_optional(self.pool())
        .await?;

        row.as_ref().map(row_to_metadata).transpose()
    }

    pub(crate) async fn metadata_migration_report_impl(&self) -> Result<MetadataMigrationReport, StoreError> {
        let total_memories = count_query(self.pool(), "SELECT COUNT(*) FROM memories").await?;
        let metadata_rows = count_query(self.pool(), "SELECT COUNT(*) FROM memory_metadata").await?;
        let missing_metadata = total_memories.saturating_sub(metadata_rows);
        let missing_summary = count_query(
            self.pool(),
            "
            SELECT COUNT(*)
            FROM memories AS m
            LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
            WHERE meta.summary IS NULL OR trim(meta.summary) = ''
            ",
        )
        .await?;
        let unresolved_scope = count_query(
            self.pool(),
            "
            SELECT COUNT(*)
            FROM memories AS m
            LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
            WHERE COALESCE(meta.scope_key, m.provenance->>'source_conversation') IS NULL
               OR COALESCE(meta.scope_key, m.provenance->>'source_conversation') = 'inbox/unresolved'
            ",
        )
        .await?;
        let duplicate_candidates = count_query(
            self.pool(),
            "
            SELECT COALESCE(SUM(cnt - 1), 0)::bigint
            FROM (
                SELECT COUNT(*) AS cnt
                FROM memories
                GROUP BY content
                HAVING COUNT(*) > 1
            ) AS duplicates
            ",
        )
        .await?;
        let oversized_threshold = i64::try_from(LARGE_CONTENT_WARNING_THRESHOLD_BYTES).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let oversized_raw = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM memories WHERE octet_length(content) > $1")
            .bind(oversized_threshold)
            .fetch_one(self.pool())
            .await?;
        let oversized = nonnegative_i64_to_u64(oversized_raw)?;
        let code_derived = count_query(
            self.pool(),
            "
            SELECT COUNT(*)
            FROM memories
            WHERE content LIKE '%```%'
               OR content LIKE '%fn %'
               OR content LIKE '%function %'
               OR content LIKE '%class %'
               OR content LIKE '%use %;%'
            ",
        )
        .await?;

        Ok(MetadataMigrationReport {
            total_memories,
            metadata_rows,
            missing_metadata,
            missing_summary,
            unresolved_scope,
            duplicate_candidates,
            oversized,
            code_derived,
        })
    }

    pub(crate) async fn migrate_metadata_impl(&self, registered_scope_keys: &[String], dry_run: bool) -> Result<MetadataMigrationOutcome, StoreError> {
        self.migrate_metadata_audited_impl(registered_scope_keys, dry_run, None).await
    }

    pub(crate) async fn migrate_metadata_audited_impl(
        &self,
        registered_scope_keys: &[String],
        dry_run: bool,
        audit: Option<&AuditDraft>,
    ) -> Result<MetadataMigrationOutcome, StoreError> {
        let registered_scope_keys = registered_scope_keys.iter().cloned().collect::<HashSet<_>>();
        let skipped_existing = count_query(self.pool(), "SELECT COUNT(*) FROM memory_metadata").await?;
        let candidates = load_metadata_migration_candidates(self.pool()).await?;
        let candidate_count = u64::try_from(candidates.len()).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        let prepared_rows = candidates
            .into_iter()
            .map(|candidate| prepare_metadata_migration_metadata(candidate, &registered_scope_keys))
            .collect::<Vec<_>>();
        let mut report = metadata_migration_outcome(candidate_count, skipped_existing, &prepared_rows);

        if dry_run {
            return Ok(report);
        }

        let mut tx = self.pool().begin().await?;
        report.migrated = insert_metadata_migration_rows(&mut tx, &prepared_rows, self.clock_now(), audit).await?;
        tx.commit().await?;
        Ok(report)
    }
}

impl MemoryReader for PostgresStore {
    fn fts_available(&self) -> bool {
        true
    }

    async fn get(&self, id: &MemoryId, principal: Option<&str>) -> Result<Option<Memory>, StoreError> {
        self.get_impl(id, principal).await
    }

    async fn search_by_embedding(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: &MemoryFilter,
        ctx: &QueryContext,
        max_distance: Option<f64>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        self.search_by_embedding_impl(embedding, limit, filter.clone(), ctx.clone(), max_distance).await
    }

    async fn search_by_text(&self, query: &str, limit: usize, filter: &MemoryFilter, ctx: &QueryContext) -> Result<Vec<SearchResult>, StoreError> {
        self.search_by_text_impl(query, limit, filter.clone(), ctx.clone()).await
    }

    async fn search_by_fts(&self, query: &str, limit: usize, filter: &MemoryFilter, ctx: &QueryContext, context: Option<&str>) -> Result<Vec<SearchResult>, StoreError> {
        self.search_by_fts_impl(query, limit, filter.clone(), ctx.clone(), context).await
    }

    async fn list(&self, filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<Memory>, StoreError> {
        self.list_impl(filter, ctx).await
    }

    async fn count(&self, filter: MemoryFilter, ctx: QueryContext, top_tags_limit: usize) -> Result<MemoryStats, StoreError> {
        self.count_impl(filter, ctx, top_tags_limit).await
    }

    async fn list_for_reembed(&self, limit: usize) -> Result<Vec<(MemoryId, String, i64)>, StoreError> {
        self.list_for_reembed_impl(limit).await
    }

    async fn get_for_reembed(&self, id: &MemoryId, principal: &str) -> Result<Option<(String, i64)>, StoreError> {
        self.get_for_reembed_impl(id, principal).await
    }

    async fn list_with_embeddings(&self, scopes_any: Option<&[String]>, limit: usize) -> Result<Vec<MemoryWithEmbedding>, StoreError> {
        self.list_with_embeddings_impl(scopes_any, limit).await
    }

    async fn query_audit_log(&self, memory_id: &MemoryId, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        self.query_audit_log_impl(memory_id, limit).await
    }

    async fn get_tombstone(&self, memory_id: &MemoryId) -> Result<Option<MemoryTombstone>, StoreError> {
        self.get_tombstone_impl(memory_id).await
    }

    async fn fetch_embeddings_for_ids(&self, ids: &[MemoryId]) -> Result<EmbeddingMap, StoreError> {
        self.fetch_embeddings_for_ids_impl(ids).await
    }

    async fn find_embedding_neighbors(&self, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<EmbeddingNeighbor>, StoreError> {
        self.find_embedding_neighbors_impl(embedding, max_l2_distance, limit).await
    }
}

impl MemoryWriter for PostgresStore {
    async fn store(&self, memory: &Memory, embedding: Option<&[f32]>) -> Result<MemoryId, StoreError> {
        self.store_impl(memory, embedding).await
    }

    async fn store_audited(&self, memory: &Memory, embedding: Option<&[f32]>, audit: &AuditDraft) -> Result<MemoryId, StoreError> {
        self.store_audited_impl(memory, embedding, Some(audit)).await
    }

    async fn store_with_supersession(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId) -> Result<MemoryId, StoreError> {
        self.store_with_supersession_impl(memory, embedding, supersedes_id).await
    }

    async fn store_with_supersession_audited(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: &MemoryId, audit: &AuditDraft) -> Result<MemoryId, StoreError> {
        self.store_with_supersession_audited_impl(memory, embedding, supersedes_id, Some(audit)).await
    }

    async fn store_with_metadata(&self, memory: &Memory, embedding: Option<&[f32]>, supersedes_id: Option<&MemoryId>, metadata: &MemoryMetadata) -> Result<MemoryId, StoreError> {
        self.store_with_metadata_impl(memory, embedding, supersedes_id, metadata).await
    }

    async fn store_with_metadata_audited(
        &self,
        memory: &Memory,
        embedding: Option<&[f32]>,
        supersedes_id: Option<&MemoryId>,
        metadata: &MemoryMetadata,
        audit: &AuditDraft,
    ) -> Result<MemoryId, StoreError> {
        self.store_with_metadata_audited_impl(memory, embedding, supersedes_id, metadata, Some(audit)).await
    }

    async fn store_batch(&self, memories: &[MemoryWithEmbedding]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_impl(memories).await
    }

    async fn store_batch_audited(&self, memories: &[MemoryWithEmbedding], audits: &[AuditDraft]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_audited_impl(memories, Some(audits)).await
    }

    async fn store_batch_with_supersession(&self, memories: &[MemoryWithEmbedding], supersedes: &[Option<MemoryId>]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_supersession_impl(memories, supersedes).await
    }

    async fn store_batch_with_supersession_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        audits: &[AuditDraft],
    ) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_supersession_audited_impl(memories, supersedes, Some(audits)).await
    }

    async fn store_batch_with_metadata(&self, memories: &[MemoryWithEmbedding], supersedes: &[Option<MemoryId>], metadata: &[MemoryMetadata]) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_metadata_impl(memories, supersedes, metadata).await
    }

    async fn store_batch_with_metadata_audited(
        &self,
        memories: &[MemoryWithEmbedding],
        supersedes: &[Option<MemoryId>],
        metadata: &[MemoryMetadata],
        audits: &[AuditDraft],
    ) -> Result<Vec<MemoryId>, StoreError> {
        self.store_batch_with_metadata_audited_impl(memories, supersedes, metadata, Some(audits)).await
    }

    async fn update(&self, id: &MemoryId, update: &MemoryUpdate) -> Result<bool, StoreError> {
        self.update_impl(id, update).await
    }

    async fn delete(&self, id: &MemoryId) -> Result<bool, StoreError> {
        self.delete_impl(id).await
    }

    async fn set_embedding(&self, id: &MemoryId, embedding: &[f32], expected_revision: i64) -> Result<(), StoreError> {
        self.set_embedding_impl(id, embedding, expected_revision).await
    }

    async fn claim_for_reembed(&self, limit: usize) -> Result<Vec<ReembedClaim>, StoreError> {
        self.claim_for_reembed_impl(limit).await
    }

    async fn release_embedding_claim(&self, id: &MemoryId, expected_revision: i64, claim_token: &str) -> Result<bool, StoreError> {
        self.release_embedding_claim_impl(id, expected_revision, claim_token).await
    }

    async fn update_authorized(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.update_authorized_impl(id, update, principal).await
    }

    async fn update_authorized_audited(&self, id: &MemoryId, update: &MemoryUpdate, principal: &str, audit: &AuditDraft) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.update_authorized_audited_impl(id, update, principal, Some(audit)).await
    }

    async fn update_authorized_with_metadata_audited(
        &self,
        id: &MemoryId,
        update: &MemoryUpdate,
        metadata_patch: Option<&MetadataPatch>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<AuthorizedUpdateOutcome, StoreError> {
        self.update_authorized_with_metadata_audited_impl(id, update, metadata_patch, principal, Some(audit)).await
    }

    async fn delete_authorized(&self, id: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.delete_authorized_impl(id, principal).await
    }

    async fn delete_authorized_audited(&self, id: &MemoryId, principal: &str, audit: &AuditDraft) -> Result<WriteOutcome, StoreError> {
        self.delete_authorized_audited_impl(id, principal, Some(audit)).await
    }

    async fn bulk_delete_ids(&self, ids: Vec<MemoryId>, principal: &str) -> Result<BulkAuthOutcome, StoreError> {
        self.bulk_delete_ids_impl(ids, principal).await
    }

    async fn bulk_delete_ids_audited(&self, ids: Vec<MemoryId>, principal: &str, audit: &AuditDraft) -> Result<BulkAuthOutcome, StoreError> {
        self.bulk_delete_ids_audited_impl(ids, principal, Some(audit)).await
    }

    async fn bulk_update_ids(&self, ids: Vec<MemoryId>, update: MemoryUpdate, principal: &str, now: DateTime<Utc>) -> Result<BulkAuthOutcome, StoreError> {
        self.bulk_update_ids_impl(ids, update, principal, now).await
    }

    async fn bulk_update_ids_audited(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: DateTime<Utc>,
        audit: &AuditDraft,
    ) -> Result<BulkAuthOutcome, StoreError> {
        self.bulk_update_ids_audited_impl(ids, update, principal, now, Some(audit)).await
    }

    async fn record_search_impression(&self, ids: &[MemoryId]) -> Result<(), StoreError> {
        self.record_search_impression_impl(ids).await
    }

    async fn record_memory_use(
        &self,
        ids: &[MemoryId],
        principal: &str,
        event_weight: f64,
        now: DateTime<Utc>,
        activity_half_life_hours: f64,
    ) -> Result<RecordUseOutcome, StoreError> {
        self.record_memory_use_impl(ids, principal, event_weight, now, activity_half_life_hours).await
    }

    async fn write_audit_entry(
        &self,
        memory_id: &MemoryId,
        action: AuditAction,
        principal: Option<&str>,
        timestamp: DateTime<Utc>,
        details: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        let entry = AuditEntry {
            action,
            caller_agent: principal.map(str::to_owned),
            timestamp,
            details: details.cloned(),
        };
        self.write_audit_entry_impl(memory_id, &entry).await
    }

    async fn mark_superseded_by(&self, id: &MemoryId, superseded_by: &MemoryId) -> Result<bool, StoreError> {
        self.mark_superseded_by_impl(id, superseded_by).await
    }

    async fn mark_superseded_by_authorized(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str) -> Result<WriteOutcome, StoreError> {
        self.mark_superseded_by_authorized_impl(id, superseded_by, principal).await
    }

    async fn mark_superseded_by_authorized_audited(&self, id: &MemoryId, superseded_by: &MemoryId, principal: &str, audit: &AuditDraft) -> Result<WriteOutcome, StoreError> {
        self.mark_superseded_by_authorized_audited_impl(id, superseded_by, principal, Some(audit)).await
    }
}

impl MemoryAdmin for PostgresStore {
    async fn evict_expired(&self) -> Result<u64, StoreError> {
        self.evict_expired_impl().await
    }

    async fn reassign_scope(&self, from_scope: &str, to_scope: &str, origin_conversation: Option<&str>, principal: &str) -> Result<ReassignScopeOutcome, StoreError> {
        self.reassign_scope_impl(from_scope, to_scope, origin_conversation, principal).await
    }

    async fn reassign_scope_audited(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<ReassignScopeOutcome, StoreError> {
        self.reassign_scope_audited_impl(from_scope, to_scope, origin_conversation, principal, Some(audit)).await
    }

    async fn register_scope(&self, scope: ScopeDefinition) -> Result<(), StoreError> {
        self.register_scope_impl(scope).await
    }

    async fn list_scopes(&self) -> Result<Vec<ScopeDefinition>, StoreError> {
        self.list_scopes_impl().await
    }

    async fn upsert_metadata(&self, metadata: MemoryMetadata) -> Result<(), StoreError> {
        self.upsert_metadata_impl(metadata).await
    }

    async fn upsert_metadata_audited(&self, metadata: MemoryMetadata, audit: &AuditDraft) -> Result<(), StoreError> {
        self.upsert_metadata_audited_impl(metadata, Some(audit)).await
    }

    async fn get_metadata(&self, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, StoreError> {
        self.get_metadata_impl(memory_id).await
    }

    async fn metadata_migration_report(&self) -> Result<MetadataMigrationReport, StoreError> {
        self.metadata_migration_report_impl().await
    }

    async fn migrate_metadata(&self, registered_scope_keys: &[String], dry_run: bool) -> Result<MetadataMigrationOutcome, StoreError> {
        self.migrate_metadata_impl(registered_scope_keys, dry_run).await
    }

    async fn migrate_metadata_audited(&self, registered_scope_keys: &[String], dry_run: bool, audit: &AuditDraft) -> Result<MetadataMigrationOutcome, StoreError> {
        self.migrate_metadata_audited_impl(registered_scope_keys, dry_run, Some(audit)).await
    }
}

fn validate_batch_embedding_dimensions(memories: &[MemoryWithEmbedding], embedding_dimensions: usize) -> Result<(), StoreError> {
    for memory_with_embedding in memories {
        if let Some(embedding) = &memory_with_embedding.embedding {
            validate_embedding_dimensions(embedding, embedding_dimensions)?;
        }
    }
    Ok(())
}

async fn insert_memory_with_embedding(tx: &mut Transaction<'_, Postgres>, memory: &Memory, embedding: Option<&[f32]>) -> Result<(), StoreError> {
    insert_memory_row(tx, memory, embedding).await?;
    if let Some(embedding) = embedding {
        insert_embedding(tx, &memory.id, embedding).await?;
    }
    insert_entities(tx, &memory.id, &memory.entities).await?;
    Ok(())
}

fn metadata_len_mismatch(expected: usize, actual: usize) -> StoreError {
    StoreError::Conflict(format!("metadata length ({actual}) must match memories length ({expected})"))
}

fn audit_len_mismatch(expected: usize, actual: usize) -> StoreError {
    StoreError::Conflict(format!("audit length ({actual}) must match memories length ({expected})"))
}

fn supersedes_len_mismatch(expected: usize, actual: usize) -> StoreError {
    StoreError::Conflict(format!("supersedes length ({actual}) must match memories length ({expected})"))
}

fn validate_metadata_memory_id(memory_id: &MemoryId, metadata: &MemoryMetadata) -> Result<(), StoreError> {
    if metadata.memory_id == *memory_id {
        return Ok(());
    }
    Err(StoreError::Conflict(format!(
        "metadata memory_id ({}) must match memory id ({memory_id})",
        metadata.memory_id
    )))
}

async fn upsert_metadata_tx(tx: &mut Transaction<'_, Postgres>, metadata: &MemoryMetadata, now: DateTime<Utc>) -> Result<(), StoreError> {
    let _result = sqlx::query(
        "
        INSERT INTO memory_metadata (
            memory_id, scope_key, summary, agent_label, created_by_principal,
            quality_flags, schema_version, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (memory_id) DO UPDATE SET
            scope_key = excluded.scope_key,
            summary = excluded.summary,
            agent_label = excluded.agent_label,
            created_by_principal = COALESCE(memory_metadata.created_by_principal, excluded.created_by_principal),
            quality_flags = excluded.quality_flags,
            schema_version = excluded.schema_version,
            updated_at = excluded.updated_at
        ",
    )
    .bind(metadata.memory_id.to_string())
    .bind(metadata.scope_key.as_deref())
    .bind(metadata.summary.as_deref())
    .bind(metadata.agent_label.as_deref())
    .bind(metadata.created_by_principal.as_deref())
    .bind(Json(metadata.quality_flags.clone()))
    .bind(metadata.schema_version)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn get_metadata_tx(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId) -> Result<Option<MemoryMetadata>, StoreError> {
    let row = sqlx::query(
        "
        SELECT memory_id, scope_key, summary, agent_label, created_by_principal, quality_flags, schema_version
        FROM memory_metadata
        WHERE memory_id = $1
        ",
    )
    .bind(memory_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;

    row.as_ref().map(row_to_metadata).transpose()
}

async fn insert_memory_row(tx: &mut Transaction<'_, Postgres>, memory: &Memory, embedding: Option<&[f32]>) -> Result<(), StoreError> {
    let impression_count = i64::try_from(memory.impression_count).map_err(|e| StoreError::Serialization(Box::new(e)))?;
    let superseded_by = memory.superseded_by.map(|id| id.to_string());
    let has_embedding = embedding.is_some();
    let _result = sqlx::query(
        "
        INSERT INTO memories (
            id, content, tags, provenance, access_policy, created_at, expires_at,
            has_embedding, memory_type, importance, impression_count, last_impressed_at,
            superseded_by, activity_mass, last_used_at, updated_at, confidence
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17
        )
        ",
    )
    .bind(memory.id.to_string())
    .bind(&memory.content)
    .bind(Json(memory.tags.clone()))
    .bind(Json(memory.provenance.clone()))
    .bind(Json(memory.access_policy.clone()))
    .bind(memory.created_at)
    .bind(memory.expires_at)
    .bind(has_embedding)
    .bind(memory.memory_type.to_string())
    .bind(memory.importance.value())
    .bind(impression_count)
    .bind(memory.last_impressed_at)
    .bind(superseded_by)
    .bind(memory.activity_mass)
    .bind(memory.last_used_at)
    .bind(memory.updated_at)
    .bind(memory.confidence.value())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_embedding(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId, embedding: &[f32]) -> Result<(), StoreError> {
    let vector = pgvector_literal(embedding);
    let _result = sqlx::query(
        "
        INSERT INTO memory_embeddings (memory_id, embedding)
        VALUES ($1, $2::vector)
        ON CONFLICT (memory_id) DO UPDATE SET
            embedding = excluded.embedding,
            updated_at = NOW()
        ",
    )
    .bind(memory_id.to_string())
    .bind(vector)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_entities(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId, entities: &[Entity]) -> Result<(), StoreError> {
    for entity in entities {
        let _result = sqlx::query(
            "
            INSERT INTO memory_entities (memory_id, entity, entity_type)
            VALUES ($1, $2, $3)
            ON CONFLICT DO NOTHING
            ",
        )
        .bind(memory_id.to_string())
        .bind(&entity.name)
        .bind(entity.entity_type.as_str())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn fetch_memory_by_id(pool: &PgPool, id: &MemoryId) -> Result<Option<Memory>, StoreError> {
    let row = sqlx::query(AssertSqlSafe(fetch_memory_by_id_query(false)))
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_memory).transpose()
}

async fn fetch_memory_by_id_for_update_tx(tx: &mut Transaction<'_, Postgres>, id: &MemoryId) -> Result<Option<Memory>, StoreError> {
    let row = sqlx::query(AssertSqlSafe(fetch_memory_by_id_query(true)))
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
    row.as_ref().map(row_to_memory).transpose()
}

fn fetch_memory_by_id_query(for_update: bool) -> String {
    let mut query = format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = $1");
    if for_update {
        query.push_str(" FOR UPDATE");
    }
    query
}

async fn memory_exists_tx(tx: &mut Transaction<'_, Postgres>, id: &MemoryId) -> Result<bool, StoreError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memories WHERE id = $1)")
        .bind(id.to_string())
        .fetch_one(&mut **tx)
        .await?;
    Ok(exists)
}

async fn validate_superseded_exists(tx: &mut Transaction<'_, Postgres>, supersedes_id: &MemoryId) -> Result<(), StoreError> {
    if !memory_exists_tx(tx, supersedes_id).await? {
        return Err(StoreError::NotFound(format!("superseded memory not found: {supersedes_id}")));
    }
    Ok(())
}

async fn mark_superseded_tx(tx: &mut Transaction<'_, Postgres>, id: &MemoryId, superseded_by: &MemoryId) -> Result<bool, StoreError> {
    let result = sqlx::query("UPDATE memories SET superseded_by = $1 WHERE id = $2 AND superseded_by IS NULL")
        .bind(superseded_by.to_string())
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    if result.rows_affected() == 0 && memory_exists_tx(tx, id).await? {
        return Err(StoreError::Conflict(format!("memory {id} is already superseded")));
    }
    Ok(result.rows_affected() > 0)
}

async fn delete_memory_tx(tx: &mut Transaction<'_, Postgres>, id: &MemoryId) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM memories WHERE id = $1").bind(id.to_string()).execute(&mut **tx).await?;
    Ok(result.rows_affected() > 0)
}

async fn insert_tombstone_tx(tx: &mut Transaction<'_, Postgres>, memory: &Memory, deleted_at: DateTime<Utc>, deleted_by: Option<&str>) -> Result<(), StoreError> {
    let _result = sqlx::query(
        "
        INSERT INTO memory_tombstones (memory_id, provenance, access_policy, deleted_at, deleted_by_principal)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (memory_id) DO UPDATE SET
            provenance = excluded.provenance,
            access_policy = excluded.access_policy,
            deleted_at = excluded.deleted_at,
            deleted_by_principal = excluded.deleted_by_principal
        ",
    )
    .bind(memory.id.to_string())
    .bind(Json(memory.provenance.clone()))
    .bind(Json(memory.access_policy.clone()))
    .bind(deleted_at)
    .bind(deleted_by)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_audit_draft_tx(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId, audit: &AuditDraft) -> Result<(), StoreError> {
    let _result = sqlx::query(
        "
        INSERT INTO memory_audit_log (memory_id, action, caller_agent, timestamp, details)
        VALUES ($1, $2, $3, $4, $5)
        ",
    )
    .bind(memory_id.to_string())
    .bind(audit.action.to_string())
    .bind(audit.caller_agent.clone())
    .bind(audit.timestamp)
    .bind(audit.details.clone().map(Json))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_optional_audit_draft_tx(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId, audit: Option<&AuditDraft>) -> Result<(), StoreError> {
    if let Some(audit) = audit {
        insert_audit_draft_tx(tx, memory_id, audit).await?;
    }
    Ok(())
}

async fn delete_embedding_tx(tx: &mut Transaction<'_, Postgres>, id: &MemoryId) -> Result<(), StoreError> {
    let _result = sqlx::query("DELETE FROM memory_embeddings WHERE memory_id = $1")
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn replace_entities_tx(tx: &mut Transaction<'_, Postgres>, memory_id: &MemoryId, entities: &[Entity]) -> Result<(), StoreError> {
    let _result = sqlx::query("DELETE FROM memory_entities WHERE memory_id = $1")
        .bind(memory_id.to_string())
        .execute(&mut **tx)
        .await?;
    insert_entities(tx, memory_id, entities).await
}

async fn apply_update_tx(tx: &mut Transaction<'_, Postgres>, id: &MemoryId, update: &MemoryUpdate, now: DateTime<Utc>) -> Result<AuthorizedUpdateOutcome, StoreError> {
    let content_changed = update.content.is_some();
    let mut reembed_revision = None;

    if has_column_updates(update) {
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE memories SET ");
        let mut has_assignments = false;

        if let Some(content) = &update.content {
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("content = ").push_bind(content.clone());
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("has_embedding = FALSE");
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("embedding_revision = embedding_revision + 1");
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("embedding_claimed_at = NULL");
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("embedding_claim_token = NULL");
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("updated_at = ").push_bind(now);
        }
        if let Some(tags) = &update.tags {
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("tags = ").push_bind(Json(tags.clone()));
        }
        if let Some(policy) = &update.access_policy {
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("access_policy = ").push_bind(Json(policy.clone()));
        }
        if let Some(importance) = update.importance {
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("importance = ").push_bind(importance.value());
        }
        if let Some(confidence) = update.confidence {
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("confidence = ").push_bind(confidence.value());
        }
        if let Some(source_conversation) = &update.source_conversation {
            push_assignment_separator(&mut builder, &mut has_assignments);
            let _ = builder.push("provenance = jsonb_set(provenance, ARRAY['source_conversation'], to_jsonb(");
            let _ = builder.push_bind(source_conversation.clone());
            let _ = builder.push("::text), true)");
        }

        let _ = builder.push(" WHERE id = ").push_bind(id.to_string());
        let _ = builder.push(" RETURNING embedding_revision");
        let revision: Option<i64> = builder.build_query_scalar().fetch_optional(&mut **tx).await?;
        let Some(revision) = revision else {
            return Ok(AuthorizedUpdateOutcome {
                outcome: WriteOutcome::NotFound,
                reembed_revision: None,
            });
        };
        if content_changed {
            delete_embedding_tx(tx, id).await?;
            reembed_revision = Some(revision);
        }
    } else if !memory_exists_tx(tx, id).await? {
        return Ok(AuthorizedUpdateOutcome {
            outcome: WriteOutcome::NotFound,
            reembed_revision: None,
        });
    }

    if let Some(entities) = &update.entities {
        replace_entities_tx(tx, id, entities).await?;
    }

    Ok(AuthorizedUpdateOutcome {
        outcome: WriteOutcome::Applied,
        reembed_revision,
    })
}

const fn has_column_updates(update: &MemoryUpdate) -> bool {
    update.content.is_some()
        || update.tags.is_some()
        || update.access_policy.is_some()
        || update.importance.is_some()
        || update.confidence.is_some()
        || update.source_conversation.is_some()
}

fn push_assignment_separator(builder: &mut QueryBuilder<Postgres>, has_assignments: &mut bool) {
    if *has_assignments {
        let _ = builder.push(", ");
    } else {
        *has_assignments = true;
    }
}

struct PostgresFilterPage<'a> {
    filter: &'a MemoryFilter,
    caller: Option<&'a str>,
    now: DateTime<Utc>,
    page_size: usize,
    offset: usize,
}

struct PostgresVisibleRowsContext<'a> {
    filter: &'a MemoryFilter,
    caller: Option<&'a str>,
    now: DateTime<Utc>,
}

async fn fetch_filtered_memory_rows(pool: &PgPool, page: &PostgresFilterPage<'_>) -> Result<Vec<PgRow>, StoreError> {
    let mut builder = QueryBuilder::<Postgres>::new(format!("SELECT {MEMORY_COLUMNS} FROM memories"));
    let mut has_condition = false;
    push_postgres_filter_conditions(&mut builder, page.filter, page.caller, page.now, &mut has_condition);
    push_postgres_ordered_page(&mut builder, page.page_size, page.offset)?;
    Ok(builder.build().fetch_all(pool).await?)
}

async fn fetch_text_search_rows(pool: &PgPool, like_pattern: &str, page: &PostgresFilterPage<'_>) -> Result<Vec<PgRow>, StoreError> {
    let mut builder = QueryBuilder::<Postgres>::new(format!("SELECT {MEMORY_COLUMNS} FROM memories"));
    let mut has_condition = false;
    push_postgres_condition_separator(&mut builder, &mut has_condition);
    let _ = builder.push("content ILIKE ").push_bind(like_pattern.to_owned()).push(" ESCAPE '\\'");
    push_postgres_filter_conditions(&mut builder, page.filter, page.caller, page.now, &mut has_condition);
    push_postgres_ordered_page(&mut builder, page.page_size, page.offset)?;
    Ok(builder.build().fetch_all(pool).await?)
}

async fn visible_memory_from_row(pool: &PgPool, row: PgRow, filter: &MemoryFilter, caller: Option<&str>, now: DateTime<Utc>) -> Result<Option<Memory>, StoreError> {
    let mut memory = row_to_memory(&row)?;
    memory.entities = fetch_entities(pool, &memory.id).await?;
    Ok(apply_access_policy_for_filter(memory, filter, caller, now))
}

async fn append_visible_memory_rows(pool: &PgPool, rows: Vec<PgRow>, ctx: &PostgresVisibleRowsContext<'_>, limit: usize, results: &mut Vec<Memory>) -> Result<bool, StoreError> {
    for row in rows {
        let Some(memory) = visible_memory_from_row(pool, row, ctx.filter, ctx.caller, ctx.now).await? else {
            continue;
        };
        results.push(memory);
        if results.len() >= limit {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn record_visible_memory_rows(pool: &PgPool, rows: Vec<PgRow>, ctx: &PostgresVisibleRowsContext<'_>, stats: &mut PostgresStatsAccumulator) -> Result<(), StoreError> {
    for row in rows {
        let Some(memory) = visible_memory_from_row(pool, row, ctx.filter, ctx.caller, ctx.now).await? else {
            continue;
        };
        stats.record(&memory);
    }
    Ok(())
}

fn push_postgres_ordered_page(builder: &mut QueryBuilder<Postgres>, page_size: usize, offset: usize) -> Result<(), StoreError> {
    let limit = usize_to_i64(page_size, "PostgreSQL filtered page size")?;
    let offset = usize_to_i64(offset, "PostgreSQL filtered page offset")?;
    let _ = builder
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "linear PostgreSQL MemoryFilter-to-SQL translation keeps backend parity reviewable")]
fn push_postgres_filter_conditions(builder: &mut QueryBuilder<Postgres>, filter: &MemoryFilter, caller: Option<&str>, now: DateTime<Utc>, has_condition: &mut bool) {
    push_postgres_condition_separator(builder, has_condition);
    let _ = builder.push("(expires_at IS NULL OR expires_at > ").push_bind(now).push(")");

    if let Some(range) = &filter.time_range {
        if let Some(after) = range.after {
            push_postgres_condition_separator(builder, has_condition);
            let _ = builder.push("created_at >= ").push_bind(after);
        }
        if let Some(before) = range.before {
            push_postgres_condition_separator(builder, has_condition);
            let _ = builder.push("created_at < ").push_bind(before);
        }
    }

    if let Some(agent) = &filter.agent_label {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("provenance->>'source_agent' = ").push_bind(agent.clone());
    }

    if let Some(scope) = &filter.scope {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("provenance->>'source_conversation' = ").push_bind(scope.clone());
    }

    if let Some(origin_scope) = &filter.origin_scope {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder
            .push("COALESCE(provenance->>'origin_conversation', provenance->>'source_conversation') = ")
            .push_bind(origin_scope.clone());
    }

    if let Some(scopes_any) = &filter.scopes_any
        && !scopes_any.is_empty()
    {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("provenance->>'source_conversation' = ANY(").push_bind(scopes_any.clone()).push(")");
    }

    if let Some(text) = &filter.text_search {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("content ILIKE ").push_bind(format!("%{}%", escape_like(text))).push(" ESCAPE '\\'");
    }

    if let Some(has_embedding) = filter.has_embedding {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("has_embedding = ").push_bind(has_embedding);
    }

    if let Some(tags) = &filter.tags
        && !tags.is_empty()
    {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("tags ?| ").push_bind(tags.clone());
    }

    if let Some(memory_type) = filter.memory_type {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("memory_type = ").push_bind(memory_type.to_string());
    }

    if !filter.include_superseded.unwrap_or(false) {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder.push("superseded_by IS NULL");
    }

    if let Some(entity) = &filter.entity {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder
            .push("EXISTS (SELECT 1 FROM memory_entities me WHERE me.memory_id = memories.id AND me.entity = ")
            .push_bind(entity.clone())
            .push(")");
    }

    if let Some(entities) = &filter.entities_any
        && !entities.is_empty()
    {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder
            .push("EXISTS (SELECT 1 FROM memory_entities me WHERE me.memory_id = memories.id AND me.entity = ANY(")
            .push_bind(entities.clone())
            .push("))");
    }

    if let Some(entity_type) = &filter.entity_type {
        push_postgres_condition_separator(builder, has_condition);
        let _ = builder
            .push("EXISTS (SELECT 1 FROM memory_entities me WHERE me.memory_id = memories.id AND me.entity_type = ")
            .push_bind(entity_type.clone())
            .push(")");
    }

    push_postgres_access_condition(builder, caller.filter(|value| !value.trim().is_empty()), has_condition);
}

fn push_postgres_access_condition(builder: &mut QueryBuilder<Postgres>, caller: Option<&str>, has_condition: &mut bool) {
    push_postgres_condition_separator(builder, has_condition);
    if let Some(caller) = caller {
        let _ = builder.push("(");
        let _ = builder.push("access_policy->>'type' = 'public'");
        let _ = builder.push(" OR provenance->>'source_agent' = ").push_bind(caller.to_owned());
        let _ = builder.push(" OR access_policy->>'type' = 'redacted'");
        let _ = builder.push(" OR (access_policy->>'type' = 'restricted' AND (access_policy->'allowed') ? ");
        let _ = builder.push_bind(caller.to_owned()).push(")");
        let _ = builder.push(")");
    } else {
        let _ = builder.push("access_policy->>'type' = 'public'");
    }
}

fn push_postgres_condition_separator(builder: &mut QueryBuilder<Postgres>, has_condition: &mut bool) {
    if *has_condition {
        let _ = builder.push(" AND ");
    } else {
        let _ = builder.push(" WHERE ");
        *has_condition = true;
    }
}

fn row_to_metadata(row: &PgRow) -> Result<MemoryMetadata, StoreError> {
    let id_str: String = row.try_get("memory_id")?;
    let quality_flags: Json<Vec<String>> = row.try_get("quality_flags")?;
    Ok(MemoryMetadata {
        memory_id: parse_memory_id(&id_str, "memory_id")?,
        scope_key: row.try_get("scope_key")?,
        summary: row.try_get("summary")?,
        agent_label: row.try_get("agent_label")?,
        created_by_principal: row.try_get("created_by_principal")?,
        quality_flags: quality_flags.0,
        schema_version: row.try_get("schema_version")?,
    })
}

async fn count_query(pool: &PgPool, sql: &'static str) -> Result<u64, StoreError> {
    let raw = sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await?;
    nonnegative_i64_to_u64(raw)
}

fn nonnegative_i64_to_u64(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|e| StoreError::Serialization(Box::new(e)))
}

fn increment_count<K: Ord>(counts: &mut BTreeMap<K, u64>, key: K) {
    let count = counts.entry(key).or_insert(0);
    *count = count.saturating_add(1);
}

#[derive(Default)]
struct PostgresStatsAccumulator {
    total: u64,
    with_embedding: u64,
    tag_counts: BTreeMap<String, u64>,
    agent_counts: BTreeMap<String, u64>,
    memory_type_counts: BTreeMap<MemoryType, u64>,
    oldest: Option<DateTime<Utc>>,
    newest: Option<DateTime<Utc>>,
    scope_set: HashSet<String>,
    superseded_count: u64,
}

impl PostgresStatsAccumulator {
    fn record(&mut self, memory: &Memory) {
        self.total = self.total.saturating_add(1);
        if memory.has_embedding {
            self.with_embedding = self.with_embedding.saturating_add(1);
        }
        for tag in &memory.tags {
            increment_count(&mut self.tag_counts, tag.clone());
        }
        if let Some(agent) = &memory.provenance.source_agent {
            increment_count(&mut self.agent_counts, agent.clone());
        }
        increment_count(&mut self.memory_type_counts, memory.memory_type);
        self.oldest = Some(self.oldest.map_or(memory.created_at, |timestamp| timestamp.min(memory.created_at)));
        self.newest = Some(self.newest.map_or(memory.created_at, |timestamp| timestamp.max(memory.created_at)));
        if let Some(scope) = &memory.provenance.source_conversation {
            let _inserted = self.scope_set.insert(scope.clone());
        }
        if memory.superseded_by.is_some() {
            self.superseded_count = self.superseded_count.saturating_add(1);
        }
    }
}

struct MigrationCandidate {
    id: String,
    content: String,
    source_agent: Option<String>,
    source_conversation: Option<String>,
}

struct PreparedMigrationMetadata {
    id: String,
    scope_key: String,
    agent_label: Option<String>,
    quality_flags: Vec<String>,
    unresolved_scope: bool,
    oversized: bool,
    code_derived: bool,
}

async fn load_metadata_migration_candidates(pool: &PgPool) -> Result<Vec<MigrationCandidate>, StoreError> {
    let rows = sqlx::query(
        "
        SELECT
            m.id,
            m.content,
            m.provenance->>'source_agent' AS source_agent,
            m.provenance->>'source_conversation' AS source_conversation
        FROM memories AS m
        LEFT JOIN memory_metadata AS meta ON meta.memory_id = m.id
        WHERE meta.memory_id IS NULL
        ORDER BY m.created_at, m.id
        ",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(MigrationCandidate {
                id: row.try_get("id")?,
                content: row.try_get("content")?,
                source_agent: row.try_get("source_agent")?,
                source_conversation: row.try_get("source_conversation")?,
            })
        })
        .collect()
}

fn prepare_metadata_migration_metadata(candidate: MigrationCandidate, registered_scope_keys: &HashSet<String>) -> PreparedMigrationMetadata {
    let scope_key = candidate
        .source_conversation
        .as_deref()
        .filter(|scope| registered_scope_keys.contains(*scope))
        .map_or_else(|| UNRESOLVED_SCOPE.to_owned(), ToOwned::to_owned);
    let unresolved_scope = scope_key == UNRESOLVED_SCOPE;
    let oversized = candidate.content.len() > LARGE_CONTENT_WARNING_THRESHOLD_BYTES;
    let code_derived = looks_code_derived(&candidate.content);

    PreparedMigrationMetadata {
        id: candidate.id,
        scope_key,
        agent_label: candidate.source_agent,
        quality_flags: migration_quality_flags(unresolved_scope, oversized, code_derived),
        unresolved_scope,
        oversized,
        code_derived,
    }
}

fn metadata_migration_outcome(candidate_count: u64, skipped_existing: u64, prepared_rows: &[PreparedMigrationMetadata]) -> MetadataMigrationOutcome {
    MetadataMigrationOutcome {
        candidate_count,
        skipped_existing,
        migrated: 0,
        unresolved_scope: count_prepared_rows(prepared_rows, |row| row.unresolved_scope),
        missing_summary: candidate_count,
        oversized: count_prepared_rows(prepared_rows, |row| row.oversized),
        code_derived: count_prepared_rows(prepared_rows, |row| row.code_derived),
    }
}

fn count_prepared_rows(prepared_rows: &[PreparedMigrationMetadata], predicate: impl Fn(&PreparedMigrationMetadata) -> bool) -> u64 {
    prepared_rows.iter().filter(|row| predicate(row)).count().try_into().unwrap_or(u64::MAX)
}

async fn insert_metadata_migration_rows(
    tx: &mut Transaction<'_, Postgres>,
    prepared_rows: &[PreparedMigrationMetadata],
    now: DateTime<Utc>,
    audit: Option<&AuditDraft>,
) -> Result<u64, StoreError> {
    let mut migrated = 0_u64;
    for row in prepared_rows {
        let result = sqlx::query(
            "
            INSERT INTO memory_metadata (
                memory_id, scope_key, summary, agent_label, created_by_principal,
                quality_flags, schema_version, migrated_at, updated_at
            ) VALUES ($1, $2, NULL, $3, NULL, $4, 2, $5, $5)
            ON CONFLICT (memory_id) DO NOTHING
            ",
        )
        .bind(&row.id)
        .bind(&row.scope_key)
        .bind(&row.agent_label)
        .bind(Json(row.quality_flags.clone()))
        .bind(now)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() > 0 {
            let memory_id = row.id.parse().map_err(|e| StoreError::Serialization(format!("invalid memory id: {e}").into()))?;
            insert_optional_audit_draft_tx(tx, &memory_id, audit).await?;
        }
        migrated = migrated.saturating_add(result.rows_affected());
    }
    Ok(migrated)
}

fn migration_quality_flags(unresolved_scope: bool, oversized: bool, code_derived: bool) -> Vec<String> {
    let mut flags = vec!["missing_summary".to_owned()];
    if unresolved_scope {
        flags.push("missing_scope".to_owned());
    }
    if oversized {
        flags.push("oversized_content".to_owned());
    }
    if code_derived {
        flags.push("possible_code_dump".to_owned());
    }
    flags
}

fn looks_code_derived(content: &str) -> bool {
    content.contains("```")
        || content
            .lines()
            .take(20)
            .any(|line| line.trim_start().starts_with("fn ") || line.trim_start().starts_with("impl "))
}

struct PostgresEmbeddingSearchContext<'a> {
    filter: &'a MemoryFilter,
    caller: Option<&'a str>,
    now: DateTime<Utc>,
    limit: usize,
    max_distance: Option<f64>,
}

async fn collect_vector_results(pool: &PgPool, hits: Vec<VectorHit>, ctx: &PostgresEmbeddingSearchContext<'_>, results: &mut Vec<SearchResult>) -> Result<(), StoreError> {
    for hit in hits {
        let Some(result) = vector_hit_to_search_result(pool, hit, ctx).await? else {
            continue;
        };
        results.push(result);
        if results.len() >= ctx.limit {
            break;
        }
    }
    Ok(())
}

async fn vector_hit_to_search_result(pool: &PgPool, hit: VectorHit, ctx: &PostgresEmbeddingSearchContext<'_>) -> Result<Option<SearchResult>, StoreError> {
    if !ctx.max_distance.is_none_or(|threshold| hit.distance <= threshold) {
        return Ok(None);
    }
    let Some(mut memory) = fetch_memory_by_id(pool, &hit.memory_id).await? else {
        return Ok(None);
    };
    memory.entities = fetch_entities(pool, &memory.id).await?;
    if !memory.content_searchable_by(ctx.caller) {
        return Ok(None);
    }
    let Some(memory) = apply_access_policy_for_filter(memory, ctx.filter, ctx.caller, ctx.now) else {
        return Ok(None);
    };
    Ok(Some(SearchResult {
        memory,
        distance: Some(hit.distance),
        retrieval_score: None,
        reranker_score: None,
        composite_score: None,
        score_breakdown: None,
    }))
}

struct PostgresSearchContext<'a> {
    filter: &'a MemoryFilter,
    caller: Option<&'a str>,
    now: DateTime<Utc>,
    limit: usize,
    rank_column: Option<&'a str>,
}

#[expect(clippy::float_arithmetic, reason = "PostgreSQL FTS rank is negated to fit lower-distance-is-better scoring")]
async fn append_search_rows_to_results(pool: &PgPool, rows: Vec<PgRow>, ctx: &PostgresSearchContext<'_>, results: &mut Vec<SearchResult>) -> Result<(), StoreError> {
    for row in rows {
        let distance = ctx.rank_column.map(|column| row.try_get::<f64, _>(column).map(|rank| -rank)).transpose()?;
        let mut memory = row_to_memory(&row)?;
        memory.entities = fetch_entities(pool, &memory.id).await?;
        if !memory.content_searchable_by(ctx.caller) {
            continue;
        }
        let Some(memory) = apply_access_policy_for_filter(memory, ctx.filter, ctx.caller, ctx.now) else {
            continue;
        };
        results.push(SearchResult {
            memory,
            distance,
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        });
        if results.len() >= ctx.limit {
            break;
        }
    }
    Ok(())
}

async fn fetch_entities(pool: &PgPool, memory_id: &MemoryId) -> Result<Vec<Entity>, StoreError> {
    let rows = sqlx::query(
        "
        SELECT entity, entity_type
        FROM memory_entities
        WHERE memory_id = $1
        ORDER BY entity, entity_type
        ",
    )
    .bind(memory_id.to_string())
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let entity_type: String = row.try_get("entity_type")?;
            Ok(Entity {
                name: row.try_get("entity")?,
                entity_type: entity_type.try_into().map_err(|e: ParseEnumError| StoreError::Serialization(Box::new(e)))?,
            })
        })
        .collect()
}

fn row_to_memory(row: &PgRow) -> Result<Memory, StoreError> {
    let id_str: String = row.try_get("id")?;
    let memory_type_str: String = row.try_get("memory_type")?;
    let tags: Json<Vec<String>> = row.try_get("tags")?;
    let provenance: Json<Provenance> = row.try_get("provenance")?;
    let access_policy: Json<AccessPolicy> = row.try_get("access_policy")?;
    let superseded_by_str: Option<String> = row.try_get("superseded_by")?;
    let impression_count: i64 = row.try_get("impression_count")?;

    Ok(Memory {
        id: parse_memory_id(&id_str, "id")?,
        content: row.try_get("content")?,
        tags: tags.0,
        provenance: provenance.0,
        access_policy: access_policy.0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        expires_at: row.try_get("expires_at")?,
        has_embedding: row.try_get("has_embedding")?,
        memory_type: memory_type_str.parse().map_err(|e: ParseEnumError| StoreError::Serialization(Box::new(e)))?,
        importance: crate::types::Importance::new(row.try_get("importance")?),
        confidence: crate::types::Confidence::new(row.try_get("confidence")?),
        impression_count: u64::try_from(impression_count).map_err(|e| StoreError::Serialization(Box::new(e)))?,
        last_impressed_at: row.try_get("last_impressed_at")?,
        superseded_by: superseded_by_str.as_deref().map(|value| parse_memory_id(value, "superseded_by")).transpose()?,
        activity_mass: row.try_get("activity_mass")?,
        last_used_at: row.try_get("last_used_at")?,
        entities: Vec::new(),
        was_redacted: false,
    })
}

fn row_to_tombstone(row: &PgRow) -> Result<MemoryTombstone, StoreError> {
    let id_str: String = row.try_get("memory_id")?;
    let provenance: Json<Provenance> = row.try_get("provenance")?;
    let access_policy: Json<AccessPolicy> = row.try_get("access_policy")?;
    Ok(MemoryTombstone {
        memory_id: parse_memory_id(&id_str, "memory_tombstones.memory_id")?,
        provenance: provenance.0,
        access_policy: access_policy.0,
        deleted_at: row.try_get("deleted_at")?,
        deleted_by_principal: row.try_get("deleted_by_principal")?,
    })
}

fn parse_memory_id(raw: &str, field: &'static str) -> Result<MemoryId, StoreError> {
    raw.parse().map_err(|e| StoreError::Serialization(format!("invalid {field} memory id {raw:?}: {e}").into()))
}

fn validate_embedding_dimensions(embedding: &[f32], embedding_dimensions: usize) -> Result<(), StoreError> {
    validate_embedding_vector(embedding, embedding_dimensions)
}

async fn search_vector_batch(pool: &PgPool, embedding: &[f32], limit: usize) -> Result<VectorBatch, StoreError> {
    if limit == 0 {
        return Ok(VectorBatch {
            hits: Vec::new(),
            returned_count: 0,
        });
    }
    let vector = pgvector_literal(embedding);
    let limit = usize_to_i64(limit, "PostgreSQL vector candidate limit")?;
    let rows = sqlx::query(
        "
        SELECT memory_id, (embedding <-> $1::vector)::double precision AS distance
        FROM memory_embeddings
        ORDER BY embedding <-> $1::vector
        LIMIT $2
        ",
    )
    .bind(vector)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let returned_count = rows.len();
    let hits = rows.iter().filter_map(row_to_vector_hit).collect();
    Ok(VectorBatch { hits, returned_count })
}

async fn fetch_embeddings_for_ids(pool: &PgPool, ids: &[MemoryId]) -> Result<EmbeddingMap, StoreError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let id_strs = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    let rows = sqlx::query(
        "
        SELECT memory_id, embedding::text AS embedding
        FROM memory_embeddings
        WHERE memory_id = ANY($1)
        ",
    )
    .bind(id_strs)
    .fetch_all(pool)
    .await?;

    let mut result = HashMap::with_capacity(rows.len());
    for row in rows {
        let id_str: String = row.try_get("memory_id")?;
        let embedding_text: String = row.try_get("embedding")?;
        let id = parse_memory_id(&id_str, "memory_id")?;
        match parse_pgvector_text(&embedding_text) {
            Some(embedding) if !embedding.is_empty() => {
                let _previous = result.insert(id, embedding);
            }
            _ => tracing::warn!(memory_id = %id, embedding = embedding_text, "invalid PostgreSQL vector text in fetch_embeddings_for_ids"),
        }
    }
    Ok(result)
}

fn parse_pgvector_text(raw: &str) -> Option<Vec<f32>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner.split(',').map(|value| value.trim().parse::<f32>().ok()).collect()
}

fn row_to_vector_hit(row: &PgRow) -> Option<VectorHit> {
    let id_str = match row.try_get::<String, _>("memory_id") {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(error = %e, "missing memory_id in PostgreSQL vector result");
            return None;
        }
    };
    let distance = match row.try_get::<f64, _>("distance") {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(memory_id = id_str, error = %e, "missing distance in PostgreSQL vector result");
            return None;
        }
    };
    match id_str.parse::<MemoryId>() {
        Ok(memory_id) => Some(VectorHit { memory_id, distance }),
        Err(e) => {
            tracing::warn!(memory_id = id_str, error = %e, "invalid memory ID in PostgreSQL vector result");
            None
        }
    }
}

fn pgvector_literal(embedding: &[f32]) -> String {
    let mut vector = String::from("[");
    for (idx, value) in embedding.iter().enumerate() {
        if idx > 0 {
            vector.push(',');
        }
        vector.push_str(&value.to_string());
    }
    vector.push(']');
    vector
}

fn validate_bootstrap_inputs(config: &PostgresDatabaseConfig, embedding_dimensions: usize) -> Result<(), StoreError> {
    if embedding_dimensions == 0 {
        return Err(StoreError::Conflict("embedding dimensions must be greater than zero".into()));
    }
    if config.max_connections == 0 {
        return Err(StoreError::Conflict("database.postgres.max_connections must be greater than zero".into()));
    }
    Ok(())
}

async fn init_schema(pool: &PgPool, embedding_dimensions: usize) -> Result<(), StoreError> {
    execute_statement(pool, CREATE_VECTOR_EXTENSION).await?;
    execute_statement(pool, CREATE_MIGRATIONS_TABLE).await?;
    check_vector_dimensions(pool, embedding_dimensions).await?;
    execute_statements(pool, POSTGRES_SCHEMA_STATEMENTS).await?;
    migrate_embedding_claim_columns(pool).await?;
    migrate_audit_log_remove_memory_fk(pool).await?;
    execute_dynamic_statement(pool, &memory_embeddings_ddl(embedding_dimensions)?).await?;
    check_vector_dimensions(pool, embedding_dimensions).await?;
    record_migration(pool, 1_i64, "bootstrap_schema").await?;
    record_migration(pool, PostgresStore::CURRENT_SCHEMA_VERSION, "audit_log_without_memory_fk").await?;
    Ok(())
}

async fn migrate_embedding_claim_columns(pool: &PgPool) -> Result<(), StoreError> {
    if !embedding_claim_columns_exist(pool).await? {
        execute_statement(
            pool,
            "
            ALTER TABLE memories
                ADD COLUMN IF NOT EXISTS embedding_claimed_at TIMESTAMPTZ,
                ADD COLUMN IF NOT EXISTS embedding_claim_token TEXT
            ",
        )
        .await?;
    }
    execute_statement(
        pool,
        "CREATE INDEX IF NOT EXISTS idx_memories_embedding_claim ON memories(has_embedding, embedding_claimed_at, created_at, id) WHERE has_embedding = FALSE",
    )
    .await
}

async fn embedding_claim_columns_exist(pool: &PgPool) -> Result<bool, StoreError> {
    let count: i64 = sqlx::query_scalar(
        "
        SELECT COUNT(*)
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'memories'
          AND column_name IN ('embedding_claimed_at', 'embedding_claim_token')
        ",
    )
    .fetch_one(pool)
    .await?;
    Ok(count == 2_i64)
}

async fn migrate_audit_log_remove_memory_fk(pool: &PgPool) -> Result<(), StoreError> {
    execute_statement(
        pool,
        "
        DO $$
        DECLARE
            constraint_name text;
        BEGIN
            FOR constraint_name IN
                SELECT conname
                FROM pg_constraint
                WHERE conrelid = 'memory_audit_log'::regclass
                  AND confrelid = 'memories'::regclass
                  AND contype = 'f'
            LOOP
                EXECUTE format('ALTER TABLE memory_audit_log DROP CONSTRAINT %I', constraint_name);
            END LOOP;
        END $$
        ",
    )
    .await
}

async fn execute_statements(pool: &PgPool, statements: &[&'static str]) -> Result<(), StoreError> {
    for statement in statements {
        execute_statement(pool, statement).await?;
    }
    Ok(())
}

async fn execute_statement(pool: &PgPool, statement: &'static str) -> Result<(), StoreError> {
    let _result = sqlx::query(statement).execute(pool).await?;
    Ok(())
}

async fn execute_dynamic_statement(pool: &PgPool, statement: &str) -> Result<(), StoreError> {
    let _result = sqlx::query(AssertSqlSafe(statement)).execute(pool).await?;
    Ok(())
}

async fn upsert_embedding_profile_executor(tx: &mut Transaction<'_, Postgres>, profile: &EmbeddingProfile) -> Result<(), StoreError> {
    let dimensions = i64::try_from(profile.dimensions).map_err(|_error| StoreError::Conflict("embedding dimensions exceed PostgreSQL BIGINT".into()))?;
    let _result = sqlx::query(
        "INSERT INTO embedding_profile (singleton, provider, endpoint, model, dimensions)
         VALUES (1, $1, $2, $3, $4)
         ON CONFLICT (singleton) DO UPDATE SET
           provider = EXCLUDED.provider,
           endpoint = EXCLUDED.endpoint,
           model = EXCLUDED.model,
           dimensions = EXCLUDED.dimensions",
    )
    .bind(&profile.provider)
    .bind(&profile.endpoint)
    .bind(&profile.model)
    .bind(dimensions)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn lock_embedding_profile(tx: &mut Transaction<'_, Postgres>) -> Result<(), StoreError> {
    let _locked = sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(EMBEDDING_PROFILE_ADVISORY_LOCK)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn read_embedding_profile_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Option<EmbeddingProfile>, StoreError> {
    let row = sqlx::query("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1")
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|row| {
        let dimensions: i64 = row.try_get("dimensions")?;
        Ok(EmbeddingProfile {
            provider: row.try_get("provider")?,
            endpoint: row.try_get("endpoint")?,
            model: row.try_get("model")?,
            dimensions: usize::try_from(dimensions).map_err(|error| StoreError::Serialization(Box::new(error)))?,
        })
    })
    .transpose()
}

async fn ensure_embedding_profile_matches_tx(tx: &mut Transaction<'_, Postgres>, expected: &EmbeddingProfile) -> Result<(), StoreError> {
    let current = read_embedding_profile_tx(tx)
        .await?
        .ok_or_else(|| StoreError::Conflict("embedding profile was removed while this server was running; restart before writing vectors".into()))?;
    if current != *expected {
        return Err(profile_mismatch(&current, expected));
    }
    Ok(())
}

fn profile_mismatch(existing: &EmbeddingProfile, configured: &EmbeddingProfile) -> StoreError {
    StoreError::Conflict(format!(
        "embedding profile mismatch: database uses {} model '{}' at '{}' with {} dimensions, but config selects {} model '{}' at '{}' with {} dimensions; run `hold embeddings reindex --yes` to rebuild all vectors",
        existing.provider, existing.model, existing.endpoint, existing.dimensions, configured.provider, configured.model, configured.endpoint, configured.dimensions
    ))
}

fn memory_embeddings_ddl(embedding_dimensions: usize) -> Result<String, StoreError> {
    if embedding_dimensions == 0 {
        return Err(StoreError::Conflict("embedding dimensions must be greater than zero".into()));
    }
    Ok(format!(
        "
        CREATE TABLE IF NOT EXISTS memory_embeddings (
            memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
            embedding vector({embedding_dimensions}) NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "
    ))
}

async fn check_vector_dimensions(pool: &PgPool, embedding_dimensions: usize) -> Result<(), StoreError> {
    let existing_type: Option<String> = sqlx::query_scalar(
        "
        SELECT format_type(attribute.atttypid, attribute.atttypmod)
        FROM pg_attribute AS attribute
        WHERE attribute.attrelid = to_regclass('memory_embeddings')
          AND attribute.attname = 'embedding'
          AND NOT attribute.attisdropped
        ",
    )
    .fetch_optional(pool)
    .await?;

    let Some(existing_type) = existing_type else {
        return Ok(());
    };
    let Some(existing_dimensions) = parse_vector_dimensions(&existing_type) else {
        return Err(StoreError::Conflict(format!(
            "existing memory_embeddings.embedding type is {existing_type}, expected vector({embedding_dimensions})"
        )));
    };
    if existing_dimensions != embedding_dimensions {
        return Err(StoreError::Conflict(format!(
            "existing memory_embeddings table has {existing_dimensions} dimensions but config specifies {embedding_dimensions}; \
             create a new database or migrate embeddings before changing dimensions"
        )));
    }
    Ok(())
}

fn parse_vector_dimensions(formatted_type: &str) -> Option<usize> {
    let inner = formatted_type.strip_prefix("vector(")?.strip_suffix(')')?;
    inner.parse().ok()
}

async fn record_migration(pool: &PgPool, version: i64, name: &'static str) -> Result<(), StoreError> {
    let _result = sqlx::query(
        "
        INSERT INTO localhold_migrations (version, name)
        VALUES ($1, $2)
        ON CONFLICT (version) DO NOTHING
        ",
    )
    .bind(version)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;
    use crate::types::AuditAction;

    #[test]
    fn parse_vector_dimensions_extracts_pgvector_typmod() {
        assert_eq!(parse_vector_dimensions("vector(768)"), Some(768_usize));
    }

    #[test]
    fn parse_vector_dimensions_rejects_unbounded_vector_type() {
        assert_eq!(parse_vector_dimensions("vector"), None);
    }

    #[test]
    fn memory_embeddings_ddl_uses_configured_dimensions() {
        let ddl = memory_embeddings_ddl(384_usize).unwrap();
        assert!(ddl.contains("embedding vector(384) NOT NULL"), "DDL should include the configured vector dimensions");
    }

    #[test]
    fn memory_embeddings_ddl_rejects_zero_dimensions() {
        let err = memory_embeddings_ddl(0_usize).unwrap_err();
        assert!(err.to_string().contains("dimensions"), "error should mention dimensions");
    }

    #[test]
    fn pgvector_literal_formats_embedding_values() {
        assert_eq!(pgvector_literal(&[0.25_f32, -1.5_f32, 3.0_f32]), "[0.25,-1.5,3]");
    }

    #[test]
    fn memory_by_id_for_update_query_locks_selected_row() {
        let unlocked = fetch_memory_by_id_query(false);
        assert!(!unlocked.contains("FOR UPDATE"));

        let locked = fetch_memory_by_id_query(true);
        assert!(locked.ends_with(" FOR UPDATE"));
    }

    #[test]
    fn postgres_filter_conditions_push_core_predicates() {
        let filter = MemoryFilter {
            tags: Some(vec!["tag-a".into()]),
            agent_label: Some("agent-a".into()),
            scope: Some("scope-a".into()),
            origin_scope: Some("origin-a".into()),
            scopes_any: Some(vec!["scope-a".into(), "scope-b".into()]),
            text_search: Some("needle".into()),
            has_embedding: Some(true),
            memory_type: Some(MemoryType::Semantic),
            entity: Some("Entity A".into()),
            ..MemoryFilter::default()
        };
        let mut builder = QueryBuilder::<Postgres>::new("SELECT * FROM memories");
        let mut has_condition = false;
        push_postgres_filter_conditions(
            &mut builder,
            &filter,
            Some("agent-a"),
            Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).single().unwrap(),
            &mut has_condition,
        );

        let sql = builder.sql();
        let sql_text = sql.as_str();
        assert!(sql_text.contains("expires_at IS NULL OR expires_at >"));
        assert!(sql_text.contains("provenance->>'source_agent'"));
        assert!(sql_text.contains("provenance->>'source_conversation'"));
        assert!(sql_text.contains("COALESCE(provenance->>'origin_conversation'"));
        assert!(sql_text.contains("= ANY("));
        assert!(sql_text.contains("content ILIKE"));
        assert!(sql_text.contains("has_embedding ="));
        assert!(sql_text.contains("tags ?|"));
        assert!(sql_text.contains("memory_type ="));
        assert!(sql_text.contains("superseded_by IS NULL"));
        assert!(sql_text.contains("memory_entities me"));
        assert!(sql_text.contains("access_policy->>'type' = 'public'"));
        assert!(sql_text.contains("access_policy->>'type' = 'redacted'"));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn open_bootstraps_schema_against_postgres() {
        let url = std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into());
        let config = PostgresDatabaseConfig {
            url,
            max_connections: 1,
            auto_migrate: true,
        };

        let store = PostgresStore::open(&config, 3_usize).await.unwrap();
        assert_eq!(store.embedding_dimensions(), 3_usize);

        let has_vector_extension: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(has_vector_extension, "bootstrap should create the pgvector extension");

        let has_memory_table: bool = sqlx::query_scalar("SELECT to_regclass('memories') IS NOT NULL").fetch_one(store.pool()).await.unwrap();
        assert!(has_memory_table, "bootstrap should create the memories table");
        let has_tombstone_table: bool = sqlx::query_scalar("SELECT to_regclass('memory_tombstones') IS NOT NULL")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(has_tombstone_table, "bootstrap should create the tombstone table");
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn store_get_list_and_delete_against_postgres() {
        let store = open_postgres_smoke_store().await;
        let memory = test_memory();
        let id = store.store_impl(&memory, Some(&[0.1_f32, 0.2_f32, 0.3_f32])).await.unwrap();

        let hidden = store.get_impl(&id, None).await.unwrap();
        assert!(hidden.is_none(), "restricted memory should be hidden from anonymous reads");

        let fetched = store.get_impl(&id, Some("postgres-test-agent")).await.unwrap().unwrap();
        assert_eq!(fetched.content, memory.content);
        assert!(fetched.has_embedding);
        assert_eq!(fetched.entities, memory.entities);

        let listed = store
            .list_impl(
                MemoryFilter {
                    tags: Some(vec!["postgres-smoke".into()]),
                    scope: memory.provenance.source_conversation.clone(),
                    limit: Some(10_usize),
                    ..MemoryFilter::default()
                },
                QueryContext {
                    principal: Some("postgres-test-agent".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(listed.len(), 1_usize);
        assert_eq!(listed[0].id, id);

        assert!(store.delete_impl(&id).await.unwrap());
        assert!(store.get_impl(&id, Some("postgres-test-agent")).await.unwrap().is_none());
        let tombstone = store.get_tombstone_impl(&id).await.unwrap().unwrap();
        assert_eq!(tombstone.memory_id, id);
        assert_eq!(tombstone.provenance.source_agent, memory.provenance.source_agent);
        assert_eq!(tombstone.deleted_by_principal, None);
        assert!(!store.delete_impl(&id).await.unwrap());
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn text_and_fts_search_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        let mut visible = test_memory_with_content("postgres search visible nebula literal 100% token");
        visible.tags = vec!["postgres-search".into()];
        let visible_id = store.store_impl(&visible, None).await.unwrap();

        let mut wildcard_decoy = test_memory_with_content("postgres search visible literal 100X token");
        wildcard_decoy.tags = vec!["postgres-search".into()];
        let decoy_id = store.store_impl(&wildcard_decoy, None).await.unwrap();

        let mut hidden = test_memory_with_content("postgres search hidden nebula literal 100% token");
        hidden.tags = vec!["postgres-search".into()];
        hidden.provenance.source_agent = Some("other-agent".into());
        hidden.access_policy = AccessPolicy::Restricted {
            allowed: vec!["third-agent".into()],
        };
        let hidden_id = store.store_impl(&hidden, None).await.unwrap();

        let filter = MemoryFilter {
            tags: Some(vec!["postgres-search".into()]),
            ..MemoryFilter::default()
        };
        let ctx = QueryContext {
            principal: Some("postgres-test-agent".into()),
        };

        let text_results = store.search_by_text_impl("100%", 10_usize, filter.clone(), ctx.clone()).await.unwrap();
        assert_eq!(text_results.len(), 1_usize);
        assert_eq!(text_results[0].memory.id, visible_id);
        assert!(text_results[0].distance.is_none());
        assert_ne!(text_results[0].memory.id, decoy_id);
        assert_ne!(text_results[0].memory.id, hidden_id);

        let fts_results = store.search_by_fts_impl("nebula", 10_usize, filter, ctx, None).await.unwrap();
        assert_eq!(fts_results.len(), 1_usize);
        assert_eq!(fts_results[0].memory.id, visible_id);
        assert!(fts_results[0].distance.is_some());

        assert!(
            store
                .search_by_text_impl("nebula", 0_usize, MemoryFilter::default(), QueryContext::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .search_by_fts_impl("nebula", 0_usize, MemoryFilter::default(), QueryContext::default(), None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn filtered_list_count_and_text_search_find_rows_beyond_old_scan_window_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        let older = Utc.with_ymd_and_hms(2026, 5, 8, 10, 0, 0).single().unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 5, 8, 11, 0, 0).single().unwrap();
        let mut target = test_memory_with_content("postgres filter pushdown needle target");
        target.tags = vec!["postgres-filter-target".into()];
        target.created_at = older;
        target.updated_at = older;
        let target_id = store.store_impl(&target, None).await.unwrap();

        insert_filter_pushdown_decoys(&store, MAX_SCAN_ROWS.saturating_add(1), newer).await;

        let ctx = QueryContext {
            principal: Some("postgres-test-agent".into()),
        };
        let target_filter = MemoryFilter {
            tags: Some(vec!["postgres-filter-target".into()]),
            limit: Some(1_usize),
            ..MemoryFilter::default()
        };

        let listed = store.list_impl(target_filter.clone(), ctx.clone()).await.unwrap();
        assert_eq!(listed.iter().map(|memory| memory.id).collect::<Vec<_>>(), vec![target_id]);

        let stats = store
            .count_impl(
                MemoryFilter {
                    tags: Some(vec!["postgres-filter-target".into()]),
                    ..MemoryFilter::default()
                },
                ctx.clone(),
                10_usize,
            )
            .await
            .unwrap();
        assert_eq!(stats.total, 1_u64);

        let text_results = store.search_by_text_impl("needle", 1_usize, target_filter, ctx).await.unwrap();
        assert_eq!(text_results.iter().map(|result| result.memory.id).collect::<Vec<_>>(), vec![target_id]);
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn semantic_search_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        let mut visible = test_memory_with_content("postgres semantic visible");
        visible.tags = vec!["postgres-semantic".into()];
        let visible_id = store.store_impl(&visible, Some(&[0.0_f32, 0.0_f32, 0.0_f32])).await.unwrap();

        let mut hidden = test_memory_with_content("postgres semantic hidden");
        hidden.tags = vec!["postgres-semantic".into()];
        hidden.provenance.source_agent = Some("other-agent".into());
        hidden.access_policy = AccessPolicy::Restricted {
            allowed: vec!["third-agent".into()],
        };
        let _hidden_id = store.store_impl(&hidden, Some(&[0.0_f32, 0.1_f32, 0.0_f32])).await.unwrap();

        let mut distant = test_memory_with_content("postgres semantic distant");
        distant.tags = vec!["postgres-semantic".into()];
        let _distant_id = store.store_impl(&distant, Some(&[1.0_f32, 1.0_f32, 1.0_f32])).await.unwrap();

        let filter = MemoryFilter {
            tags: Some(vec!["postgres-semantic".into()]),
            ..MemoryFilter::default()
        };
        let ctx = QueryContext {
            principal: Some("postgres-test-agent".into()),
        };

        let results = store
            .search_by_embedding_impl(&[0.0_f32, 0.0_f32, 0.0_f32], 10_usize, filter.clone(), ctx.clone(), Some(0.05_f64))
            .await
            .unwrap();
        assert_eq!(results.len(), 1_usize);
        assert_eq!(results[0].memory.id, visible_id);
        assert!(results[0].distance.is_some_and(|distance| distance <= 0.05_f64));

        assert!(
            store
                .search_by_embedding_impl(&[0.0_f32, 0.0_f32, 0.0_f32], 0_usize, filter, ctx, None)
                .await
                .unwrap()
                .is_empty()
        );
        let err = store
            .search_by_embedding_impl(&[0.0_f32, 0.0_f32], 10_usize, MemoryFilter::default(), QueryContext::default(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("embedding dimension mismatch"));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn postgres_store_satisfies_memory_store_conformance() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        crate::store::conformance::assert_memory_store_contract(&store, 3_usize).await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn postgres_store_rejects_non_finite_embeddings() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        crate::store::conformance::assert_non_finite_embeddings_rejected(&store, 3_usize).await;
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn reembed_embeddings_and_neighbors_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        let no_embedding = test_memory_with_content("postgres needs reembed");
        let no_embedding_id = store.store_impl(&no_embedding, None).await.unwrap();
        let reembed = store.list_for_reembed_impl(10_usize).await.unwrap();
        assert_eq!(reembed.len(), 1_usize);
        assert_eq!(reembed[0].0, no_embedding_id);
        assert_eq!(reembed[0].1, "postgres needs reembed");
        assert_eq!(
            store.get_for_reembed_impl(&no_embedding_id, "postgres-test-agent").await.unwrap(),
            Some(("postgres needs reembed".into(), 0_i64))
        );
        assert!(store.get_for_reembed_impl(&no_embedding_id, "intruder").await.unwrap().is_none());

        let first_claim = store.claim_for_reembed_impl(10_usize).await.unwrap();
        assert_eq!(first_claim.len(), 1_usize);
        assert_eq!(first_claim[0].id, no_embedding_id);
        assert!(store.claim_for_reembed_impl(10_usize).await.unwrap().is_empty());

        let _result = sqlx::query("UPDATE memories SET embedding_claimed_at = NOW() - INTERVAL '301 seconds' WHERE id = $1")
            .bind(no_embedding_id.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        let expired_claim = store.claim_for_reembed_impl(10_usize).await.unwrap();
        assert_eq!(expired_claim.len(), 1_usize);
        assert_ne!(expired_claim[0].claim_token, first_claim[0].claim_token);
        assert!(
            store
                .release_embedding_claim_impl(&expired_claim[0].id, expired_claim[0].embedding_revision, &expired_claim[0].claim_token)
                .await
                .unwrap()
        );

        store.record_search_impression_impl(&[no_embedding_id]).await.unwrap();
        let impressed = store.get_impl(&no_embedding_id, Some("postgres-test-agent")).await.unwrap().unwrap();
        assert_eq!(impressed.impression_count, 1_u64);
        assert!(impressed.last_impressed_at.is_some());

        let scope = format!("postgres-neighbor/{}", MemoryId::new());
        let mut base = test_memory_with_content("postgres neighbor base");
        base.provenance.source_conversation = Some(scope.clone());
        let base_id = store.store_impl(&base, Some(&[0.0_f32, 0.0_f32, 0.0_f32])).await.unwrap();
        let mut neighbor = test_memory_with_content("postgres neighbor nearby");
        neighbor.provenance.source_conversation = Some(scope.clone());
        let neighbor_id = store.store_impl(&neighbor, Some(&[0.1_f32, 0.0_f32, 0.0_f32])).await.unwrap();
        let mut superseded = test_memory_with_content("postgres neighbor superseded");
        superseded.provenance.source_conversation = Some(scope.clone());
        let superseded_id = store.store_impl(&superseded, Some(&[0.05_f32, 0.0_f32, 0.0_f32])).await.unwrap();
        assert!(store.mark_superseded_by_impl(&superseded_id, &base_id).await.unwrap());

        let embeddings = store.fetch_embeddings_for_ids_impl(&[base_id, neighbor_id, no_embedding_id]).await.unwrap();
        assert_eq!(embeddings.get(&base_id), Some(&vec![0.0_f32, 0.0_f32, 0.0_f32]));
        assert_eq!(embeddings.get(&neighbor_id), Some(&vec![0.1_f32, 0.0_f32, 0.0_f32]));
        assert!(!embeddings.contains_key(&no_embedding_id));

        let scoped = store.list_with_embeddings_impl(Some(std::slice::from_ref(&scope)), 10_usize).await.unwrap();
        let scoped_ids = scoped.iter().map(|entry| entry.memory.id).collect::<Vec<_>>();
        assert!(scoped_ids.contains(&base_id));
        assert!(scoped_ids.contains(&neighbor_id));
        assert!(!scoped_ids.contains(&superseded_id));
        assert!(scoped.iter().all(|entry| entry.embedding.is_some()));

        let neighbors = store.find_embedding_neighbors_impl(&[0.0_f32, 0.0_f32, 0.0_f32], 0.2_f64, 10_usize).await.unwrap();
        assert!(neighbors.iter().any(|(id, distance)| *id == neighbor_id && *distance <= 0.2_f64));
        assert!(!neighbors.iter().any(|(id, _)| *id == superseded_id));
        assert!(
            store
                .find_embedding_neighbors_impl(&[0.0_f32, 0.0_f32, 0.0_f32], 0.2_f64, 0_usize)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn write_authorization_and_embedding_against_postgres() {
        let store = open_postgres_smoke_store().await;
        let mut memory = test_memory_with_content("postgres writable memory");
        memory.tags = vec!["postgres-write".into()];
        let id = store.store_impl(&memory, Some(&[0.1_f32, 0.2_f32, 0.3_f32])).await.unwrap();

        let denied = store
            .update_authorized_impl(
                &id,
                &MemoryUpdate {
                    tags: Some(vec!["denied".into()]),
                    ..MemoryUpdate::default()
                },
                "intruder",
            )
            .await
            .unwrap();
        assert_eq!(denied.outcome, WriteOutcome::Denied);

        let updated_entity = Entity::new("Updated Entity", "test").unwrap();
        let new_scope = format!("postgres-updated/{}", MemoryId::new());
        let updated = store
            .update_authorized_impl(
                &id,
                &MemoryUpdate {
                    content: Some("postgres updated content".into()),
                    tags: Some(vec!["postgres-updated".into()]),
                    source_conversation: Some(new_scope.clone()),
                    entities: Some(vec![updated_entity.clone()]),
                    ..MemoryUpdate::default()
                },
                "postgres-test-agent",
            )
            .await
            .unwrap();
        assert_eq!(updated.outcome, WriteOutcome::Applied);
        assert_eq!(updated.reembed_revision, Some(1_i64));

        let fetched = store.get_impl(&id, Some("postgres-test-agent")).await.unwrap().unwrap();
        assert_eq!(fetched.content, "postgres updated content");
        assert_eq!(fetched.tags, vec!["postgres-updated"]);
        assert_eq!(fetched.provenance.source_conversation.as_deref(), Some(new_scope.as_str()));
        assert_eq!(fetched.entities, vec![updated_entity]);
        assert!(!fetched.has_embedding, "content updates should clear stale embeddings");

        let embedding_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_embeddings WHERE memory_id = $1)")
            .bind(id.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(!embedding_exists, "stale vector row should be removed after content update");

        store
            .set_embedding_impl(&id, &[0.3_f32, 0.2_f32, 0.1_f32], updated.reembed_revision.unwrap())
            .await
            .unwrap();
        assert!(store.get_impl(&id, Some("postgres-test-agent")).await.unwrap().unwrap().has_embedding);
        let stale_revision = store.set_embedding_impl(&id, &[0.3_f32, 0.2_f32, 0.1_f32], 0_i64).await.unwrap_err();
        assert!(stale_revision.to_string().contains("revision mismatch"));

        let plain_update_applied = store
            .update_impl(&id, &MemoryUpdate {
                confidence: Some(crate::types::Confidence::new(0.7_f64)),
                ..MemoryUpdate::default()
            })
            .await
            .unwrap();
        assert!(plain_update_applied);

        let delete_target = test_memory_with_content("postgres authorized delete target");
        let _delete_target_id = store.store_impl(&delete_target, None).await.unwrap();
        let delete_denied = store.delete_authorized_impl(&delete_target.id, "intruder").await.unwrap();
        assert_eq!(delete_denied, WriteOutcome::Denied);
        let delete_applied = store.delete_authorized_impl(&delete_target.id, "postgres-test-agent").await.unwrap();
        assert_eq!(delete_applied, WriteOutcome::Applied);
        let tombstone = store.get_tombstone_impl(&delete_target.id).await.unwrap().unwrap();
        assert_eq!(tombstone.memory_id, delete_target.id);
        assert_eq!(tombstone.provenance.source_agent, delete_target.provenance.source_agent);
        assert_eq!(tombstone.deleted_by_principal.as_deref(), Some("postgres-test-agent"));
        assert_eq!(
            serde_json::to_value(&tombstone.access_policy).unwrap(),
            serde_json::to_value(&delete_target.access_policy).unwrap()
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn batch_and_bulk_write_authorization_against_postgres() {
        let store = open_postgres_smoke_store().await;
        let batch_one = test_memory_with_content("postgres batch one");
        let batch_two = test_memory_with_content("postgres batch two");
        let batch_ids = store
            .store_batch_impl(&[
                MemoryWithEmbedding {
                    memory: batch_one.clone(),
                    embedding: None,
                },
                MemoryWithEmbedding {
                    memory: batch_two.clone(),
                    embedding: Some(vec![0.4_f32, 0.5_f32, 0.6_f32]),
                },
            ])
            .await
            .unwrap();
        assert_eq!(batch_ids, vec![batch_one.id, batch_two.id]);

        let bulk_denied = store.bulk_delete_ids_impl(batch_ids.clone(), "intruder").await.unwrap();
        assert_eq!(bulk_denied.applied_ids, Vec::<MemoryId>::new());
        assert_eq!(bulk_denied.denied, 2_u64);

        let bulk_update = store
            .bulk_update_ids_impl(
                batch_ids.clone(),
                MemoryUpdate {
                    tags: Some(vec!["bulk-updated".into()]),
                    ..MemoryUpdate::default()
                },
                "postgres-test-agent",
                Utc.with_ymd_and_hms(2026, 5, 8, 13, 0, 0).single().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bulk_update.applied_ids, batch_ids);
        assert_eq!(bulk_update.denied, 0_u64);

        let bulk_delete = store.bulk_delete_ids_impl(vec![batch_one.id, batch_two.id], "postgres-test-agent").await.unwrap();
        assert_eq!(bulk_delete.applied_ids, vec![batch_one.id, batch_two.id]);
        assert_eq!(bulk_delete.denied, 0_u64);
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn supersession_against_postgres() {
        let store = open_postgres_smoke_store().await;
        let old_memory = test_memory_with_content("postgres old superseded memory");
        let old_id = store.store_impl(&old_memory, None).await.unwrap();
        let new_memory = test_memory_with_content("postgres new superseding memory");
        let new_id = store.store_with_supersession_impl(&new_memory, None, &old_id).await.unwrap();
        let superseded = fetch_memory_by_id(store.pool(), &old_id).await.unwrap().unwrap();
        assert_eq!(superseded.superseded_by, Some(new_id));
        let already_superseded = store.mark_superseded_by_impl(&old_id, &new_id).await.unwrap_err();
        assert!(already_superseded.to_string().contains("already superseded"));
        assert!(!store.mark_superseded_by_impl(&MemoryId::new(), &new_id).await.unwrap());

        let auth_old = test_memory_with_content("postgres authorized supersession old");
        let auth_new = test_memory_with_content("postgres authorized supersession new");
        let _auth_old_id = store.store_impl(&auth_old, None).await.unwrap();
        let _auth_new_id = store.store_impl(&auth_new, None).await.unwrap();
        let denied = store.mark_superseded_by_authorized_impl(&auth_old.id, &auth_new.id, "intruder").await.unwrap();
        assert_eq!(denied, WriteOutcome::Denied);
        let applied = store.mark_superseded_by_authorized_impl(&auth_old.id, &auth_new.id, "postgres-test-agent").await.unwrap();
        assert_eq!(applied, WriteOutcome::Applied);

        let batch_old = test_memory_with_content("postgres batch supersession old");
        let _batch_old_id = store.store_impl(&batch_old, None).await.unwrap();
        let batch_new = test_memory_with_content("postgres batch supersession new");
        let batch_plain = test_memory_with_content("postgres batch plain new");
        let supersession_ids = store
            .store_batch_with_supersession_impl(
                &[
                    MemoryWithEmbedding {
                        memory: batch_new.clone(),
                        embedding: None,
                    },
                    MemoryWithEmbedding {
                        memory: batch_plain.clone(),
                        embedding: None,
                    },
                ],
                &[Some(batch_old.id), None],
            )
            .await
            .unwrap();
        assert_eq!(supersession_ids, vec![batch_new.id, batch_plain.id]);
        let batch_superseded = fetch_memory_by_id(store.pool(), &batch_old.id).await.unwrap().unwrap();
        assert_eq!(batch_superseded.superseded_by, Some(batch_new.id));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn batch_supersession_rejects_missing_target_against_postgres() {
        let store = open_postgres_smoke_store().await;
        let new_memory = test_memory_with_content("postgres rejected batch supersession new");
        let err = store
            .store_batch_with_supersession_impl(
                &[MemoryWithEmbedding {
                    memory: new_memory.clone(),
                    embedding: None,
                }],
                &[Some(MemoryId::new())],
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("superseded memory not found"));
        assert!(store.get_impl(&new_memory.id, Some("postgres-test-agent")).await.unwrap().is_none());
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn audit_and_scope_registry_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;
        let memory = test_memory_with_content("postgres audit scope memory");
        let id = store.store_impl(&memory, None).await.unwrap();
        let timestamp = Utc.with_ymd_and_hms(2026, 5, 8, 14, 0, 0).single().unwrap();
        let details = serde_json::json!({"field": "content"});

        let store_entry = AuditEntry {
            action: AuditAction::Store,
            caller_agent: Some("postgres-test-agent".to_owned()),
            timestamp,
            details: Some(details.clone()),
        };
        store.write_audit_entry_impl(&id, &store_entry).await.unwrap();
        let update_entry = AuditEntry {
            action: AuditAction::Update,
            caller_agent: None,
            timestamp,
            details: None,
        };
        store.write_audit_entry_impl(&id, &update_entry).await.unwrap();
        let audit = store.query_audit_log_impl(&id, 10_usize).await.unwrap();
        assert_eq!(audit.len(), 2_usize);
        assert_eq!(audit[0].action, AuditAction::Store);
        assert_eq!(audit[0].caller_agent.as_deref(), Some("postgres-test-agent"));
        assert_eq!(audit[0].details.as_ref(), Some(&details));
        assert_eq!(audit[1].action, AuditAction::Update);

        let scope = ScopeDefinition {
            scope_key: format!("postgres-scope/{}", MemoryId::new()),
            display_name: "Postgres Scope".into(),
            description: Some("scope description".into()),
            aliases: vec!["pg-alias".into()],
            matchers: vec!["/tmp/postgres-scope".into()],
            parent: Some("postgres-parent".into()),
            related: vec!["postgres-related".into()],
        };
        store.register_scope_impl(scope.clone()).await.unwrap();
        assert_eq!(store.list_scopes_impl().await.unwrap(), vec![scope]);
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn metadata_and_migration_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;
        let memory = test_memory_with_content("postgres metadata memory");
        let id = store.store_impl(&memory, None).await.unwrap();
        let scope_key = format!("postgres-migration/{}", MemoryId::new());

        let metadata = MemoryMetadata {
            memory_id: id,
            scope_key: Some(scope_key.clone()),
            summary: Some("metadata summary".into()),
            agent_label: Some("postgres-agent-label".into()),
            created_by_principal: Some("creator".into()),
            quality_flags: vec!["manual".into()],
            schema_version: 1,
        };
        store.upsert_metadata_impl(metadata.clone()).await.unwrap();
        assert_eq!(store.get_metadata_impl(&id).await.unwrap(), Some(metadata.clone()));

        let rejected_memory = test_memory_with_content("postgres mismatched metadata memory");
        let wrong_metadata = MemoryMetadata {
            memory_id: id,
            scope_key: Some(scope_key.clone()),
            summary: Some("wrong target".into()),
            agent_label: Some("postgres-agent-label".into()),
            created_by_principal: Some("creator".into()),
            quality_flags: vec!["manual".into()],
            schema_version: 1,
        };
        let err = store.store_with_metadata_impl(&rejected_memory, None, None, &wrong_metadata).await.unwrap_err();
        assert!(err.to_string().contains("metadata memory_id"));
        let rejected_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memories WHERE id = $1)")
            .bind(rejected_memory.id.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(!rejected_exists);
        assert_eq!(store.get_metadata_impl(&id).await.unwrap(), Some(metadata));

        let mut registered_memory = test_memory_with_content("postgres registered migration memory");
        registered_memory.provenance.source_conversation = Some(scope_key.clone());
        let registered_id = store.store_impl(&registered_memory, None).await.unwrap();
        let mut unresolved_memory = test_memory_with_content("fn postgres_code_dump() {}");
        unresolved_memory.provenance.source_conversation = None;
        let unresolved_id = store.store_impl(&unresolved_memory, None).await.unwrap();

        let report = store.metadata_migration_report_impl().await.unwrap();
        assert_eq!(report.total_memories, 3_u64);
        assert_eq!(report.metadata_rows, 1_u64);
        assert_eq!(report.missing_metadata, 2_u64);
        assert_eq!(report.unresolved_scope, 1_u64);

        let dry_run = store.migrate_metadata_impl(std::slice::from_ref(&scope_key), true).await.unwrap();
        assert_eq!(dry_run.candidate_count, 2_u64);
        assert_eq!(dry_run.skipped_existing, 1_u64);
        assert_eq!(dry_run.migrated, 0_u64);
        assert_eq!(dry_run.unresolved_scope, 1_u64);
        assert_eq!(dry_run.code_derived, 1_u64);

        let applied = store.migrate_metadata_impl(std::slice::from_ref(&scope_key), false).await.unwrap();
        assert_eq!(applied.migrated, 2_u64);
        let registered_metadata = store.get_metadata_impl(&registered_id).await.unwrap().unwrap();
        assert_eq!(registered_metadata.scope_key.as_deref(), Some(scope_key.as_str()));
        assert_eq!(registered_metadata.quality_flags, vec!["missing_summary"]);
        let unresolved_metadata = store.get_metadata_impl(&unresolved_id).await.unwrap().unwrap();
        assert_eq!(unresolved_metadata.scope_key.as_deref(), Some(UNRESOLVED_SCOPE));
        assert!(unresolved_metadata.quality_flags.contains(&"missing_scope".to_owned()));
        assert!(unresolved_metadata.quality_flags.contains(&"possible_code_dump".to_owned()));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn postgres_store_batch_with_metadata_rejects_supersedes_length_mismatch() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;
        let first = test_memory_with_content("postgres batch supersedes mismatch one");
        let second = test_memory_with_content("postgres batch supersedes mismatch two");
        let memories = vec![
            MemoryWithEmbedding {
                memory: first.clone(),
                embedding: None,
            },
            MemoryWithEmbedding {
                memory: second.clone(),
                embedding: None,
            },
        ];
        let metadata = vec![
            MemoryMetadata {
                memory_id: first.id,
                scope_key: Some("postgres-batch-mismatch".into()),
                summary: Some("batch first".into()),
                agent_label: Some("postgres-agent-label".into()),
                created_by_principal: Some("creator".into()),
                quality_flags: vec!["manual".into()],
                schema_version: 1,
            },
            MemoryMetadata {
                memory_id: second.id,
                scope_key: Some("postgres-batch-mismatch".into()),
                summary: Some("batch second".into()),
                agent_label: Some("postgres-agent-label".into()),
                created_by_principal: Some("creator".into()),
                quality_flags: vec!["manual".into()],
                schema_version: 1,
            },
        ];

        let err = store.store_batch_with_metadata_impl(&memories, &[None], &metadata).await.unwrap_err();

        assert!(err.to_string().contains("supersedes length"));
    }

    #[tokio::test]
    #[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn memory_store_traits_and_admin_helpers_against_postgres() {
        let store = open_postgres_smoke_store().await;
        reset_postgres_smoke_database(&store).await;

        let from_scope = format!("postgres-trait-from/{}", MemoryId::new());
        let to_scope = format!("postgres-trait-to/{}", MemoryId::new());

        let mut visible = test_memory_with_content("postgres trait visible");
        visible.tags = vec!["postgres-trait".into()];
        visible.provenance.source_conversation = Some(from_scope.clone());
        let visible_id = MemoryWriter::store(&store, &visible, None).await.unwrap();

        let mut hidden = test_memory_with_content("postgres trait hidden");
        hidden.tags = vec!["postgres-trait".into()];
        hidden.provenance.source_agent = Some("other-agent".into());
        hidden.provenance.source_conversation = Some(from_scope.clone());
        hidden.access_policy = AccessPolicy::Restricted {
            allowed: vec!["third-agent".into()],
        };
        let hidden_id = MemoryWriter::store(&store, &hidden, None).await.unwrap();

        let mut expired = test_memory_with_content("postgres trait expired");
        expired.tags = vec!["postgres-trait".into()];
        expired.provenance.source_conversation = Some(format!("postgres-trait-expired/{}", MemoryId::new()));
        expired.expires_at = Some(Utc.with_ymd_and_hms(2026, 5, 8, 11, 0, 0).single().unwrap());
        let expired_id = MemoryWriter::store(&store, &expired, None).await.unwrap();

        let stats = MemoryReader::count(
            &store,
            MemoryFilter {
                tags: Some(vec!["postgres-trait".into()]),
                ..MemoryFilter::default()
            },
            QueryContext {
                principal: Some("postgres-test-agent".into()),
            },
            10_usize,
        )
        .await
        .unwrap();
        assert_eq!(stats.total, 1_u64);
        assert_eq!(stats.with_embedding, 0_u64);
        assert_eq!(stats.without_embedding, 1_u64);
        assert_eq!(stats.expired, 1_u64);
        assert_eq!(stats.by_tag, vec![("postgres-trait".into(), 1_u64)]);
        assert_eq!(stats.scope_count, 1_u64);

        let use_now = Utc.with_ymd_and_hms(2026, 5, 8, 16, 0, 0).single().unwrap();
        let use_outcome = MemoryWriter::record_memory_use(
            &store,
            &[visible_id, visible_id, hidden_id, MemoryId::new()],
            "postgres-test-agent",
            2.0_f64,
            use_now,
            24.0_f64,
        )
        .await
        .unwrap();
        assert_eq!(use_outcome.recorded, 1_u64);
        assert_eq!(use_outcome.denied, 1_u64);
        assert_eq!(use_outcome.not_found, 1_u64);

        let used = MemoryReader::get(&store, &visible_id, Some("postgres-test-agent")).await.unwrap().unwrap();
        assert_eq!(used.last_used_at, Some(use_now));
        assert!(used.activity_mass > 0.0_f64);

        MemoryWriter::write_audit_entry(&store, &visible_id, AuditAction::Store, Some("postgres-test-agent"), use_now, None)
            .await
            .unwrap();
        let audit = MemoryReader::query_audit_log(&store, &visible_id, 10_usize).await.unwrap();
        assert_eq!(audit.len(), 1_usize);
        assert_eq!(audit[0].action, AuditAction::Store);

        let reassigned = MemoryAdmin::reassign_scope(&store, &from_scope, &to_scope, None, "postgres-test-agent").await.unwrap();
        assert_eq!(reassigned.applied_ids, vec![visible_id]);
        let moved = MemoryReader::get(&store, &visible_id, Some("postgres-test-agent")).await.unwrap().unwrap();
        assert_eq!(moved.provenance.source_conversation.as_deref(), Some(to_scope.as_str()));
        assert_eq!(moved.provenance.origin_conversation.as_deref(), Some(from_scope.as_str()));

        assert_eq!(MemoryAdmin::evict_expired(&store).await.unwrap(), 1_u64);
        let expired_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memories WHERE id = $1)")
            .bind(expired_id.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert!(!expired_exists);
    }

    async fn open_postgres_smoke_store() -> PostgresStore {
        let url = std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into());
        let config = PostgresDatabaseConfig {
            url,
            max_connections: 1,
            auto_migrate: true,
        };
        PostgresStore::open(&config, 3_usize).await.unwrap()
    }

    async fn reset_postgres_smoke_database(store: &PostgresStore) {
        let _ = sqlx::query(
            "
            TRUNCATE TABLE
                memory_audit_log,
                memory_tombstones,
                memory_metadata,
                memory_entities,
                memory_embeddings,
                memories,
                scope_registry
            RESTART IDENTITY CASCADE
            ",
        )
        .execute(store.pool())
        .await
        .unwrap();
    }

    async fn insert_filter_pushdown_decoys(store: &PostgresStore, count: usize, created_at: DateTime<Utc>) {
        let ids = std::iter::repeat_with(|| MemoryId::new().to_string()).take(count).collect::<Vec<_>>();
        let _ = sqlx::query(
            "
            INSERT INTO memories (
                id, content, tags, provenance, access_policy, created_at,
                updated_at, has_embedding, memory_type, importance, confidence
            )
            SELECT ids.id,
                   $2::text,
                   $3::jsonb,
                   $4::jsonb,
                   $5::jsonb,
                   $6::timestamptz,
                   $6::timestamptz,
                   FALSE,
                   'semantic',
                   0.5,
                   0.8
            FROM UNNEST($1::text[]) AS ids(id)
            ",
        )
        .bind(ids)
        .bind("postgres filter pushdown needle decoy")
        .bind(Json(vec!["postgres-filter-decoy".to_owned()]))
        .bind(Json(Provenance {
            source_agent: Some("postgres-test-agent".into()),
            source_conversation: Some("postgres-filter-pushdown".into()),
            origin_conversation: None,
            source_user: None,
        }))
        .bind(Json(AccessPolicy::Public))
        .bind(created_at)
        .execute(store.pool())
        .await
        .unwrap();
    }

    fn test_memory() -> Memory {
        let now = Utc.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).single().unwrap();
        let scope = format!("postgres-smoke/{}", MemoryId::new());
        Memory {
            id: MemoryId::new(),
            content: "postgres smoke memory".into(),
            tags: vec!["postgres-smoke".into()],
            provenance: Provenance {
                source_agent: Some("postgres-test-agent".into()),
                source_conversation: Some(scope),
                origin_conversation: None,
                source_user: None,
            },
            access_policy: AccessPolicy::Restricted {
                allowed: vec!["postgres-test-agent".into()],
            },
            created_at: now,
            updated_at: now,
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::Semantic,
            importance: crate::types::Importance::new(0.75_f64),
            confidence: crate::types::Confidence::new(0.9_f64),
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: vec![Entity::new("Postgres Smoke", "test").unwrap()],
            was_redacted: false,
        }
    }

    fn test_memory_with_content(content: &str) -> Memory {
        let mut memory = test_memory();
        memory.content = content.into();
        memory
    }
}
