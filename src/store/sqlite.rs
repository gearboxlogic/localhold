//! `SqliteStore` — struct definition, connection lifecycle, schema initialization,
//! and `MemoryReader`/`MemoryWriter`/`MemoryAdmin` trait implementations (delegating to submodules).

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, Transaction, TransactionBehavior, ffi::sqlite3_auto_extension};
use sqlite_vec::sqlite3_vec_init;

use super::{
    EmbeddingProfile, MemoryAdmin, MemoryReader, MemoryWithEmbedding, MemoryWriter,
    migration::{reject_retired_sqlite_schema, validate_sqlite_source_schema},
    sqlite_lease::SqliteDatabaseLease,
    vector::{SqliteVecIndex, VectorIndex as _},
};
use crate::{
    clock::{Clock, SystemClock},
    error::StoreError,
    store::schema::{
        MAIN_DDL, TRIGGER_DDL, existing_embedding_dimensions, migrate_create_audit_log, migrate_create_fts_index, migrate_create_memory_entities, migrate_create_memory_tombstones,
        migrate_create_metadata, migrate_create_scope_registry, migrate_memories_add_activity_tracking, migrate_memories_add_confidence, migrate_memories_add_embedding_claims,
        migrate_memories_add_embedding_revision, migrate_memories_add_importance, migrate_memories_add_memory_type, migrate_memories_add_superseded_by,
        migrate_memories_add_updated_at, migrate_memories_align_impression_tracking, migrate_memories_backfill_origin_conversation, migrate_memory_embedding_map_fk,
    },
    types::{
        AuditAction, AuditDraft, AuditEntry, AuthorizedUpdateOutcome, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryStats, MemoryTombstone, MemoryUpdate,
        MetadataMigrationOutcome, MetadataMigrationReport, QueryContext, ScopeDefinition, SearchResult, WriteOutcome,
    },
};

/// Wrap any error type into a [`StoreError::MigrationFailed`] to preserve the source chain.
fn migration_failed(step: &'static str, source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> StoreError {
    StoreError::MigrationFailed { step, source: source.into() }
}

#[derive(Debug)]
struct SqliteInner {
    conn: Mutex<Connection>,
    vector_index: SqliteVecIndex,
    clock: Arc<dyn Clock>,
    /// Whether FTS5 is available in this SQLite build. Set once during schema init.
    fts_available: AtomicBool,
    active_embedding_profile: RwLock<Option<EmbeddingProfile>>,
    /// Shared process-lifetime lease that prevents an online restore from
    /// replacing this database while the connection is open.
    _database_lease: Option<SqliteDatabaseLease>,
}

pub(crate) const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Start a SQLite write transaction that serializes read-modify-write work
/// against writers in other server processes.
pub(crate) fn sqlite_write_tx(conn: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(StoreError::from)
}

/// SQLite-backed memory store with sqlite-vec vector indexing.
#[derive(Clone, Debug)]
pub struct SqliteStore {
    inner: Arc<SqliteInner>,
}

impl SqliteStore {
    /// Default embedding dimensions for test stores (nomic-embed-text).
    #[cfg(any(test, feature = "testing"))]
    pub const DEFAULT_TEST_DIMENSIONS: usize = 768;
    /// Static assertion: verify `sqlite3_vec_init` is an `unsafe extern "C" fn`.
    ///
    /// The `sqlite-vec` crate exports `sqlite3_vec_init` with an erased signature
    /// (`unsafe extern "C" fn()`). The actual SQLite extension init signature
    /// (`fn(sqlite3*, *mut *const c_char, *const sqlite3_api_routines) -> c_int`)
    /// cannot be checked in Rust because the crate does not export the typed form.
    /// This const verifies the function exists and has the correct calling convention;
    /// the parameter-level contract is enforced at the C ABI level by SQLite itself.
    const _FFI_SIG_CHECK: unsafe extern "C" fn() = sqlite3_vec_init;

    /// Open a store at the given path, creating the database if needed.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the database cannot be opened or schema initialization fails.
    pub fn open(path: &Path, embedding_dimensions: usize) -> Result<Self, StoreError> {
        Self::open_with_clock(path, embedding_dimensions, Arc::new(SystemClock::new()))
    }

    /// Open a store at the given path with a custom clock.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the database cannot be opened or schema initialization fails.
    pub fn open_with_clock(path: &Path, embedding_dimensions: usize, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        Self::register_extension()?;
        let database_lease = SqliteDatabaseLease::shared(path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        // WAL mode allows concurrent reads during writes (important for HTTP transport
        // with background embedding tasks). NORMAL synchronous is safe with WAL and faster.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Self {
            inner: Arc::new(SqliteInner {
                conn: Mutex::new(conn),
                clock,
                vector_index: SqliteVecIndex::new(embedding_dimensions),
                fts_available: AtomicBool::new(false),
                active_embedding_profile: RwLock::new(None),
                _database_lease: Some(database_lease),
            }),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Open an existing, current-schema SQLite store without creating files or
    /// running migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is missing, needs migration, has
    /// incompatible embedding dimensions, or cannot be opened read-only.
    pub fn open_read_only_with_clock(path: &Path, embedding_dimensions: usize, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        Self::register_extension()?;
        if !path.exists() {
            return Err(StoreError::Conflict(format!("SQLite database does not exist: {}", path.display())));
        }
        let database_lease = SqliteDatabaseLease::shared(path)?;
        if sqlite_wal_requires_shm_creation(path) {
            return Err(StoreError::Conflict(
                "SQLite WAL exists without its shared-memory sidecar; refusing a read-only open that could create files".into(),
            ));
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "query_only", true)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        validate_sqlite_source_schema(&conn, embedding_dimensions)?;
        Ok(Self {
            inner: Arc::new(SqliteInner {
                conn: Mutex::new(conn),
                clock,
                vector_index: SqliteVecIndex::new(embedding_dimensions),
                fts_available: AtomicBool::new(true),
                active_embedding_profile: RwLock::new(None),
                _database_lease: Some(database_lease),
            }),
        })
    }

    /// Create an in-memory store (for testing).
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the in-memory database cannot be initialized.
    #[cfg(any(test, feature = "testing"))]
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::in_memory_with_clock(Arc::new(SystemClock::new()))
    }

    /// Create an in-memory store with a custom clock (for testing).
    ///
    /// # Errors
    ///
    /// Returns `StoreError` if the in-memory database cannot be initialized.
    #[cfg(any(test, feature = "testing"))]
    pub fn in_memory_with_clock(clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        Self::register_extension()?;
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Self {
            inner: Arc::new(SqliteInner {
                conn: Mutex::new(conn),
                clock,
                vector_index: SqliteVecIndex::new(Self::DEFAULT_TEST_DIMENSIONS),
                fts_available: AtomicBool::new(false),
                active_embedding_profile: RwLock::new(None),
                _database_lease: None,
            }),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Register the sqlite-vec extension as an auto-extension so it is loaded
    /// for every new connection. Returns an error on first call if registration fails.
    /// Subsequent calls are no-ops.
    #[expect(
        clippy::missing_transmute_annotations,
        reason = "sqlite3 extension fn ptr requires transmute; type is inferred by sqlite3_auto_extension signature"
    )]
    pub(crate) fn register_extension() -> Result<(), StoreError> {
        use std::sync::OnceLock;
        static REGISTER: OnceLock<Result<(), String>> = OnceLock::new();
        let result = REGISTER.get_or_init(|| {
            // SAFETY: sqlite3_vec_init is an extern "C" function with the correct
            // signature for sqlite3_auto_extension. The transmute converts the
            // concrete fn pointer to the Option<unsafe extern "C" fn()> expected
            // by the SQLite API.
            //
            // RR-095: The sqlite-vec crate exports sqlite3_vec_init with an erased
            // signature (unsafe extern "C" fn()). The actual SQLite extension init
            // signature is (sqlite3*, *mut *const c_char, *const sqlite3_api_routines) -> c_int.
            // If the crate ever exports the typed form, update the const _FFI_SIG_CHECK
            // and this transmute to use it directly.
            #[expect(unsafe_code, reason = "required by sqlite3_auto_extension FFI")]
            #[expect(clippy::as_conversions, reason = "FFI function pointer cast required by sqlite3_auto_extension signature")]
            #[expect(clippy::multiple_unsafe_ops_per_block, reason = "transmute + FFI call are a single logical operation")]
            let rc = unsafe { sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ()))) };
            if rc != 0_i32 {
                Err(format!("sqlite3_auto_extension returned error code {rc}"))
            } else {
                Ok(())
            }
        });
        result
            .as_ref()
            .copied()
            .map_err(|msg| StoreError::Database(format!("sqlite-vec registration failed: {msg}").into()))
    }

    fn init_schema(&self) -> Result<(), StoreError> {
        let conn = self.inner.conn.lock();
        let schema_version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version > crate::store::schema::SQLITE_SCHEMA_VERSION {
            return Err(StoreError::Conflict(format!(
                "SQLite schema version {schema_version} is newer than this binary supports ({})",
                crate::store::schema::SQLITE_SCHEMA_VERSION
            )));
        }
        reject_retired_sqlite_schema(&conn)?;

        // First pass: create tables and indexes for fresh databases. For legacy
        // databases some indexes may reference migration-added columns and fail
        // — that is expected. We re-run MAIN_DDL after migrations to pick them up.
        let first_pass_failed = match conn.execute_batch(MAIN_DDL) {
            Ok(()) => false,
            Err(e) => {
                tracing::debug!("first-pass MAIN_DDL failed (expected for legacy databases): {e}");
                true
            }
        };
        self.inner.vector_index.init_schema(&conn)?;
        migrate_memories_add_embedding_revision(&conn).map_err(|e| migration_failed("add_embedding_revision", e))?;
        migrate_memories_add_embedding_claims(&conn).map_err(|e| migration_failed("add_embedding_claims", e))?;
        migrate_memories_backfill_origin_conversation(&conn).map_err(|e| migration_failed("backfill_origin_conversation", e))?;
        migrate_memory_embedding_map_fk(&conn).map_err(|e| migration_failed("memory_embedding_map_fk", e))?;
        // Wave 1 migrations
        migrate_memories_add_memory_type(&conn).map_err(|e| migration_failed("add_memory_type", e))?;
        migrate_memories_add_importance(&conn).map_err(|e| migration_failed("add_importance", e))?;
        migrate_memories_align_impression_tracking(&conn).map_err(|e| migration_failed("align_impression_tracking", e))?;
        // Wave 2 migrations
        migrate_memories_add_superseded_by(&conn).map_err(|e| migration_failed("add_superseded_by", e))?;
        // Wave 3 migrations
        migrate_create_memory_entities(&conn).map_err(|e| migration_failed("create_memory_entities", e))?;
        // Second pass: re-run MAIN_DDL after migrations so indexes on
        // migration-added columns (e.g. `memory_type`) are created for legacy
        // databases. All statements use IF NOT EXISTS, so this is idempotent.
        if first_pass_failed {
            conn.execute_batch(MAIN_DDL).map_err(|e| migration_failed("main_ddl_retry", e))?;
        }
        conn.execute_batch(TRIGGER_DDL).map_err(|e| migration_failed("trigger_ddl", e))?;

        let fts_available = migrate_create_fts_index(&conn).map_err(|e| migration_failed("create_fts_index", e))?;
        self.inner.fts_available.store(fts_available, Ordering::Release);
        // Wave 4 migrations
        migrate_create_audit_log(&conn).map_err(|e| migration_failed("create_audit_log", e))?;
        // Wave 5 migrations (ranking overhaul)
        migrate_memories_add_activity_tracking(&conn).map_err(|e| migration_failed("add_activity_tracking", e))?;
        migrate_memories_add_updated_at(&conn).map_err(|e| migration_failed("add_updated_at", e))?;
        migrate_memories_add_confidence(&conn).map_err(|e| migration_failed("add_confidence", e))?;
        // migrations
        migrate_create_scope_registry(&conn).map_err(|e| migration_failed("create_scope_registry", e))?;
        migrate_create_metadata(&conn).map_err(|e| migration_failed("create_metadata", e))?;
        migrate_create_memory_tombstones(&conn).map_err(|e| migration_failed("create_memory_tombstones", e))?;
        conn.pragma_update(None, "user_version", crate::store::schema::SQLITE_SCHEMA_VERSION)?;

        drop(conn);
        Ok(())
    }

    /// Run a blocking closure on the connection inside `spawn_blocking`.
    ///
    /// # TOCTOU safety
    ///
    /// All check-then-act patterns (e.g., `update_authorized`, `delete_authorized`)
    /// rely on the process-local `parking_lot::Mutex<Connection>` to serialize callers
    /// in this process and on `BEGIN IMMEDIATE` write transactions to serialize against
    /// other server processes using the same SQLite file. Do not introduce a connection
    /// pool or second connection handle without auditing every closure for TOCTOU races.
    pub(crate) async fn with_conn<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.conn.lock();
            f(&mut conn)
        })
        .await
        .map_err(|e| StoreError::Database(Box::new(e)))?
    }

    /// Get the current time from the store's clock.
    pub(crate) fn clock_now(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.clock.now()
    }

    /// Get the expected embedding dimensions for this store.
    pub(crate) fn embedding_dimensions(&self) -> usize {
        self.inner.vector_index.dimensions()
    }

    /// Get the composed vector index adapter.
    pub(crate) fn vector_index(&self) -> SqliteVecIndex {
        self.inner.vector_index.clone()
    }

    pub(crate) fn active_embedding_profile(&self) -> Option<EmbeddingProfile> {
        self.inner.active_embedding_profile.read().clone()
    }

    /// Verify that configured embeddings belong to the database's vector space.
    ///
    /// A fresh database is stamped automatically. Legacy databases containing
    /// vectors require an explicit reindex because their model identity cannot
    /// be inferred safely.
    ///
    /// # Errors
    ///
    /// Returns an error when database access fails or the configured profile
    /// does not match stored vectors.
    pub async fn verify_embedding_profile(&self, profile: &EmbeddingProfile) -> Result<(), StoreError> {
        let configured = profile.clone();
        let profile_for_query = configured.clone();
        let result = self.with_conn(move |conn| verify_embedding_profile_conn(conn, &profile_for_query)).await;
        if result.is_ok() {
            *self.inner.active_embedding_profile.write() = Some(configured);
        }
        result
    }

    /// Check vector-space identity without stamping a missing profile.
    ///
    /// # Errors
    ///
    /// Returns an error when stored vectors have unknown identity or the
    /// configured profile differs from the stored profile.
    pub async fn check_embedding_profile(&self, profile: &EmbeddingProfile) -> Result<(), StoreError> {
        let profile = profile.clone();
        self.with_conn(move |conn| check_embedding_profile_conn(conn, &profile)).await
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
    pub async fn reindex_embeddings(path: &Path, profile: &EmbeddingProfile) -> Result<(), StoreError> {
        let probe = Connection::open(path)?;
        let existing_dimensions = existing_embedding_dimensions(&probe)?;
        drop(probe);

        let store = Self::open(path, existing_dimensions.unwrap_or(profile.dimensions))?;
        let profile = profile.clone();
        store
            .with_conn(move |conn| {
                let tx = sqlite_write_tx(conn)?;
                let _deleted = tx.execute("DELETE FROM memory_embedding_map", [])?;
                let _dropped = tx.execute("DROP TABLE memory_embeddings", [])?;
                let _updated = tx.execute("UPDATE memories SET has_embedding = 0, embedding_claimed_at = NULL, embedding_claim_token = NULL", [])?;
                let vec_ddl = format!("CREATE VIRTUAL TABLE memory_embeddings USING vec0(embedding float[{}]);", profile.dimensions);
                tx.execute_batch(&vec_ddl)?;
                insert_embedding_profile(&tx, &profile)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Override FTS availability in tests to exercise fallback/error branches.
    #[cfg(any(test, feature = "testing"))]
    pub fn set_fts_available_for_test(&self, available: bool) {
        self.inner.fts_available.store(available, Ordering::Release);
    }
}

fn sqlite_wal_requires_shm_creation(path: &Path) -> bool {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    Path::new(&wal).exists() && !Path::new(&shm).exists()
}

pub(crate) fn verify_embedding_profile_conn(conn: &mut Connection, profile: &EmbeddingProfile) -> Result<(), StoreError> {
    let tx = sqlite_write_tx(conn)?;
    let existing = read_embedding_profile(&tx)?;

    if let Some(existing) = existing {
        if existing != *profile {
            return Err(profile_mismatch(&existing, profile));
        }
        tx.commit()?;
        return Ok(());
    }

    let vector_count: i64 = tx.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get(0))?;
    if vector_count > 0 {
        return Err(StoreError::Conflict(
            "existing embeddings have no recorded provider/model identity; run `hold embeddings reindex --yes` before starting with an active embedding provider".into(),
        ));
    }
    insert_embedding_profile(&tx, profile)?;
    tx.commit()?;
    Ok(())
}

fn check_embedding_profile_conn(conn: &Connection, profile: &EmbeddingProfile) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    match read_embedding_profile(&tx)? {
        Some(existing) if existing != *profile => return Err(profile_mismatch(&existing, profile)),
        None => {
            let vector_count: i64 = tx.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get(0))?;
            if vector_count > 0 {
                return Err(StoreError::Conflict(
                    "existing embeddings have no recorded provider/model identity; run `hold embeddings reindex --yes` before searching with an active embedding provider".into(),
                ));
            }
        }
        Some(_) => {}
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn ensure_embedding_profile_matches(conn: &Connection, expected: &EmbeddingProfile) -> Result<(), StoreError> {
    let current =
        read_embedding_profile(conn)?.ok_or_else(|| StoreError::Conflict("embedding profile was removed while this server was running; restart before writing vectors".into()))?;
    if current != *expected {
        return Err(profile_mismatch(&current, expected));
    }
    Ok(())
}

pub(crate) fn read_embedding_profile(conn: &Connection) -> Result<Option<EmbeddingProfile>, StoreError> {
    conn.query_row("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1", [], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
    })
    .optional()?
    .map(|(provider, endpoint, model, dimensions)| {
        Ok::<EmbeddingProfile, StoreError>(EmbeddingProfile {
            provider,
            endpoint,
            model,
            dimensions: usize::try_from(dimensions).map_err(|_error| StoreError::Conflict("stored embedding dimensions are invalid".into()))?,
        })
    })
    .transpose()
}

fn insert_embedding_profile(conn: &Connection, profile: &EmbeddingProfile) -> Result<(), StoreError> {
    let dimensions = i64::try_from(profile.dimensions).map_err(|_error| StoreError::Conflict("embedding dimensions exceed SQLite INTEGER".into()))?;
    let _updated = conn.execute(
        "INSERT INTO embedding_profile (singleton, provider, endpoint, model, dimensions)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(singleton) DO UPDATE SET
           provider = excluded.provider,
           endpoint = excluded.endpoint,
           model = excluded.model,
           dimensions = excluded.dimensions",
        (&profile.provider, &profile.endpoint, &profile.model, dimensions),
    )?;
    Ok(())
}

fn profile_mismatch(existing: &EmbeddingProfile, configured: &EmbeddingProfile) -> StoreError {
    StoreError::Conflict(format!(
        "embedding profile mismatch: database uses {} model '{}' at '{}' with {} dimensions, but config selects {} model '{}' at '{}' with {} dimensions; run `hold embeddings reindex --yes` to rebuild all vectors",
        existing.provider, existing.model, existing.endpoint, existing.dimensions, configured.provider, configured.model, configured.endpoint, configured.dimensions
    ))
}

impl MemoryReader for SqliteStore {
    fn fts_available(&self) -> bool {
        self.inner.fts_available.load(Ordering::Acquire)
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

    async fn fetch_embeddings_for_ids(&self, ids: &[MemoryId]) -> Result<super::EmbeddingMap, StoreError> {
        self.fetch_embeddings_for_ids_impl(ids).await
    }

    async fn find_embedding_neighbors(&self, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<super::EmbeddingNeighbor>, StoreError> {
        self.find_embedding_neighbors_impl(embedding, max_l2_distance, limit).await
    }
}

impl MemoryWriter for SqliteStore {
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

    async fn claim_for_reembed(&self, limit: usize) -> Result<Vec<super::ReembedClaim>, StoreError> {
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
        metadata_patch: Option<&crate::types::MetadataPatch>,
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

    async fn bulk_delete_ids(&self, ids: Vec<MemoryId>, principal: &str) -> Result<super::BulkAuthOutcome, StoreError> {
        self.bulk_delete_ids_impl(ids, principal).await
    }

    async fn bulk_delete_ids_audited(&self, ids: Vec<MemoryId>, principal: &str, audit: &AuditDraft) -> Result<super::BulkAuthOutcome, StoreError> {
        self.bulk_delete_ids_audited_impl(ids, principal, Some(audit)).await
    }

    async fn bulk_update_ids(&self, ids: Vec<MemoryId>, update: MemoryUpdate, principal: &str, now: chrono::DateTime<chrono::Utc>) -> Result<super::BulkAuthOutcome, StoreError> {
        self.bulk_update_ids_impl(ids, update, principal, now).await
    }

    async fn bulk_update_ids_audited(
        &self,
        ids: Vec<MemoryId>,
        update: MemoryUpdate,
        principal: &str,
        now: chrono::DateTime<chrono::Utc>,
        audit: &AuditDraft,
    ) -> Result<super::BulkAuthOutcome, StoreError> {
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
        now: chrono::DateTime<chrono::Utc>,
        activity_half_life_hours: f64,
    ) -> Result<super::RecordUseOutcome, StoreError> {
        self.record_memory_use_impl(ids, principal, event_weight, now, activity_half_life_hours).await
    }

    async fn write_audit_entry(
        &self,
        memory_id: &MemoryId,
        action: AuditAction,
        principal: Option<&str>,
        timestamp: chrono::DateTime<chrono::Utc>,
        details: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        self.write_audit_entry_impl(memory_id, action, principal, timestamp, details).await
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

impl MemoryAdmin for SqliteStore {
    async fn evict_expired(&self) -> Result<u64, StoreError> {
        self.evict_expired_impl().await
    }

    async fn reassign_scope(&self, from_scope: &str, to_scope: &str, origin_conversation: Option<&str>, principal: &str) -> Result<super::ReassignScopeOutcome, StoreError> {
        self.reassign_scope_impl(from_scope, to_scope, origin_conversation, principal).await
    }

    async fn reassign_scope_audited(
        &self,
        from_scope: &str,
        to_scope: &str,
        origin_conversation: Option<&str>,
        principal: &str,
        audit: &AuditDraft,
    ) -> Result<super::ReassignScopeOutcome, StoreError> {
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

#[cfg(test)]
#[expect(unused_results, reason = "test setup and assertions discard many results intentionally")]
mod tests {
    use chrono::{DateTime, TimeZone as _, Utc};
    use rusqlite::Connection;

    use super::*;
    use crate::{
        clock::MockClock,
        store::schema::{
            migrate_memories_add_embedding_revision, migrate_memories_add_importance, migrate_memories_add_memory_type, migrate_memories_add_superseded_by,
            migrate_memories_align_impression_tracking, migrate_memories_backfill_origin_conversation, migrate_memory_embedding_map_fk,
        },
        types::{AccessPolicy, Entity, Provenance, RedactableField, ScopeDefinition},
    };

    /// Fixed base time used by all non-time-sensitive tests.
    fn base_time() -> DateTime<Utc> {
        #[expect(clippy::expect_used, reason = "hardcoded valid date never fails")]
        Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).single().expect("valid date")
    }

    fn make_memory(content: &str, tags: &[&str], now: DateTime<Utc>) -> Memory {
        Memory {
            id: MemoryId::new(),
            content: content.into(),
            tags: tags.iter().map(ToString::to_string).collect(),
            provenance: Provenance {
                source_agent: Some("test-agent".into()),
                ..Default::default()
            },
            access_policy: AccessPolicy::Public,
            created_at: now,
            updated_at: now,
            expires_at: None,
            has_embedding: false,
            memory_type: crate::types::MemoryType::default(),
            importance: crate::types::Importance::DEFAULT,
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

    fn embedding_profile(model: &str, dimensions: usize) -> EmbeddingProfile {
        EmbeddingProfile::openai_compatible("http://127.0.0.1:8000/v1", model, dimensions)
    }

    #[test]
    fn open_stamps_schema_version_and_rejects_newer_databases() {
        let directory = tempfile::tempdir().unwrap();
        let current_path = directory.path().join("current.db");
        let current = SqliteStore::open(&current_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
        drop(current);
        let connection = Connection::open(&current_path).unwrap();
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(version, crate::store::schema::SQLITE_SCHEMA_VERSION);
        drop(connection);

        let future_path = directory.path().join("future.db");
        let connection = Connection::open(&future_path).unwrap();
        connection
            .pragma_update(None, "user_version", crate::store::schema::SQLITE_SCHEMA_VERSION.saturating_add(1))
            .unwrap();
        drop(connection);
        let error = SqliteStore::open(&future_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap_err();
        assert!(error.to_string().contains("newer than this binary supports"));
    }

    #[test]
    fn read_only_open_neither_creates_nor_migrates_databases() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.db");
        let error = SqliteStore::open_read_only_with_clock(&missing, SqliteStore::DEFAULT_TEST_DIMENSIONS, Arc::new(MockClock::new())).unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(!missing.exists(), "read-only open must not create a missing database");

        let outdated = directory.path().join("outdated.db");
        drop(Connection::open(&outdated).unwrap());
        let error = SqliteStore::open_read_only_with_clock(&outdated, SqliteStore::DEFAULT_TEST_DIMENSIONS, Arc::new(MockClock::new())).unwrap_err();
        assert!(error.to_string().contains("schema version"));
        let connection = Connection::open(&outdated).unwrap();
        let memories_exist: bool = connection
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!memories_exist, "read-only open must not bootstrap an outdated database");
    }

    #[tokio::test]
    async fn read_only_store_reads_but_rejects_writes_and_profile_stamping() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.db");
        let writable = SqliteStore::open(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
        let memory = make_memory("read-only", &[], base_time());
        let id = writable.store(&memory, None).await.unwrap();
        drop(writable);

        let read_only = SqliteStore::open_read_only_with_clock(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS, Arc::new(MockClock::new())).unwrap();
        assert!(
            matches!(SqliteDatabaseLease::try_exclusive(&path), Err(crate::store::sqlite_lease::ExclusiveLeaseError::InUse)),
            "the read-only store must prevent replacement while its connection is live"
        );
        assert!(read_only.get(&id, None).await.unwrap().is_some());
        read_only
            .check_embedding_profile(&embedding_profile("model-a", SqliteStore::DEFAULT_TEST_DIMENSIONS))
            .await
            .unwrap();
        let error = read_only.store(&make_memory("write attempt", &[], base_time()), None).await.unwrap_err();
        assert!(error.to_string().contains("readonly") || error.to_string().contains("read-only"));
        drop(read_only);
        assert!(
            SqliteDatabaseLease::try_exclusive(&path).is_ok(),
            "dropping the read-only store must release its database lease"
        );

        let connection = Connection::open(&path).unwrap();
        let profile_count: i64 = connection.query_row("SELECT COUNT(*) FROM embedding_profile", [], |row| row.get(0)).unwrap();
        assert_eq!(profile_count, 0_i64, "profile checks must not stamp the database");
    }

    #[tokio::test]
    async fn embedding_profile_stamps_fresh_database_and_rejects_mismatch() {
        let store = SqliteStore::in_memory().unwrap();
        let original = embedding_profile("model-a", SqliteStore::DEFAULT_TEST_DIMENSIONS);
        store.verify_embedding_profile(&original).await.unwrap();
        store.verify_embedding_profile(&original).await.unwrap();

        let error = store
            .verify_embedding_profile(&embedding_profile("model-b", SqliteStore::DEFAULT_TEST_DIMENSIONS))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("embedding profile mismatch"));
        assert!(error.to_string().contains("hold embeddings reindex --yes"));
    }

    #[tokio::test]
    async fn legacy_vectors_without_profile_require_reindex() {
        let store = SqliteStore::in_memory().unwrap();
        let memory = make_memory("legacy vector", &[], base_time());
        store.store(&memory, Some(&vec![0.25_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS])).await.unwrap();

        let error = store
            .verify_embedding_profile(&embedding_profile("model-a", SqliteStore::DEFAULT_TEST_DIMENSIONS))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no recorded provider/model identity"));
    }

    #[tokio::test]
    async fn concurrent_first_profile_writers_cannot_select_different_models() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let first = SqliteStore::open(temp.path(), 3).unwrap();
        let second = SqliteStore::open(temp.path(), 3).unwrap();
        let profile_a = embedding_profile("model-a", 3);
        let profile_b = embedding_profile("model-b", 3);

        let (result_a, result_b) = tokio::join!(first.verify_embedding_profile(&profile_a), second.verify_embedding_profile(&profile_b));
        assert_ne!(result_a.is_ok(), result_b.is_ok(), "exactly one profile should claim a fresh database");
        let conflict = result_a.err().or_else(|| result_b.err()).unwrap();
        assert!(conflict.to_string().contains("embedding profile mismatch"));
    }

    #[tokio::test]
    async fn reindex_preserves_memories_and_supports_dimension_change() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path().to_owned();
        let memory = make_memory("preserved during reindex", &[], base_time());
        let memory_id = memory.id;
        let old_store = SqliteStore::open(&path, 3).unwrap();
        let original = embedding_profile("model-a", 3);
        old_store.verify_embedding_profile(&original).await.unwrap();
        old_store.store(&memory, Some(&[0.1_f32, 0.2_f32, 0.3_f32])).await.unwrap();

        let replacement = embedding_profile("model-b", 4);
        SqliteStore::reindex_embeddings(&path, &replacement).await.unwrap();

        let stale_write = old_store.set_embedding(&memory_id, &[0.3_f32, 0.2_f32, 0.1_f32], 0).await.unwrap_err();
        assert!(stale_write.to_string().contains("embedding profile mismatch"));

        let reopened = SqliteStore::open(&path, 4).unwrap();
        reopened.verify_embedding_profile(&replacement).await.unwrap();
        let preserved = reopened.get(&memory_id, None).await.unwrap().unwrap();
        assert!(!preserved.has_embedding);
        assert!(reopened.fetch_embeddings_for_ids(&[memory_id]).await.unwrap().is_empty());
    }

    fn make_metadata(memory_id: MemoryId) -> MemoryMetadata {
        MemoryMetadata {
            memory_id,
            scope_key: Some("test-scope".into()),
            summary: Some("test summary".into()),
            agent_label: Some("test-agent".into()),
            created_by_principal: Some("test-principal".into()),
            quality_flags: vec!["test_flag".into()],
            schema_version: 1,
        }
    }

    fn audit_draft(action: AuditAction) -> AuditDraft {
        AuditDraft {
            action,
            caller_agent: Some("test-agent".into()),
            timestamp: base_time(),
            details: None,
        }
    }

    async fn drop_table(store: &SqliteStore, table: &'static str) {
        store
            .with_conn(move |conn| {
                conn.execute(&format!("DROP TABLE {table}"), [])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    fn sqlite_index_exists(conn: &Connection, index_name: &str) -> bool {
        conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)", [index_name], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[tokio::test]
    async fn sqlite_store_satisfies_memory_store_conformance() {
        let store = SqliteStore::in_memory().unwrap();
        crate::store::conformance::assert_memory_store_contract(&store, SqliteStore::DEFAULT_TEST_DIMENSIONS).await;
    }

    #[tokio::test]
    async fn sqlite_store_rejects_non_finite_embeddings() {
        let store = SqliteStore::in_memory().unwrap();
        crate::store::conformance::assert_non_finite_embeddings_rejected(&store, SqliteStore::DEFAULT_TEST_DIMENSIONS).await;
    }

    #[tokio::test]
    async fn sqlite_store_configures_busy_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("busy-timeout.db");
        let store = SqliteStore::open(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();

        let timeout_ms: i64 = store.with_conn(|conn| Ok(conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?)).await.unwrap();

        assert_eq!(timeout_ms, i64::try_from(SQLITE_BUSY_TIMEOUT.as_millis()).unwrap());
    }

    #[test]
    fn sqlite_write_tx_acquires_cross_connection_writer_lock() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("immediate-lock.db");
        let mut first = Connection::open(&path).unwrap();
        first.busy_timeout(SQLITE_BUSY_TIMEOUT).unwrap();
        let second = Connection::open(&path).unwrap();
        second.busy_timeout(Duration::from_millis(0)).unwrap();
        first.execute_batch("CREATE TABLE locks (id INTEGER PRIMARY KEY)").unwrap();

        let tx = sqlite_write_tx(&mut first).unwrap();
        let err = second.execute_batch("BEGIN IMMEDIATE").unwrap_err();

        assert!(
            matches!(
                err,
                rusqlite::Error::SqliteFailure(error, _)
                    if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ),
            "expected writer lock to block a second immediate transaction, got {err:?}"
        );
        tx.commit().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_sqlite_store_instances_record_memory_use_without_lost_updates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("activity-contention.db");
        let store_a = SqliteStore::open(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
        let store_b = SqliteStore::open(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
        let now = base_time();
        let mut ids = Vec::with_capacity(32_usize);
        for idx in 0_usize..32_usize {
            let memory = make_memory(&format!("activity contention {idx}"), &[], now);
            ids.push(store_a.store(&memory, None).await.unwrap());
        }

        let first = store_a.record_memory_use(&ids, "agent-a", 1.0, now, 24.0);
        let second = store_b.record_memory_use(&ids, "agent-b", 1.0, now, 24.0);
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.unwrap().recorded, 32_u64);
        assert_eq!(second.unwrap().recorded, 32_u64);
        for id in ids {
            let memory = store_a.get(&id, Some("agent-a")).await.unwrap().unwrap();
            assert_eq!(
                memory.activity_mass.to_bits(),
                2.0_f64.to_bits(),
                "expected both store instances to contribute activity for {id}"
            );
            assert_eq!(memory.last_used_at, Some(now));
        }
    }

    #[tokio::test]
    async fn store_and_get() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("hello world", &["greeting"], base_time());
        let id = store.store(&mem, None).await.unwrap();
        let retrieved = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(retrieved.content, "hello world");
        assert_eq!(retrieved.tags, vec!["greeting"]);
    }

    #[tokio::test]
    async fn store_with_metadata_persists_memory_and_metadata() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("metadata memory", &[], base_time());
        let metadata = make_metadata(mem.id);

        let id = store.store_with_metadata(&mem, None, None, &metadata).await.unwrap();

        assert_eq!(id, mem.id);
        assert!(store.get(&id, None).await.unwrap().is_some());
        assert_eq!(store.get_metadata(&id).await.unwrap(), Some(metadata));
    }

    #[tokio::test]
    async fn store_audited_rolls_back_memory_when_audit_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("audit rollback memory", &[], base_time());
        drop_table(&store, "memory_audit_log").await;

        let err = store.store_audited(&mem, None, &audit_draft(AuditAction::Store)).await.unwrap_err();

        assert!(err.to_string().contains("memory_audit_log"));
        assert!(store.get(&mem.id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_with_metadata_audited_rolls_back_when_metadata_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("metadata rollback audited memory", &[], base_time());
        let metadata = make_metadata(mem.id);
        drop_table(&store, "memory_metadata").await;

        let err = store
            .store_with_metadata_audited(&mem, None, None, &metadata, &audit_draft(AuditAction::Store))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("memory_metadata"));
        assert!(store.get(&mem.id, None).await.unwrap().is_none());
        assert!(store.query_audit_log(&mem.id, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_authorized_audited_rolls_back_update_when_audit_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("before audit rollback", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();
        drop_table(&store, "memory_audit_log").await;

        let update = MemoryUpdate {
            content: Some("after audit rollback".into()),
            ..MemoryUpdate::default()
        };
        let err = store
            .update_authorized_audited(&id, &update, "test-agent", &audit_draft(AuditAction::Update))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("memory_audit_log"));
        let after = store.get(&id, Some("test-agent")).await.unwrap().unwrap();
        assert_eq!(after.content, "before audit rollback");
        assert_eq!(after.has_embedding, mem.has_embedding);
    }

    #[tokio::test]
    async fn update_authorized_with_metadata_audited_rolls_back_update_when_metadata_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("before metadata rollback", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();
        drop_table(&store, "memory_metadata").await;

        let update = MemoryUpdate {
            content: Some("after metadata rollback".into()),
            ..MemoryUpdate::default()
        };
        let patch = crate::types::MetadataPatch {
            summary: Some("rollback summary".into()),
            ..Default::default()
        };
        let err = store
            .update_authorized_with_metadata_audited(&id, &update, Some(&patch), "test-agent", &audit_draft(AuditAction::Update))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("memory_metadata"));
        let after = store.get(&id, Some("test-agent")).await.unwrap().unwrap();
        assert_eq!(after.content, "before metadata rollback");
        assert!(store.query_audit_log(&id, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn migrate_metadata_audited_records_only_inserted_rows() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("metadata migration audited", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        let report = store.migrate_metadata_audited(&[], false, &audit_draft(AuditAction::Update)).await.unwrap();

        assert_eq!(report.migrated, 1_u64);
        assert_eq!(store.get_metadata(&id).await.unwrap().unwrap().schema_version, 1_i64);
        let history = store.query_audit_log(&id, 10).await.unwrap();
        assert_eq!(history.len(), 1_usize);
        assert_eq!(history[0].action, AuditAction::Update);

        let second_report = store.migrate_metadata_audited(&[], false, &audit_draft(AuditAction::Update)).await.unwrap();
        assert_eq!(second_report.migrated, 0_u64);
        assert_eq!(store.query_audit_log(&id, 10).await.unwrap().len(), 1_usize);
    }

    #[tokio::test]
    async fn delete_authorized_audited_rolls_back_delete_when_audit_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("delete audit rollback", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();
        drop_table(&store, "memory_audit_log").await;

        let err = store.delete_authorized_audited(&id, "test-agent", &audit_draft(AuditAction::Delete)).await.unwrap_err();

        assert!(err.to_string().contains("memory_audit_log"));
        assert!(store.get(&id, Some("test-agent")).await.unwrap().is_some());
        assert!(store.get_tombstone(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn bulk_delete_audited_records_only_applied_ids() {
        let store = SqliteStore::in_memory().unwrap();
        let owned = make_memory("owned bulk audited delete", &[], base_time());
        let denied = Memory {
            provenance: Provenance {
                source_agent: Some("other-agent".into()),
                ..Default::default()
            },
            ..make_memory("denied bulk audited delete", &[], base_time())
        };
        let owned_id = store.store(&owned, None).await.unwrap();
        let denied_id = store.store(&denied, None).await.unwrap();

        let outcome = store
            .bulk_delete_ids_audited(vec![owned_id, denied_id], "test-agent", &audit_draft(AuditAction::BulkDelete))
            .await
            .unwrap();

        assert_eq!(outcome.applied_ids, vec![owned_id]);
        assert_eq!(outcome.denied, 1_u64);
        assert_eq!(store.query_audit_log(&owned_id, 10).await.unwrap()[0].action, AuditAction::BulkDelete);
        assert!(store.query_audit_log(&denied_id, 10).await.unwrap().is_empty());
        assert!(store.get(&denied_id, Some("other-agent")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn store_with_metadata_rolls_back_memory_when_metadata_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("rollback memory", &[], base_time());
        let bad_metadata = make_metadata(MemoryId::new());

        let _err = store.store_with_metadata(&mem, None, None, &bad_metadata).await.unwrap_err();

        assert!(store.get(&mem.id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_with_metadata_rejects_existing_wrong_metadata_id() {
        let store = SqliteStore::in_memory().unwrap();
        let existing = make_memory("existing metadata owner", &[], base_time());
        let existing_metadata = make_metadata(existing.id);
        let _existing_id = store.store_with_metadata(&existing, None, None, &existing_metadata).await.unwrap();

        let new_memory = make_memory("new memory with mismatched metadata", &[], base_time());
        let mut wrong_metadata = make_metadata(existing.id);
        wrong_metadata.summary = Some("wrong target".into());

        let err = store.store_with_metadata(&new_memory, None, None, &wrong_metadata).await.unwrap_err();

        assert!(err.to_string().contains("metadata memory_id"));
        assert!(store.get(&new_memory.id, None).await.unwrap().is_none());
        assert_eq!(store.get_metadata(&existing.id).await.unwrap(), Some(existing_metadata));
    }

    #[tokio::test]
    async fn store_batch_with_metadata_rolls_back_all_memories_when_metadata_insert_fails() {
        let store = SqliteStore::in_memory().unwrap();
        let first = make_memory("batch rollback one", &[], base_time());
        let second = make_memory("batch rollback two", &[], base_time());
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
        let metadata = vec![make_metadata(first.id), make_metadata(MemoryId::new())];

        let _err = store.store_batch_with_metadata(&memories, &[None, None], &metadata).await.unwrap_err();

        assert!(store.get(&first.id, None).await.unwrap().is_none());
        assert!(store.get(&second.id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_batch_with_metadata_rejects_supersedes_length_mismatch() {
        let store = SqliteStore::in_memory().unwrap();
        let first = make_memory("batch supersedes mismatch one", &[], base_time());
        let second = make_memory("batch supersedes mismatch two", &[], base_time());
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
        let metadata = vec![make_metadata(first.id), make_metadata(second.id)];

        let err = store.store_batch_with_metadata(&memories, &[None], &metadata).await.unwrap_err();

        assert!(err.to_string().contains("supersedes length"));
        assert!(store.get(&first.id, None).await.unwrap().is_none());
        assert!(store.get(&second.id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_batch_with_metadata_rejects_existing_wrong_metadata_id() {
        let store = SqliteStore::in_memory().unwrap();
        let existing = make_memory("existing batch metadata owner", &[], base_time());
        let existing_metadata = make_metadata(existing.id);
        let _existing_id = store.store_with_metadata(&existing, None, None, &existing_metadata).await.unwrap();

        let first = make_memory("batch wrong id one", &[], base_time());
        let second = make_memory("batch wrong id two", &[], base_time());
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
        let mut wrong_metadata = make_metadata(existing.id);
        wrong_metadata.summary = Some("wrong batch target".into());
        let metadata = vec![make_metadata(first.id), wrong_metadata];

        let err = store.store_batch_with_metadata(&memories, &[None, None], &metadata).await.unwrap_err();

        assert!(err.to_string().contains("metadata memory_id"));
        assert!(store.get(&first.id, None).await.unwrap().is_none());
        assert!(store.get(&second.id, None).await.unwrap().is_none());
        assert_eq!(store.get_metadata(&existing.id).await.unwrap(), Some(existing_metadata));
    }

    #[tokio::test]
    async fn delete_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("to be deleted", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();
        assert!(store.delete(&id).await.unwrap());
        assert!(store.get(&id, None).await.unwrap().is_none());
        let tombstone = store.get_tombstone(&id).await.unwrap().unwrap();
        assert_eq!(tombstone.memory_id, id);
        assert_eq!(tombstone.provenance.source_agent, mem.provenance.source_agent);
        assert_eq!(tombstone.deleted_by_principal, None);
        // Double delete returns false
        assert!(!store.delete(&id).await.unwrap());
    }

    #[tokio::test]
    async fn list_with_tag_filter() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("mem1", &["a", "b"], base_time()), None).await.unwrap();
        store.store(&make_memory("mem2", &["b", "c"], base_time()), None).await.unwrap();
        store.store(&make_memory("mem3", &["d"], base_time()), None).await.unwrap();

        let filter = MemoryFilter {
            tags: Some(vec!["b".into()]),
            ..Default::default()
        };
        let results = store.list(filter, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn list_with_scope_filter() {
        let store = SqliteStore::in_memory().unwrap();

        let mut conv_1 = make_memory("conv-1 memory", &[], base_time());
        conv_1.provenance.source_conversation = Some("conv-1".into());
        store.store(&conv_1, None).await.unwrap();

        let mut conv_2 = make_memory("conv-2 memory", &[], base_time());
        conv_2.provenance.source_conversation = Some("conv-2".into());
        store.store(&conv_2, None).await.unwrap();

        let filter = MemoryFilter {
            scope: Some("conv-1".into()),
            ..Default::default()
        };
        let results = store.list(filter, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "conv-1 memory");
    }

    #[tokio::test]
    async fn text_search() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("rust is great", &[], base_time()), None).await.unwrap();
        store.store(&make_memory("python is fine", &[], base_time()), None).await.unwrap();

        let results = store.search_by_text("rust", 10, &MemoryFilter::default(), &QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].memory.content.contains("rust"));
    }

    #[tokio::test]
    async fn text_search_with_scopes_filter() {
        let store = SqliteStore::in_memory().unwrap();

        let mut conv_1 = make_memory("needle from conv-1", &[], base_time());
        conv_1.provenance.source_conversation = Some("conv-1".into());
        store.store(&conv_1, None).await.unwrap();

        let mut conv_2 = make_memory("needle from conv-2", &[], base_time());
        conv_2.provenance.source_conversation = Some("conv-2".into());
        store.store(&conv_2, None).await.unwrap();

        let filter = MemoryFilter {
            scopes_any: Some(vec!["conv-2".into(), "conv-3".into()]),
            ..Default::default()
        };
        let results = store.search_by_text("needle", 10, &filter, &QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "needle from conv-2");
    }

    #[tokio::test]
    async fn list_with_origin_scope_filter() {
        let store = SqliteStore::in_memory().unwrap();

        let mut conv_a = make_memory("conv-a memory", &[], base_time());
        conv_a.provenance.source_conversation = Some("project-1".into());
        conv_a.provenance.origin_conversation = Some("conv-a".into());
        store.store(&conv_a, None).await.unwrap();

        let mut conv_b = make_memory("conv-b memory", &[], base_time());
        conv_b.provenance.source_conversation = Some("project-1".into());
        conv_b.provenance.origin_conversation = Some("conv-b".into());
        store.store(&conv_b, None).await.unwrap();

        let filter = MemoryFilter {
            origin_scope: Some("conv-a".into()),
            ..Default::default()
        };
        let results = store.list(filter, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "conv-a memory");
    }

    #[tokio::test]
    async fn search_with_origin_scope_filter() {
        let store = SqliteStore::in_memory().unwrap();

        let mut conv_a = make_memory("needle from conv-a", &[], base_time());
        conv_a.provenance.source_conversation = Some("project-1".into());
        conv_a.provenance.origin_conversation = Some("conv-a".into());
        store.store(&conv_a, None).await.unwrap();

        let mut conv_b = make_memory("needle from conv-b", &[], base_time());
        conv_b.provenance.source_conversation = Some("project-1".into());
        conv_b.provenance.origin_conversation = Some("conv-b".into());
        store.store(&conv_b, None).await.unwrap();

        let filter = MemoryFilter {
            origin_scope: Some("conv-b".into()),
            ..Default::default()
        };
        let results = store.search_by_text("needle", 10, &filter, &QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "needle from conv-b");
    }

    #[tokio::test]
    async fn origin_filter_falls_back_to_source_conversation_for_legacy_rows() {
        let store = SqliteStore::in_memory().unwrap();

        let mut legacy = make_memory("legacy from conv-legacy", &[], base_time());
        legacy.provenance.source_conversation = Some("conv-legacy".into());
        legacy.provenance.origin_conversation = None;
        store.store(&legacy, None).await.unwrap();

        let mut other = make_memory("other memory", &[], base_time());
        other.provenance.source_conversation = Some("conv-other".into());
        store.store(&other, None).await.unwrap();

        let list_results = store
            .list(
                MemoryFilter {
                    origin_scope: Some("conv-legacy".into()),
                    ..Default::default()
                },
                QueryContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(list_results.len(), 1);
        assert_eq!(list_results[0].content, "legacy from conv-legacy");

        let search_results = store
            .search_by_text(
                "legacy",
                10,
                &MemoryFilter {
                    origin_scope: Some("conv-legacy".into()),
                    ..Default::default()
                },
                &QueryContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(search_results.len(), 1);
        assert_eq!(search_results[0].memory.content, "legacy from conv-legacy");
    }

    #[tokio::test]
    async fn reassign_scope_moves_matching_rows_and_backfills_origin() {
        let store = SqliteStore::in_memory().unwrap();

        let mut legacy = make_memory("legacy-conv-1", &[], base_time());
        legacy.provenance.source_conversation = Some("conv-1".into());
        let legacy_id = store.store(&legacy, None).await.unwrap();

        let mut with_origin = make_memory("origin-conv-1", &[], base_time());
        with_origin.provenance.source_conversation = Some("conv-1".into());
        with_origin.provenance.origin_conversation = Some("conv-root".into());
        let with_origin_id = store.store(&with_origin, None).await.unwrap();

        let mut other = make_memory("other-conv", &[], base_time());
        other.provenance.source_conversation = Some("conv-2".into());
        let other_id = store.store(&other, None).await.unwrap();

        let reassigned = store.reassign_scope("conv-1", "project-1", None, "test-agent").await.unwrap();
        assert_eq!(reassigned.applied_ids.len(), 2);
        assert!(reassigned.applied_ids.contains(&legacy_id));
        assert!(reassigned.applied_ids.contains(&with_origin_id));

        let legacy_after = store.get(&legacy_id, None).await.unwrap().unwrap();
        assert_eq!(legacy_after.provenance.source_conversation.as_deref(), Some("project-1"));
        assert_eq!(legacy_after.provenance.origin_conversation.as_deref(), Some("conv-1"));

        let with_origin_after = store.get(&with_origin_id, None).await.unwrap().unwrap();
        assert_eq!(with_origin_after.provenance.source_conversation.as_deref(), Some("project-1"));
        assert_eq!(with_origin_after.provenance.origin_conversation.as_deref(), Some("conv-root"));

        let other_after = store.get(&other_id, None).await.unwrap().unwrap();
        assert_eq!(other_after.provenance.source_conversation.as_deref(), Some("conv-2"));
        assert!(other_after.provenance.origin_conversation.is_none());
    }

    #[tokio::test]
    async fn reassign_scope_with_origin_filter_moves_only_matching_origin() {
        let store = SqliteStore::in_memory().unwrap();

        let mut moved_seed = make_memory("conv-a memory", &[], base_time());
        moved_seed.provenance.source_conversation = Some("project-1".into());
        moved_seed.provenance.origin_conversation = Some("conv-a".into());
        let moved_id = store.store(&moved_seed, None).await.unwrap();

        let mut retained_seed = make_memory("conv-b memory", &[], base_time());
        retained_seed.provenance.source_conversation = Some("project-1".into());
        retained_seed.provenance.origin_conversation = Some("conv-b".into());
        let retained_id = store.store(&retained_seed, None).await.unwrap();

        let reassigned = store.reassign_scope("project-1", "conv-a", Some("conv-a"), "test-agent").await.unwrap();
        assert_eq!(reassigned.applied_ids, vec![moved_id]);

        let moved_memory = store.get(&moved_id, None).await.unwrap().unwrap();
        assert_eq!(moved_memory.provenance.source_conversation.as_deref(), Some("conv-a"));
        assert_eq!(moved_memory.provenance.origin_conversation.as_deref(), Some("conv-a"));

        let retained_memory = store.get(&retained_id, None).await.unwrap().unwrap();
        assert_eq!(retained_memory.provenance.source_conversation.as_deref(), Some("project-1"));
        assert_eq!(retained_memory.provenance.origin_conversation.as_deref(), Some("conv-b"));
    }

    #[tokio::test]
    async fn reassign_scope_origin_filter_matches_legacy_rows_without_origin() {
        let store = SqliteStore::in_memory().unwrap();

        let mut legacy = make_memory("legacy record", &[], base_time());
        legacy.provenance.source_conversation = Some("project-1".into());
        legacy.provenance.origin_conversation = None;
        let legacy_id = store.store(&legacy, None).await.unwrap();

        let reassigned = store.reassign_scope("project-1", "conv-legacy", Some("project-1"), "test-agent").await.unwrap();
        assert_eq!(reassigned.applied_ids, vec![legacy_id]);

        let moved = store.get(&legacy_id, None).await.unwrap().unwrap();
        assert_eq!(moved.provenance.source_conversation.as_deref(), Some("conv-legacy"));
        assert_eq!(moved.provenance.origin_conversation.as_deref(), Some("project-1"));
    }

    #[tokio::test]
    async fn reassign_scope_skips_unauthorized_rows() {
        let store = SqliteStore::in_memory().unwrap();

        let mut caller_owned = make_memory("caller-owned", &[], base_time());
        caller_owned.provenance.source_agent = Some("caller".into());
        caller_owned.provenance.source_conversation = Some("project-1".into());
        let caller_id = store.store(&caller_owned, None).await.unwrap();

        let mut other_owned = make_memory("other-owned", &[], base_time());
        other_owned.provenance.source_agent = Some("other".into());
        other_owned.provenance.source_conversation = Some("project-1".into());
        let other_id = store.store(&other_owned, None).await.unwrap();

        let reassigned = store.reassign_scope("project-1", "project-2", None, "caller").await.unwrap();
        assert_eq!(reassigned.applied_ids, vec![caller_id]);

        let caller_after = store.get(&caller_id, None).await.unwrap().unwrap();
        assert_eq!(caller_after.provenance.source_conversation.as_deref(), Some("project-2"));

        let other_after = store.get(&other_id, None).await.unwrap().unwrap();
        assert_eq!(other_after.provenance.source_conversation.as_deref(), Some("project-1"));
    }

    #[tokio::test]
    async fn text_search_with_zero_limit_returns_empty() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("rust is great", &[], base_time()), None).await.unwrap();

        let results = store.search_by_text("rust", 0, &MemoryFilter::default(), &QueryContext::default()).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn list_with_zero_limit_returns_empty() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("one", &[], base_time()), None).await.unwrap();
        store.store(&make_memory("two", &[], base_time()), None).await.unwrap();

        let results = store
            .list(
                MemoryFilter {
                    limit: Some(0),
                    ..Default::default()
                },
                QueryContext::default(),
            )
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn list_with_identical_created_at_orders_by_id_descending() {
        let store = SqliteStore::in_memory().unwrap();
        let mut low_id = make_memory("low", &[], base_time());
        low_id.id = "01J0000000000000000000000A".parse().unwrap();
        let mut high_id = make_memory("high", &[], base_time());
        high_id.id = "01J0000000000000000000000B".parse().unwrap();
        store.store(&low_id, None).await.unwrap();
        store.store(&high_id, None).await.unwrap();

        let results = store.list(MemoryFilter::default(), QueryContext::default()).await.unwrap();

        assert_eq!(results.iter().map(|memory| memory.id).collect::<Vec<_>>(), vec![high_id.id, low_id.id]);
    }

    #[tokio::test]
    async fn embedding_store_and_search() {
        let store = SqliteStore::in_memory().unwrap();
        // Simple 768-dim embeddings for testing
        let mut emb1 = vec![0.0_f32; 768];
        emb1[0] = 1.0;
        let mut emb2 = vec![0.0_f32; 768];
        emb2[1] = 1.0;

        let mem1 = make_memory("first", &[], base_time());
        let mem2 = make_memory("second", &[], base_time());
        store.store(&mem1, Some(&emb1)).await.unwrap();
        store.store(&mem2, Some(&emb2)).await.unwrap();

        // Search with emb close to emb1
        let mut query_emb = vec![0.0_f32; 768];
        query_emb[0] = 0.9;
        let results = store
            .search_by_embedding(&query_emb, 2, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].memory.content, "first");
    }

    #[tokio::test]
    async fn embedding_search_with_zero_limit_returns_empty() {
        let store = SqliteStore::in_memory().unwrap();
        let query_emb = sparse_embedding(&[(0, 1.0)]);
        store.store(&make_memory("first", &[], base_time()), Some(&query_emb)).await.unwrap();

        let results = store
            .search_by_embedding(&query_emb, 0, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn get_expired_memory_returns_none() {
        let clock: Arc<dyn Clock> = Arc::new(MockClock::pinned(base_time()));
        let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
        let now = clock.now();
        let mut mem = make_memory("ephemeral", &[], now);
        // Already expired
        mem.expires_at = Some(now - chrono::Duration::seconds(1));
        let id = store.store(&mem, None).await.unwrap();
        assert!(store.get(&id, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_excludes_expired_memories() {
        let clock: Arc<dyn Clock> = Arc::new(MockClock::pinned(base_time()));
        let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
        let now = clock.now();

        let mut expired = make_memory("old", &["a"], now);
        expired.expires_at = Some(now - chrono::Duration::seconds(1));
        store.store(&expired, None).await.unwrap();

        let fresh = make_memory("new", &["a"], now);
        store.store(&fresh, None).await.unwrap();

        let results = store.list(MemoryFilter::default(), QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "new");
    }

    #[tokio::test]
    async fn evict_expired_removes_only_expired_rows() {
        let clock: Arc<dyn Clock> = Arc::new(MockClock::pinned(base_time()));
        let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
        let now = clock.now();

        let mut expired = make_memory("expired", &[], now);
        expired.expires_at = Some(now - chrono::Duration::seconds(1));
        let expired_id = store.store(&expired, None).await.unwrap();

        store.store(&make_memory("fresh", &[], now), None).await.unwrap();

        let deleted = store.evict_expired().await.unwrap();
        assert_eq!(deleted, 1);

        let listed = store.list(MemoryFilter::default(), QueryContext::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].content, "fresh");
        let tombstone = store.get_tombstone(&expired_id).await.unwrap().unwrap();
        assert_eq!(tombstone.memory_id, expired_id);
        assert_eq!(tombstone.deleted_by_principal, None);

        let deleted_again = store.evict_expired().await.unwrap();
        assert_eq!(deleted_again, 0);
    }

    #[tokio::test]
    async fn set_embedding_after_store() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("backfill me", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        // No embedding initially
        let got = store.get(&id, None).await.unwrap().unwrap();
        assert!(!got.has_embedding);

        // Set embedding
        let emb = vec![0.1_f32; 768];
        store.set_embedding(&id, &emb, 0).await.unwrap();

        let got = store.get(&id, None).await.unwrap().unwrap();
        assert!(got.has_embedding);

        // Should now be searchable by embedding
        let results = store.search_by_embedding(&emb, 1, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "backfill me");
    }

    #[tokio::test]
    async fn update_content() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("original", &["a"], base_time());
        let id = store.store(&mem, None).await.unwrap();

        let update = MemoryUpdate {
            content: Some("updated".into()),
            ..Default::default()
        };
        assert!(store.update(&id, &update).await.unwrap());

        let got = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(got.content, "updated");
        // Content change should reset has_embedding
        assert!(!got.has_embedding);
    }

    #[tokio::test]
    async fn update_tags() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("tagged", &["old-tag"], base_time());
        let id = store.store(&mem, None).await.unwrap();

        let update = MemoryUpdate {
            tags: Some(vec!["new-tag".into()]),
            ..Default::default()
        };
        assert!(store.update(&id, &update).await.unwrap());

        let got = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(got.tags, vec!["new-tag"]);
    }

    #[tokio::test]
    async fn update_nonexistent_returns_false() {
        let store = SqliteStore::in_memory().unwrap();
        let update = MemoryUpdate {
            content: Some("nope".into()),
            ..Default::default()
        };
        assert!(!store.update(&MemoryId::new(), &update).await.unwrap());
    }

    #[tokio::test]
    async fn access_policy_restricted_enforced() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("secret", &["classified"], base_time());
        mem.access_policy = AccessPolicy::Restricted { allowed: vec!["bot-a".into()] };
        let id = store.store(&mem, None).await.unwrap();

        // No caller → hidden
        assert!(store.get(&id, None).await.unwrap().is_none());
        // Allowed caller → visible
        assert!(store.get(&id, Some("bot-a")).await.unwrap().is_some());
        // Unauthorized caller → hidden
        assert!(store.get(&id, Some("bot-b")).await.unwrap().is_none());
        // Creator always has access
        assert!(store.get(&id, Some("test-agent")).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn access_policy_public_visible_without_caller() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("public info", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();
        // Public memory visible even without caller
        assert!(store.get(&id, None).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn text_search_escapes_like_wildcards() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("100% complete", &[], base_time()), None).await.unwrap();
        store.store(&make_memory("1000 items", &[], base_time()), None).await.unwrap();

        // Searching for "100%" should only match the literal percent, not wildcard
        let results = store.search_by_text("100%", 10, &MemoryFilter::default(), &QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "100% complete");

        // Underscore wildcard should also be escaped
        store.store(&make_memory("file_name.txt", &[], base_time()), None).await.unwrap();
        store.store(&make_memory("filename.txt", &[], base_time()), None).await.unwrap();

        let results = store.search_by_text("file_name", 10, &MemoryFilter::default(), &QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "file_name.txt");
    }

    #[tokio::test]
    async fn list_filters_by_access_policy() {
        let store = SqliteStore::in_memory().unwrap();

        let public = make_memory("public", &["a"], base_time());
        store.store(&public, None).await.unwrap();

        let mut restricted = make_memory("restricted", &["a"], base_time());
        restricted.access_policy = AccessPolicy::Restricted { allowed: vec!["bot-a".into()] };
        store.store(&restricted, None).await.unwrap();

        // No caller → only public
        let results = store.list(MemoryFilter::default(), QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "public");

        // With allowed caller → both
        let ctx = QueryContext { principal: Some("bot-a".into()) };
        let results = store.list(MemoryFilter::default(), ctx).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn tags_empty_means_no_tag_filter() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("mem1", &["a"], base_time()), None).await.unwrap();
        store.store(&make_memory("mem2", &["b"], base_time()), None).await.unwrap();

        let filter = MemoryFilter {
            tags: Some(Vec::new()),
            ..Default::default()
        };
        let listed = store.list(filter.clone(), QueryContext::default()).await.unwrap();
        assert_eq!(listed.len(), 2);

        let searched = store.search_by_text("mem", 10, &filter, &QueryContext::default()).await.unwrap();
        assert_eq!(searched.len(), 2);
    }

    #[tokio::test]
    async fn authorized_write_policy_enforced() {
        let store = SqliteStore::in_memory().unwrap();
        let mut public_mem = make_memory("public-owned", &[], base_time());
        public_mem.provenance.source_agent = Some("owner".into());
        let public_id = store.store(&public_mem, None).await.unwrap();

        let denied = store
            .update_authorized(
                &public_id,
                &MemoryUpdate {
                    content: Some("tampered".into()),
                    ..Default::default()
                },
                "other",
            )
            .await
            .unwrap();
        assert_eq!(denied.outcome, WriteOutcome::Denied);

        let allowed = store
            .update_authorized(
                &public_id,
                &MemoryUpdate {
                    content: Some("owner-updated".into()),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        assert_eq!(allowed.outcome, WriteOutcome::Applied);

        let mut restricted = make_memory("restricted", &[], base_time());
        restricted.provenance.source_agent = Some("owner".into());
        restricted.access_policy = AccessPolicy::Restricted { allowed: vec!["friend".into()] };
        let restricted_id = store.store(&restricted, None).await.unwrap();
        assert_eq!(store.delete_authorized(&restricted_id, "friend").await.unwrap(), WriteOutcome::Applied);
        let tombstone = store.get_tombstone(&restricted_id).await.unwrap().unwrap();
        assert_eq!(tombstone.deleted_by_principal.as_deref(), Some("friend"));
        assert_eq!(tombstone.check_access_level(Some("friend")), crate::types::AccessLevel::Full);
        assert_eq!(tombstone.check_access_level(Some("intruder")), crate::types::AccessLevel::Denied);
    }

    #[tokio::test]
    async fn update_authorized_returns_not_found_for_missing_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let outcome = store
            .update_authorized(
                &MemoryId::new(),
                &MemoryUpdate {
                    content: Some("does-not-exist".into()),
                    ..Default::default()
                },
                "caller",
            )
            .await
            .unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::NotFound);
        assert!(outcome.reembed_revision.is_none());
    }

    #[tokio::test]
    async fn update_authorized_noop_reports_existence_without_reembed() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("no-op target", &[], base_time());
        mem.provenance.source_agent = Some("owner".into());
        let id = store.store(&mem, None).await.unwrap();

        let outcome = store.update_authorized(&id, &MemoryUpdate::default(), "owner").await.unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Applied);
        assert!(outcome.reembed_revision.is_none());
    }

    #[tokio::test]
    async fn update_authorized_access_policy_only_updates_without_reembed() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("policy target", &[], base_time());
        mem.provenance.source_agent = Some("owner".into());
        let id = store.store(&mem, None).await.unwrap();

        let outcome = store
            .update_authorized(
                &id,
                &MemoryUpdate {
                    access_policy: Some(AccessPolicy::Restricted { allowed: vec!["friend".into()] }),
                    ..Default::default()
                },
                "owner",
            )
            .await
            .unwrap();
        assert_eq!(outcome.outcome, WriteOutcome::Applied);
        assert!(outcome.reembed_revision.is_none());

        let owner_view = store.get(&id, Some("owner")).await.unwrap().unwrap();
        assert!(matches!(owner_view.access_policy, AccessPolicy::Restricted { .. }));
    }

    #[tokio::test]
    async fn content_update_invalidates_embedding_until_fresh_write() {
        let store = SqliteStore::in_memory().unwrap();
        let mut emb = vec![0.0_f32; 768];
        emb[0] = 1.0;
        let mem = make_memory("semantic", &[], base_time());
        let id = store.store(&mem, Some(&emb)).await.unwrap();

        let before = store.search_by_embedding(&emb, 10, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert_eq!(before.len(), 1);

        assert!(
            store
                .update(&id, &MemoryUpdate {
                    content: Some("changed".into()),
                    ..Default::default()
                },)
                .await
                .unwrap()
        );

        let after = store.search_by_embedding(&emb, 10, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert!(after.is_empty());

        let stale_err = store.set_embedding(&id, &emb, 0).await.unwrap_err();
        assert!(matches!(stale_err, StoreError::Conflict(_)));

        store.set_embedding(&id, &emb, 1).await.unwrap();
        let refreshed = store.search_by_embedding(&emb, 10, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert_eq!(refreshed.len(), 1);
    }

    #[tokio::test]
    async fn set_embedding_replaces_existing_vector_row() {
        let store = SqliteStore::in_memory().unwrap();
        let original = sparse_embedding(&[(0, 1.0)]);
        let replacement = sparse_embedding(&[(1, 1.0)]);

        let id = store.store(&make_memory("replace-embedding", &[], base_time()), Some(&original)).await.unwrap();
        let before = store
            .search_by_embedding(&original, 1, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        let before_distance = before[0].distance.unwrap_or(0.0_f64);

        store.set_embedding(&id, &replacement, 0).await.unwrap();

        let old_results = store
            .search_by_embedding(&original, 5, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert_eq!(old_results.len(), 1);
        let old_distance = old_results[0].distance.unwrap_or(0.0_f64);
        assert!(old_distance > before_distance);

        let new_results = store
            .search_by_embedding(&replacement, 5, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert_eq!(new_results.len(), 1);
        assert_eq!(new_results[0].memory.id, id);

        let (map_count, vec_count): (i64, i64) = store
            .with_conn(|conn| {
                let map_count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get(0))?;
                let vec_count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| row.get(0))?;
                Ok((map_count, vec_count))
            })
            .await
            .unwrap();
        assert_eq!(map_count, 1);
        assert_eq!(vec_count, 1);
    }

    #[tokio::test]
    async fn set_embedding_after_delete_does_not_leave_orphans() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("to-delete", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();
        assert!(store.delete(&id).await.unwrap());

        let emb = vec![0.0_f32; 768];
        let err = store.set_embedding(&id, &emb, 0).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));

        let (map_count, vec_count): (i64, i64) = store
            .with_conn(|conn| {
                let map_count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get(0))?;
                let vec_count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| row.get(0))?;
                Ok((map_count, vec_count))
            })
            .await
            .unwrap();
        assert_eq!(map_count, 0);
        assert_eq!(vec_count, 0);
    }

    #[tokio::test]
    async fn set_embedding_wrong_dimensions_returns_error() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("embed me", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        // Store is configured with DEFAULT_TEST_DIMENSIONS dims; try to set a 512-dim vector
        let wrong_dim = vec![0.5_f32; 512];
        let err = store.set_embedding(&id, &wrong_dim, 0).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "expected Conflict error, got: {err:?}");
    }

    #[test]
    fn migrate_adds_embedding_revision_to_legacy_memories_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE memories (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                tags          TEXT NOT NULL,
                provenance    TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                expires_at    TEXT,
                has_embedding INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, expires_at, has_embedding)
            VALUES ('01H00000000000000000000000', 'legacy', '[]', '{\"source_agent\":\"legacy\"}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 0);
            ",
        )
        .unwrap();

        migrate_memories_add_embedding_revision(&conn).unwrap();

        let mut stmt = conn.prepare("PRAGMA table_info(memories)").unwrap();
        let columns: Vec<String> = stmt.query_map([], |row| row.get(1)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert!(columns.iter().any(|name| name == "embedding_revision"));

        let revision: i64 = conn
            .query_row("SELECT embedding_revision FROM memories WHERE id = '01H00000000000000000000000'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(revision, 0);
    }

    #[test]
    fn migrate_backfills_origin_conversation_for_legacy_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE memories (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                tags          TEXT NOT NULL,
                provenance    TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                expires_at    TEXT,
                has_embedding INTEGER NOT NULL DEFAULT 0,
                embedding_revision INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, expires_at, has_embedding, embedding_revision)
            VALUES
                ('legacy', 'l1', '[]', '{\"source_conversation\":\"conv-1\"}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 0, 0),
                ('new', 'l2', '[]', '{\"source_conversation\":\"project-1\",\"origin_conversation\":\"conv-2\"}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 0, 0),
                ('none', 'l3', '[]', '{}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 0, 0);
            ",
        )
        .unwrap();

        migrate_memories_backfill_origin_conversation(&conn).unwrap();

        let legacy_origin: Option<String> = conn
            .query_row("SELECT json_extract(provenance, '$.origin_conversation') FROM memories WHERE id = 'legacy'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(legacy_origin.as_deref(), Some("conv-1"));

        let new_origin: Option<String> = conn
            .query_row("SELECT json_extract(provenance, '$.origin_conversation') FROM memories WHERE id = 'new'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(new_origin.as_deref(), Some("conv-2"));

        let none_origin: Option<String> = conn
            .query_row("SELECT json_extract(provenance, '$.origin_conversation') FROM memories WHERE id = 'none'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(none_origin.is_none());
    }

    #[tokio::test]
    #[expect(clippy::let_underscore_must_use, reason = "best-effort temp file cleanup; files may not exist")]
    #[expect(let_underscore_drop, reason = "best-effort temp file cleanup — Result dropped immediately is fine")]
    async fn open_backfills_origin_conversation_for_existing_database() {
        let db_path = std::env::temp_dir().join(format!("localhold-migration-{}.db", ulid::Ulid::new()));
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE memories (
                    id            TEXT PRIMARY KEY,
                    content       TEXT NOT NULL,
                    tags          TEXT NOT NULL,
                    provenance    TEXT NOT NULL,
                    access_policy TEXT NOT NULL,
                    created_at    TEXT NOT NULL,
                    expires_at    TEXT,
                    has_embedding INTEGER NOT NULL DEFAULT 0,
                    embedding_revision INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, expires_at, has_embedding, embedding_revision)
                VALUES ('01H00000000000000000000000', 'legacy memory', '[]', '{\"source_conversation\":\"conv-legacy\"}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 0, 0);
                ",
            )
            .unwrap();
        }

        let store = SqliteStore::open(&db_path, 768).unwrap();
        let filtered = store
            .list(
                MemoryFilter {
                    origin_scope: Some("conv-legacy".into()),
                    ..Default::default()
                },
                QueryContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].content, "legacy memory");
        assert_eq!(filtered[0].provenance.origin_conversation.as_deref(), Some("conv-legacy"));

        drop(store);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[test]
    fn migrate_embedding_map_adds_fk_and_drops_orphans() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            CREATE TABLE memories (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                tags          TEXT NOT NULL,
                provenance    TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                expires_at    TEXT,
                has_embedding INTEGER NOT NULL DEFAULT 0,
                embedding_revision INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE memory_embedding_map (
                memory_id TEXT PRIMARY KEY,
                vec_rowid INTEGER NOT NULL UNIQUE
            );
            INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, expires_at, has_embedding, embedding_revision)
            VALUES ('alive', 'ok', '[]', '{\"source_agent\":\"a\"}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 1, 0);
            INSERT INTO memory_embedding_map(memory_id, vec_rowid) VALUES ('alive', 1);
            INSERT INTO memory_embedding_map(memory_id, vec_rowid) VALUES ('orphan', 2);
            ",
        )
        .unwrap();

        migrate_memory_embedding_map_fk(&conn).unwrap();

        let mut stmt = conn.prepare("PRAGMA foreign_key_list(memory_embedding_map)").unwrap();
        let fks: Vec<(String, String)> = stmt.query_map([], |row| Ok((row.get(2)?, row.get(3)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert!(fks.iter().any(|(table, from_col)| table == "memories" && from_col == "memory_id"));

        let mut stmt = conn.prepare("SELECT memory_id FROM memory_embedding_map ORDER BY memory_id").unwrap();
        let ids: Vec<String> = stmt.query_map([], |row| row.get(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(ids, vec!["alive"]);
    }

    #[test]
    fn migrate_embedding_map_is_noop_when_fk_already_present() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            CREATE TABLE memories (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                tags          TEXT NOT NULL,
                provenance    TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                expires_at    TEXT,
                has_embedding INTEGER NOT NULL DEFAULT 0,
                embedding_revision INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE memory_embedding_map (
                memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                vec_rowid INTEGER NOT NULL UNIQUE
            );
            INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, expires_at, has_embedding, embedding_revision)
            VALUES ('alive', 'ok', '[]', '{\"source_agent\":\"a\"}', '{\"type\":\"public\"}', '2025-01-01T00:00:00Z', NULL, 1, 0);
            INSERT INTO memory_embedding_map(memory_id, vec_rowid) VALUES ('alive', 1);
            ",
        )
        .unwrap();

        migrate_memory_embedding_map_fk(&conn).unwrap();

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn delete_with_existing_embedding_cleans_map_and_vector_rows() {
        let store = SqliteStore::in_memory().unwrap();
        let emb = sparse_embedding(&[(0, 1.0)]);
        let id = store.store(&make_memory("with-embedding", &[], base_time()), Some(&emb)).await.unwrap();

        assert!(store.delete(&id).await.unwrap());

        let (map_count, vec_count): (i64, i64) = store
            .with_conn(|conn| {
                let map_count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get(0))?;
                let vec_count: i64 = conn.query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| row.get(0))?;
                Ok((map_count, vec_count))
            })
            .await
            .unwrap();
        assert_eq!(map_count, 0);
        assert_eq!(vec_count, 0);
    }

    #[tokio::test]
    async fn list_paginates_to_fill_limit_when_newest_rows_are_denied() {
        let store = SqliteStore::in_memory().unwrap();

        for i in 0_i32..5_i32 {
            store.store(&make_memory(&format!("allowed-{i}"), &[], base_time()), None).await.unwrap();
        }

        for i in 0_i32..20_i32 {
            let mut denied = make_memory(&format!("denied-{i}"), &[], base_time());
            denied.provenance.source_agent = Some("owner".into());
            denied.access_policy = AccessPolicy::Restricted { allowed: vec!["friend".into()] };
            store.store(&denied, None).await.unwrap();
        }

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let filter = MemoryFilter {
            limit: Some(5),
            ..Default::default()
        };
        let results = store.list(filter, ctx).await.unwrap();

        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|m| m.content.starts_with("allowed-")));
    }

    #[tokio::test]
    async fn text_search_paginates_to_fill_limit_when_newest_rows_are_denied() {
        let store = SqliteStore::in_memory().unwrap();

        for i in 0_i32..5_i32 {
            store.store(&make_memory(&format!("needle allowed-{i}"), &[], base_time()), None).await.unwrap();
        }

        for i in 0_i32..20_i32 {
            let mut denied = make_memory(&format!("needle denied-{i}"), &[], base_time());
            denied.provenance.source_agent = Some("owner".into());
            denied.access_policy = AccessPolicy::Restricted { allowed: vec!["friend".into()] };
            store.store(&denied, None).await.unwrap();
        }

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let results = store.search_by_text("needle", 5, &MemoryFilter::default(), &ctx).await.unwrap();

        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.memory.content.contains("allowed-")));
    }

    #[tokio::test]
    async fn fts_search_paginates_to_fill_limit_when_top_ranked_rows_are_denied() {
        let store = SqliteStore::in_memory().unwrap();

        for i in 0_i32..5_i32 {
            store.store(&make_memory(&format!("needle allowed-{i}"), &[], base_time()), None).await.unwrap();
        }

        let denied_time = base_time() + chrono::Duration::hours(1);
        for i in 0_i32..20_i32 {
            let mut denied = make_memory(&format!("needle denied-{i}"), &[], denied_time);
            denied.provenance.source_agent = Some("owner".into());
            denied.access_policy = AccessPolicy::Restricted { allowed: vec!["friend".into()] };
            store.store(&denied, None).await.unwrap();
        }

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let results = store.search_by_fts("needle", 5, &MemoryFilter::default(), &ctx, None).await.unwrap();

        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.memory.content.contains("allowed-")));
    }

    #[tokio::test]
    async fn text_and_fts_search_skip_hidden_content_redacted_memories() {
        let store = SqliteStore::in_memory().unwrap();

        for i in 0_i32..20_i32 {
            let mut hidden = make_memory(&format!("needle hidden-{i}"), &[], base_time());
            hidden.provenance.source_agent = Some("owner".into());
            hidden.access_policy = AccessPolicy::Redacted { visible_fields: Vec::new() };
            store.store(&hidden, None).await.unwrap();
        }

        for i in 0_i32..5_i32 {
            let visible = make_memory(&format!("needle allowed-{i}"), &[], base_time());
            store.store(&visible, None).await.unwrap();
        }

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let text_results = store.search_by_text("needle", 5, &MemoryFilter::default(), &ctx).await.unwrap();
        let fts_results = store.search_by_fts("needle", 5, &MemoryFilter::default(), &ctx, None).await.unwrap();

        assert_eq!(text_results.len(), 5);
        assert_eq!(fts_results.len(), 5);
        assert!(text_results.iter().all(|r| r.memory.content.contains("allowed-")));
        assert!(fts_results.iter().all(|r| r.memory.content.contains("allowed-")));
    }

    #[tokio::test]
    async fn list_time_range_filter_respects_after_and_before_bounds() {
        let clock: Arc<dyn Clock> = Arc::new(MockClock::pinned(base_time()));
        let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
        let now = clock.now();

        let mut old = make_memory("old", &[], now);
        old.created_at = now - chrono::Duration::hours(2);
        store.store(&old, None).await.unwrap();

        let mut in_range = make_memory("in-range", &[], now);
        in_range.created_at = now - chrono::Duration::minutes(30);
        store.store(&in_range, None).await.unwrap();

        let mut future = make_memory("future", &[], now);
        future.created_at = now + chrono::Duration::hours(2);
        store.store(&future, None).await.unwrap();

        let filter = MemoryFilter {
            time_range: Some(crate::types::TimeRange {
                after: Some(now - chrono::Duration::hours(1)),
                before: Some(now + chrono::Duration::hours(1)),
            }),
            ..Default::default()
        };
        let results = store.list(filter, QueryContext::default()).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "in-range");
    }

    #[tokio::test]
    async fn embedding_search_retries_when_initial_candidates_are_denied() {
        let store = SqliteStore::in_memory().unwrap();
        let query = sparse_embedding(&[(0, 1.0)]);

        // The first vector-search pass fetches limit*OVERFETCH_FACTOR (5*4 = 20)
        // candidates. Make those closest rows denied for caller "outsider".
        for i in 0_i32..20_i32 {
            let mut denied = make_memory(&format!("denied-closest-{i}"), &[], base_time());
            denied.provenance.source_agent = Some("owner".into());
            denied.access_policy = AccessPolicy::Restricted { allowed: vec!["friend".into()] };
            store.store(&denied, Some(&query)).await.unwrap();
        }

        // Add visible rows that are slightly farther away; retry should include them.
        for (i, x0) in [0.8_f32, 0.79, 0.78, 0.77, 0.76].into_iter().enumerate() {
            let visible = make_memory(&format!("allowed-farther-{i}"), &[], base_time());
            let emb = sparse_embedding(&[(0, x0), (1, 0.2)]);
            store.store(&visible, Some(&emb)).await.unwrap();
        }

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let results = store.search_by_embedding(&query, 5, &MemoryFilter::default(), &ctx, None).await.unwrap();

        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.memory.content.starts_with("allowed-farther-")));
    }

    #[tokio::test]
    async fn embedding_search_retries_when_initial_candidates_are_hidden_content_redacted() {
        let store = SqliteStore::in_memory().unwrap();
        let query = sparse_embedding(&[(0, 1.0)]);

        for i in 0_i32..20_i32 {
            let mut hidden = make_memory(&format!("hidden-closest-{i}"), &[], base_time());
            hidden.provenance.source_agent = Some("owner".into());
            hidden.access_policy = AccessPolicy::Redacted { visible_fields: Vec::new() };
            store.store(&hidden, Some(&query)).await.unwrap();
        }

        for (i, x0) in [0.8_f32, 0.79, 0.78, 0.77, 0.76].into_iter().enumerate() {
            let visible = make_memory(&format!("allowed-farther-{i}"), &[], base_time());
            let emb = sparse_embedding(&[(0, x0), (1, 0.2)]);
            store.store(&visible, Some(&emb)).await.unwrap();
        }

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let results = store.search_by_embedding(&query, 5, &MemoryFilter::default(), &ctx, None).await.unwrap();

        assert_eq!(results.len(), 5);
        assert!(results.iter().all(|r| r.memory.content.starts_with("allowed-farther-")));
    }

    #[tokio::test]
    async fn list_does_not_restore_hidden_entities_on_redacted_memories() {
        let store = SqliteStore::in_memory().unwrap();

        let mut memory = make_memory("visible content", &[], base_time());
        memory.provenance.source_agent = Some("owner".into());
        memory.access_policy = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        };
        memory.entities = vec![Entity::new("Alice", "person").unwrap()];
        store.store(&memory, None).await.unwrap();

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let results = store.list(MemoryFilter::default(), ctx).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "visible content");
        assert!(results[0].entities.is_empty(), "redacted list results should not restore hidden entities");
    }

    #[tokio::test]
    async fn embedding_search_does_not_restore_hidden_entities_on_redacted_memories() {
        let store = SqliteStore::in_memory().unwrap();
        let query = sparse_embedding(&[(0, 1.0)]);

        let mut memory = make_memory("semantic secret", &[], base_time());
        memory.provenance.source_agent = Some("owner".into());
        memory.access_policy = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        };
        memory.entities = vec![Entity::new("Alice", "person").unwrap()];
        store.store(&memory, Some(&query)).await.unwrap();

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let results = store.search_by_embedding(&query, 1, &MemoryFilter::default(), &ctx, None).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "semantic secret");
        assert!(results[0].memory.entities.is_empty(), "redacted search results should not restore hidden entities");
    }

    #[tokio::test]
    async fn fts_search_filters_and_redaction_precede_entity_hydration() {
        let store = SqliteStore::in_memory().unwrap();

        let mut filtered = make_memory("coffee filtered", &["skip"], base_time());
        filtered.entities = vec![Entity::new("Filtered", "person").unwrap()];
        store.store(&filtered, None).await.unwrap();

        let mut denied = make_memory("coffee denied", &["keep"], base_time());
        denied.provenance.source_agent = Some("owner".into());
        denied.access_policy = AccessPolicy::Restricted { allowed: vec!["owner".into()] };
        denied.entities = vec![Entity::new("Denied", "person").unwrap()];
        store.store(&denied, None).await.unwrap();

        let mut redacted = make_memory("coffee visible", &["keep"], base_time());
        redacted.provenance.source_agent = Some("owner".into());
        redacted.access_policy = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content, RedactableField::Tags],
        };
        redacted.entities = vec![Entity::new("Hidden", "person").unwrap()];
        store.store(&redacted, None).await.unwrap();

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let filter = MemoryFilter {
            tags: Some(vec!["keep".into()]),
            ..Default::default()
        };

        let results = store.search_by_fts("coffee", 10, &filter, &ctx, None).await.unwrap();

        assert_eq!(results.len(), 1, "filtered and denied rows should be discarded before the final result set");
        assert_eq!(results[0].memory.content, "coffee visible");
        assert!(results[0].memory.entities.is_empty(), "redacted FTS results should not restore hidden entities");
    }

    #[tokio::test]
    async fn ownerless_public_memory_allows_authorized_writes() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("ownerless-public", &[], base_time());
        mem.provenance.source_agent = None;
        mem.access_policy = AccessPolicy::Public;
        let id = store.store(&mem, None).await.unwrap();

        let update = store
            .update_authorized(
                &id,
                &MemoryUpdate {
                    content: Some("updated-by-caller".into()),
                    ..Default::default()
                },
                "caller-a",
            )
            .await
            .unwrap();
        assert_eq!(update.outcome, WriteOutcome::Applied);

        let after = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(after.content, "updated-by-caller");
        assert_eq!(store.delete_authorized(&id, "caller-b").await.unwrap(), WriteOutcome::Applied);
    }

    #[tokio::test]
    async fn ownerless_redacted_memory_denies_writes() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("ownerless-redacted", &[], base_time());
        mem.provenance.source_agent = None;
        mem.access_policy = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content, RedactableField::Tags],
        };
        let id = store.store(&mem, None).await.unwrap();

        // Ownerless redacted memories deny writes to prevent privilege escalation (T2).
        let updated = store
            .update_authorized(
                &id,
                &MemoryUpdate {
                    tags: Some(vec!["touched".into()]),
                    ..Default::default()
                },
                "maintainer",
            )
            .await
            .unwrap();
        assert_eq!(updated.outcome, WriteOutcome::Denied);

        // Content should remain unchanged.
        let after = store.get(&id, Some("maintainer")).await.unwrap().unwrap();
        assert_eq!(after.content, "ownerless-redacted");
    }

    #[test]
    fn migration_error_keeps_foreign_keys_enabled() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE memory_embedding_map (
                memory_id TEXT PRIMARY KEY,
                vec_rowid INTEGER NOT NULL UNIQUE
            );
            INSERT INTO memory_embedding_map(memory_id, vec_rowid) VALUES ('ghost', 1);
            ",
        )
        .unwrap();

        let err = migrate_memory_embedding_map_fk(&conn).unwrap_err();
        assert!(err.to_string().contains("no such table: memories"));

        let fk_enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap();
        assert_eq!(fk_enabled, 1);
    }

    // ── count tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn count_empty_store() {
        let store = SqliteStore::in_memory().unwrap();
        let stats = store.count(MemoryFilter::default(), QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.with_embedding, 0);
        assert_eq!(stats.without_embedding, 0);
        assert_eq!(stats.expired, 0);
        assert!(stats.by_tag.is_empty());
        assert!(stats.by_agent_label.is_empty());
    }

    #[tokio::test]
    async fn count_total_and_embedding_breakdown() {
        let store = SqliteStore::in_memory().unwrap();
        let emb1 = sparse_embedding(&[(0, 1.0)]);
        let emb2 = sparse_embedding(&[(1, 1.0)]);

        store.store(&make_memory("with-emb-1", &[], base_time()), Some(&emb1)).await.unwrap();
        store.store(&make_memory("with-emb-2", &[], base_time()), Some(&emb2)).await.unwrap();
        store.store(&make_memory("no-emb", &[], base_time()), None).await.unwrap();

        let stats = store.count(MemoryFilter::default(), QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.with_embedding, 2);
        assert_eq!(stats.without_embedding, 1);
    }

    #[tokio::test]
    async fn count_respects_tag_filter() {
        let store = SqliteStore::in_memory().unwrap();
        store.store(&make_memory("m1", &["a"], base_time()), None).await.unwrap();
        store.store(&make_memory("m2", &["a", "b"], base_time()), None).await.unwrap();
        store.store(&make_memory("m3", &["b"], base_time()), None).await.unwrap();

        let filter = MemoryFilter {
            tags: Some(vec!["a".into()]),
            ..Default::default()
        };
        let stats = store.count(filter, QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats.total, 2);
    }

    #[tokio::test]
    async fn count_tag_breakdown_ordered_by_frequency() {
        let store = SqliteStore::in_memory().unwrap();
        for _ in 0_i32..3_i32 {
            store.store(&make_memory("p", &["popular"], base_time()), None).await.unwrap();
        }
        for _ in 0_i32..2_i32 {
            store.store(&make_memory("m", &["medium"], base_time()), None).await.unwrap();
        }
        store.store(&make_memory("r", &["rare"], base_time()), None).await.unwrap();

        let stats = store.count(MemoryFilter::default(), QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats.by_tag.len(), 3);
        assert_eq!(stats.by_tag[0], ("popular".to_owned(), 3));
        assert_eq!(stats.by_tag[1], ("medium".to_owned(), 2));
        assert_eq!(stats.by_tag[2], ("rare".to_owned(), 1));
    }

    #[tokio::test]
    async fn count_tag_breakdown_respects_limit() {
        let store = SqliteStore::in_memory().unwrap();
        for _ in 0_i32..3_i32 {
            store.store(&make_memory("p", &["popular"], base_time()), None).await.unwrap();
        }
        for _ in 0_i32..2_i32 {
            store.store(&make_memory("m", &["medium"], base_time()), None).await.unwrap();
        }
        store.store(&make_memory("r", &["rare"], base_time()), None).await.unwrap();

        let stats = store.count(MemoryFilter::default(), QueryContext::default(), 2).await.unwrap();
        assert_eq!(stats.by_tag.len(), 2);
        assert_eq!(stats.by_tag[0], ("popular".to_owned(), 3));
        assert_eq!(stats.by_tag[1], ("medium".to_owned(), 2));
    }

    #[tokio::test]
    async fn count_agent_breakdown() {
        let store = SqliteStore::in_memory().unwrap();

        let mut m1 = make_memory("from-alpha", &[], base_time());
        m1.provenance.source_agent = Some("alpha".into());
        store.store(&m1, None).await.unwrap();

        let mut m2 = make_memory("from-alpha-2", &[], base_time());
        m2.provenance.source_agent = Some("alpha".into());
        store.store(&m2, None).await.unwrap();

        let mut m3 = make_memory("from-beta", &[], base_time());
        m3.provenance.source_agent = Some("beta".into());
        store.store(&m3, None).await.unwrap();

        let stats = store.count(MemoryFilter::default(), QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats.by_agent_label.len(), 2);
        assert_eq!(stats.by_agent_label[0], ("alpha".to_owned(), 2));
        assert_eq!(stats.by_agent_label[1], ("beta".to_owned(), 1));
    }

    #[tokio::test]
    async fn count_expired_memories() {
        let clock: Arc<dyn Clock> = Arc::new(MockClock::pinned(base_time()));
        let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
        let now = clock.now();

        let mut expired = make_memory("old", &[], now);
        expired.expires_at = Some(now - chrono::Duration::hours(1));
        store.store(&expired, None).await.unwrap();

        store.store(&make_memory("fresh", &[], now), None).await.unwrap();

        let stats = store.count(MemoryFilter::default(), QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats.expired, 1);
    }

    #[tokio::test]
    async fn count_access_policy_no_caller_only_public() {
        let store = SqliteStore::in_memory().unwrap();

        store.store(&make_memory("public-mem", &[], base_time()), None).await.unwrap();

        let mut restricted = make_memory("restricted-mem", &[], base_time());
        restricted.access_policy = AccessPolicy::Restricted { allowed: vec!["trusted".into()] };
        store.store(&restricted, None).await.unwrap();

        // No principal -> only public counted
        let stats_no_caller = store.count(MemoryFilter::default(), QueryContext::default(), 10).await.unwrap();
        assert_eq!(stats_no_caller.total, 1);

        // With principal in allowed list -> both counted
        let ctx = QueryContext {
            principal: Some("trusted".into()),
        };
        let stats_with_caller = store.count(MemoryFilter::default(), ctx, 10).await.unwrap();
        assert_eq!(stats_with_caller.total, 2);
    }

    #[tokio::test]
    async fn count_redacted_memories_do_not_leak_hidden_breakdowns() {
        let store = SqliteStore::in_memory().unwrap();

        let mut redacted = make_memory("redacted", &["secret-tag"], base_time());
        redacted.provenance.source_agent = Some("owner".into());
        redacted.access_policy = AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        };
        store.store(&redacted, None).await.unwrap();

        let ctx = QueryContext {
            principal: Some("outsider".into()),
        };
        let stats = store.count(MemoryFilter::default(), ctx, 10).await.unwrap();

        assert_eq!(stats.total, 1);
        assert!(stats.by_tag.is_empty(), "hidden tags should not appear in count breakdowns");
        assert!(stats.by_agent_label.is_empty(), "hidden provenance should not appear in count breakdowns");
    }

    // ── store_batch tests ───────────────────────────────────────────

    #[tokio::test]
    async fn store_batch_single() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("batch-single", &["x"], base_time());
        let id = mem.id;
        let ids = store.store_batch(&[MemoryWithEmbedding { memory: mem, embedding: None }]).await.unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], id);

        let fetched = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(fetched.content, "batch-single");
    }

    #[tokio::test]
    async fn store_batch_multiple() {
        let store = SqliteStore::in_memory().unwrap();
        let batch: Vec<MemoryWithEmbedding> = (0_i32..5_i32)
            .map(|i| MemoryWithEmbedding {
                memory: make_memory(&format!("batch-{i}"), &[], base_time()),
                embedding: None,
            })
            .collect();
        let expected_ids: Vec<MemoryId> = batch.iter().map(|mwe| mwe.memory.id).collect();

        let ids = store.store_batch(&batch).await.unwrap();
        assert_eq!(ids.len(), 5);
        assert_eq!(ids, expected_ids);

        for (i, id) in ids.iter().enumerate() {
            let fetched = store.get(id, None).await.unwrap().unwrap();
            assert_eq!(fetched.content, format!("batch-{i}"));
        }
    }

    #[tokio::test]
    async fn store_batch_with_embeddings() {
        let store = SqliteStore::in_memory().unwrap();
        let emb = sparse_embedding(&[(0, 1.0)]);
        let mem_with = make_memory("with-emb", &[], base_time());
        let mem_without = make_memory("without-emb", &[], base_time());
        let id_with = mem_with.id;
        let id_without = mem_without.id;

        let ids = store
            .store_batch(&[
                MemoryWithEmbedding {
                    memory: mem_with,
                    embedding: Some(emb.clone()),
                },
                MemoryWithEmbedding {
                    memory: mem_without,
                    embedding: None,
                },
            ])
            .await
            .unwrap();
        assert_eq!(ids.len(), 2);

        let fetched_with = store.get(&id_with, None).await.unwrap().unwrap();
        assert!(fetched_with.has_embedding);

        let fetched_without = store.get(&id_without, None).await.unwrap().unwrap();
        assert!(!fetched_without.has_embedding);

        // The embedded memory should be findable by vector search
        let query = sparse_embedding(&[(0, 0.9)]);
        let results = store
            .search_by_embedding(&query, 10, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.id, id_with);
    }

    // ── has_embedding filter tests ──────────────────────────────────

    #[tokio::test]
    async fn list_with_has_embedding_filter() {
        let store = SqliteStore::in_memory().unwrap();
        let emb = sparse_embedding(&[(0, 1.0)]);

        store.store(&make_memory("embedded", &[], base_time()), Some(&emb)).await.unwrap();
        store.store(&make_memory("plain", &[], base_time()), None).await.unwrap();

        // has_embedding = true
        let filter_true = MemoryFilter {
            has_embedding: Some(true),
            ..Default::default()
        };
        let results = store.list(filter_true, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "embedded");

        // has_embedding = false
        let filter_false = MemoryFilter {
            has_embedding: Some(false),
            ..Default::default()
        };
        let results = store.list(filter_false, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "plain");

        // has_embedding = None (no filter)
        let results = store.list(MemoryFilter::default(), QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn text_search_with_has_embedding_filter() {
        let store = SqliteStore::in_memory().unwrap();
        let emb = sparse_embedding(&[(0, 1.0)]);

        store.store(&make_memory("needle embedded", &[], base_time()), Some(&emb)).await.unwrap();
        store.store(&make_memory("needle plain", &[], base_time()), None).await.unwrap();

        let filter = MemoryFilter {
            has_embedding: Some(true),
            ..Default::default()
        };
        let results = store.search_by_text("needle", 10, &filter, &QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "needle embedded");
    }

    // ── list_for_reembed tests ──────────────────────────────────────

    #[tokio::test]
    async fn list_for_reembed_returns_unembedded_only() {
        let store = SqliteStore::in_memory().unwrap();
        let emb = sparse_embedding(&[(0, 1.0)]);

        store.store(&make_memory("no-emb-1", &[], base_time()), None).await.unwrap();
        store.store(&make_memory("no-emb-2", &[], base_time()), None).await.unwrap();
        store.store(&make_memory("has-emb", &[], base_time()), Some(&emb)).await.unwrap();

        let reembed = store.list_for_reembed(10).await.unwrap();
        assert_eq!(reembed.len(), 2);
        let contents: Vec<&str> = reembed.iter().map(|(_, c, _)| c.as_str()).collect();
        assert!(contents.contains(&"no-emb-1"));
        assert!(contents.contains(&"no-emb-2"));
    }

    #[tokio::test]
    async fn list_for_reembed_respects_limit() {
        let store = SqliteStore::in_memory().unwrap();
        for i in 0_i32..5_i32 {
            store.store(&make_memory(&format!("mem-{i}"), &[], base_time()), None).await.unwrap();
        }

        let reembed = store.list_for_reembed(2).await.unwrap();
        assert_eq!(reembed.len(), 2);
    }

    #[tokio::test]
    async fn list_for_reembed_oldest_first() {
        let clock: Arc<dyn Clock> = Arc::new(MockClock::pinned(base_time()));
        let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
        let now = clock.now();

        let mut m1 = make_memory("old", &[], now);
        m1.created_at = now - chrono::Duration::hours(3);
        store.store(&m1, None).await.unwrap();

        let mut m2 = make_memory("medium", &[], now);
        m2.created_at = now - chrono::Duration::hours(2);
        store.store(&m2, None).await.unwrap();

        let mut m3 = make_memory("recent", &[], now);
        m3.created_at = now - chrono::Duration::hours(1);
        store.store(&m3, None).await.unwrap();

        let reembed = store.list_for_reembed(10).await.unwrap();
        assert_eq!(reembed.len(), 3);
        assert_eq!(reembed[0].1, "old");
        assert_eq!(reembed[1].1, "medium");
        assert_eq!(reembed[2].1, "recent");
    }

    #[tokio::test]
    async fn claim_for_reembed_leases_rows_until_timeout_or_release() {
        let clock = Arc::new(MockClock::pinned(base_time()));
        let store_clock: Arc<dyn Clock> = Arc::<MockClock>::clone(&clock);
        let store = SqliteStore::in_memory_with_clock(store_clock).unwrap();
        let id = store.store(&make_memory("claim me", &[], base_time()), None).await.unwrap();

        let first = store.claim_for_reembed(10).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, id);

        let leased = store.claim_for_reembed(10).await.unwrap();
        assert!(leased.is_empty(), "fresh claim should hide the row from duplicate recovery work");

        assert!(
            store
                .release_embedding_claim(&first[0].id, first[0].embedding_revision, &first[0].claim_token)
                .await
                .unwrap()
        );
        let after_release = store.claim_for_reembed(10).await.unwrap();
        assert_eq!(after_release.len(), 1);

        let token_before_timeout = after_release[0].claim_token.clone();
        clock.advance(chrono::Duration::seconds(301));
        let after_timeout = store.claim_for_reembed(10).await.unwrap();
        assert_eq!(after_timeout.len(), 1);
        assert_ne!(after_timeout[0].claim_token, token_before_timeout);
    }

    #[tokio::test]
    async fn set_embedding_clears_reembed_claim() {
        let store = SqliteStore::in_memory().unwrap();
        let id = store.store(&make_memory("claim then embed", &[], base_time()), None).await.unwrap();
        let claim = store.claim_for_reembed(10).await.unwrap().pop().unwrap();

        store.set_embedding(&id, &sparse_embedding(&[(0, 1.0)]), claim.embedding_revision).await.unwrap();

        assert!(store.claim_for_reembed(10).await.unwrap().is_empty());
        assert!(!store.release_embedding_claim(&id, claim.embedding_revision, &claim.claim_token).await.unwrap());
    }

    // ── get_for_reembed tests ───────────────────────────────────────

    #[tokio::test]
    async fn get_for_reembed_checks_authorization() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("owned-content", &[], base_time());
        mem.provenance.source_agent = Some("owner".into());
        mem.access_policy = AccessPolicy::Public;
        let id = store.store(&mem, None).await.unwrap();

        // Owner should have access
        let result = store.get_for_reembed(&id, "owner").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "owned-content");

        // Non-owner should be denied (public write access is owner-only when owner exists)
        let result = store.get_for_reembed(&id, "intruder").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_for_reembed_not_found() {
        let store = SqliteStore::in_memory().unwrap();
        let result = store.get_for_reembed(&MemoryId::new(), "anyone").await.unwrap();
        assert!(result.is_none());
    }

    fn sparse_embedding(entries: &[(usize, f32)]) -> Vec<f32> {
        let mut emb = vec![0.0_f32; 768];
        for &(idx, val) in entries {
            emb[idx] = val;
        }
        emb
    }

    // -- FTS5 search tests --------------------------------------------------

    #[tokio::test]
    async fn fts_available_on_in_memory_store() {
        let store = SqliteStore::in_memory().unwrap();
        assert!(store.fts_available(), "FTS5 should be available in bundled rusqlite");
    }

    #[tokio::test]
    async fn fts_search_finds_stored_content() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("coffee purchase for account ABC-123", &[], base_time());
        store.store(&mem, None).await.unwrap();

        let results = store.search_by_fts("coffee", 10, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "coffee purchase for account ABC-123");
    }

    #[tokio::test]
    async fn fts_search_finds_exact_identifier() {
        let store = SqliteStore::in_memory().unwrap();
        let mem1 = make_memory("coffee purchase for account ABC-123", &[], base_time());
        let mem2 = make_memory("tea order for account XYZ-789", &[], base_time());
        store.store(&mem1, None).await.unwrap();
        store.store(&mem2, None).await.unwrap();

        // FTS5 should find the exact identifier token
        let results = store.search_by_fts("ABC-123", 10, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "coffee purchase for account ABC-123");
    }

    #[tokio::test]
    async fn fts_search_with_zero_limit_returns_empty() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("findable content", &[], base_time());
        store.store(&mem, None).await.unwrap();

        let results = store.search_by_fts("findable", 0, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn fts_search_respects_access_policy() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("restricted FTS content", &[], base_time());
        mem.access_policy = AccessPolicy::Restricted {
            allowed: vec!["allowed-agent".into()],
        };
        mem.provenance.source_agent = Some("owner-agent".into());
        store.store(&mem, None).await.unwrap();

        // No caller → denied
        let results = store
            .search_by_fts("restricted", 10, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert!(results.is_empty(), "no caller should not see restricted memories");

        // Allowed caller → visible
        let ctx = QueryContext {
            principal: Some("allowed-agent".into()),
        };
        let results = store.search_by_fts("restricted", 10, &MemoryFilter::default(), &ctx, None).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_search_special_characters_sanitized() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("hello world", &[], base_time());
        store.store(&mem, None).await.unwrap();

        // Queries with FTS5 special characters should not cause errors
        let results = store
            .search_by_fts("hello AND world OR NOT something", 10, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        // Should not panic or error — results may vary based on tokenization
        assert!(results.len() <= 1);
    }

    #[tokio::test]
    async fn fts_search_updated_content_is_reindexed() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("original content", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        // Update content
        let update = MemoryUpdate {
            content: Some("updated content with new keywords".into()),
            ..MemoryUpdate::default()
        };
        store.update(&id, &update).await.unwrap();

        // Old content should not be found
        let results = store.search_by_fts("original", 10, &MemoryFilter::default(), &QueryContext::default(), None).await.unwrap();
        assert!(results.is_empty(), "old content should not be found after update");

        // New content should be found
        let results = store
            .search_by_fts("updated keywords", 10, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_search_deleted_content_is_removed() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("deletable content", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        store.delete(&id).await.unwrap();

        let results = store
            .search_by_fts("deletable", 10, &MemoryFilter::default(), &QueryContext::default(), None)
            .await
            .unwrap();
        assert!(results.is_empty(), "deleted content should not be found");
    }

    // -----------------------------------------------------------------------
    // Wave 1: Memory type, importance, access tracking
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn store_memory_with_type_and_importance() {
        let store = SqliteStore::in_memory().unwrap();
        let mut mem = make_memory("step-by-step guide", &["howto"], base_time());
        mem.memory_type = crate::types::MemoryType::Procedural;
        mem.importance = crate::types::Importance::new(0.9_f64);
        let id = store.store(&mem, None).await.unwrap();

        let retrieved = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(retrieved.memory_type, crate::types::MemoryType::Procedural);
        assert!((retrieved.importance.value() - 0.9).abs() < f64::EPSILON, "importance should be 0.9");
        assert_eq!(retrieved.impression_count, 0);
        assert!(retrieved.last_impressed_at.is_none());
    }

    #[tokio::test]
    async fn filter_by_memory_type() {
        let store = SqliteStore::in_memory().unwrap();
        let mut semantic = make_memory("a fact", &[], base_time());
        semantic.memory_type = crate::types::MemoryType::Semantic;
        store.store(&semantic, None).await.unwrap();

        let mut episodic = make_memory("an event", &[], base_time());
        episodic.memory_type = crate::types::MemoryType::Episodic;
        store.store(&episodic, None).await.unwrap();

        let filter = MemoryFilter {
            memory_type: Some(crate::types::MemoryType::Episodic),
            ..Default::default()
        };
        let results = store.list(filter, QueryContext::default()).await.unwrap();
        assert_eq!(results.len(), 1, "should find only the episodic memory");
        assert_eq!(results[0].memory_type, crate::types::MemoryType::Episodic);
    }

    #[tokio::test]
    async fn impression_count_increments_on_search() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("searchable needle", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        // Before any search
        let before = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(before.impression_count, 0);
        assert!(before.last_impressed_at.is_none());

        // Record access
        store.record_search_impression(&[id]).await.unwrap();

        let after = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(after.impression_count, 1, "access count should increment");
        assert!(after.last_impressed_at.is_some(), "last_impressed_at should be set");

        // Record access again
        store.record_search_impression(&[id]).await.unwrap();
        let after2 = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(after2.impression_count, 2, "access count should increment again");
    }

    #[tokio::test]
    async fn update_importance() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("adjustable", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        let update = MemoryUpdate {
            importance: Some(crate::types::Importance::new(0.8_f64)),
            ..Default::default()
        };
        assert!(store.update(&id, &update).await.unwrap());

        let updated = store.get(&id, None).await.unwrap().unwrap();
        assert!((updated.importance.value() - 0.8).abs() < f64::EPSILON, "importance should be updated to 0.8");
    }

    #[tokio::test]
    async fn migration_adds_wave1_columns_to_legacy_db() {
        // Simulate a legacy database without the wave 1 columns
        SqliteStore::register_extension().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();

        // Create old schema without wave 1 columns
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                tags          TEXT NOT NULL,
                provenance    TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                expires_at    TEXT,
                has_embedding INTEGER NOT NULL DEFAULT 0,
                embedding_revision INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();

        // Run wave 1 migrations
        migrate_memories_add_memory_type(&conn).unwrap();
        migrate_memories_add_importance(&conn).unwrap();
        migrate_memories_align_impression_tracking(&conn).unwrap();

        // Verify columns exist by inserting and reading
        conn.execute(
            "INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, has_embedding, memory_type, importance, impression_count)
             VALUES ('test-id', 'content', '[]', '{}', '{\"type\":\"public\"}', '2025-06-15T00:00:00+00:00', 0, 'episodic', 0.7, 5)",
            [],
        )
        .unwrap();

        let (mt, imp, ac): (String, f64, i64) = conn
            .query_row("SELECT memory_type, importance, impression_count FROM memories WHERE id = 'test-id'", [], |row| {
                Ok((row.get(0).unwrap(), row.get(1).unwrap(), row.get(2).unwrap()))
            })
            .unwrap();
        assert_eq!(mt, "episodic");
        assert!((imp - 0.7).abs() < f64::EPSILON);
        assert_eq!(ac, 5);

        // Running migrations again should be idempotent
        migrate_memories_add_memory_type(&conn).unwrap();
        migrate_memories_add_importance(&conn).unwrap();
        migrate_memories_align_impression_tracking(&conn).unwrap();
    }

    #[tokio::test]
    async fn fresh_schema_creates_superseded_by_index() {
        let store = SqliteStore::in_memory().unwrap();
        let conn = store.inner.conn.lock();
        assert!(sqlite_index_exists(&conn, "idx_memories_superseded_by"));
    }

    #[tokio::test]
    async fn migration_ensures_superseded_by_index_when_column_already_exists() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                tags          TEXT NOT NULL,
                provenance    TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                expires_at    TEXT,
                has_embedding INTEGER NOT NULL DEFAULT 0,
                superseded_by TEXT
            );",
        )
        .unwrap();

        assert!(!sqlite_index_exists(&conn, "idx_memories_superseded_by"));
        migrate_memories_add_superseded_by(&conn).unwrap();
        assert!(sqlite_index_exists(&conn, "idx_memories_superseded_by"));
    }

    #[tokio::test]
    async fn record_search_impression_empty_ids_is_noop() {
        let store = SqliteStore::in_memory().unwrap();
        // Should not fail or panic
        store.record_search_impression(&[]).await.unwrap();
    }

    // -- Wave 2: Supersession tracking tests --

    #[tokio::test]
    async fn store_with_supersession_marks_old_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let mem_a = make_memory("project uses React 17", &["tech"], base_time());
        let id_a = store.store(&mem_a, None).await.unwrap();

        let mem_b = make_memory("project uses React 19", &["tech"], base_time());
        let id_b = store.store_with_supersession(&mem_b, None, &id_a).await.unwrap();

        // A should have superseded_by = B
        let a = store.get(&id_a, None).await.unwrap().unwrap();
        assert_eq!(a.superseded_by, Some(id_b));

        // B should not be superseded
        let b = store.get(&id_b, None).await.unwrap().unwrap();
        assert!(b.superseded_by.is_none());
    }

    #[tokio::test]
    async fn superseded_memory_hidden_from_search_by_default() {
        let store = SqliteStore::in_memory().unwrap();
        let mem_a = make_memory("old fact", &["info"], base_time());
        let id_a = store.store(&mem_a, None).await.unwrap();

        let mem_b = make_memory("new fact", &["info"], base_time());
        store.store_with_supersession(&mem_b, None, &id_a).await.unwrap();

        // Default list (include_superseded=false) should not return A
        let results = store.list(MemoryFilter::default(), QueryContext::default()).await.unwrap();
        assert!(!results.iter().any(|m| m.id == id_a), "superseded memory should be hidden");
    }

    #[tokio::test]
    async fn superseded_memory_visible_with_include_superseded() {
        let store = SqliteStore::in_memory().unwrap();
        let mem_a = make_memory("old fact", &["info"], base_time());
        let id_a = store.store(&mem_a, None).await.unwrap();

        let mem_b = make_memory("new fact", &["info"], base_time());
        store.store_with_supersession(&mem_b, None, &id_a).await.unwrap();

        // With include_superseded=true, A should be visible
        let filter = MemoryFilter {
            include_superseded: Some(true),
            ..Default::default()
        };
        let results = store.list(filter, QueryContext::default()).await.unwrap();
        assert!(results.iter().any(|m| m.id == id_a), "superseded memory should be visible when include_superseded=true");
    }

    #[tokio::test]
    async fn get_always_returns_superseded_memory() {
        let store = SqliteStore::in_memory().unwrap();
        let mem_a = make_memory("old fact", &["info"], base_time());
        let id_a = store.store(&mem_a, None).await.unwrap();

        let mem_b = make_memory("new fact", &["info"], base_time());
        store.store_with_supersession(&mem_b, None, &id_a).await.unwrap();

        // get should always return even superseded memories
        let result = store.get(&id_a, None).await.unwrap();
        assert!(result.is_some(), "get should always return superseded memories");
        assert_eq!(result.unwrap().content, "old fact");
    }

    #[tokio::test]
    async fn supersedes_nonexistent_id_returns_error() {
        let store = SqliteStore::in_memory().unwrap();
        let mem = make_memory("new fact", &["info"], base_time());
        let fake_id: MemoryId = "01H00000000000000000000000".parse().unwrap();
        let result = store.store_with_supersession(&mem, None, &fake_id).await;
        assert!(result.is_err(), "should error when superseded memory doesn't exist");
    }

    #[tokio::test]
    async fn batch_store_with_supersession_works() {
        let store = SqliteStore::in_memory().unwrap();

        // Create a memory to supersede
        let old_mem = make_memory("old info", &["tech"], base_time());
        let old_id = store.store(&old_mem, None).await.unwrap();

        let new_mem = MemoryWithEmbedding {
            memory: make_memory("new info", &["tech"], base_time()),
            embedding: None,
        };
        let no_supersede_mem = MemoryWithEmbedding {
            memory: make_memory("unrelated", &["other"], base_time()),
            embedding: None,
        };

        let ids = store.store_batch_with_supersession(&[new_mem, no_supersede_mem], &[Some(old_id), None]).await.unwrap();

        assert_eq!(ids.len(), 2);

        // old memory should be superseded
        let old = store.get(&old_id, None).await.unwrap().unwrap();
        assert_eq!(old.superseded_by, Some(ids[0]));
    }

    #[tokio::test]
    async fn record_search_impression_uses_injected_clock() {
        let fixed_time = Utc.with_ymd_and_hms(2030, 3, 15, 12, 0, 0).single().unwrap();
        let clock = Arc::new(MockClock::pinned(fixed_time));
        let dyn_clock: Arc<dyn Clock> = clock;
        let store = SqliteStore::in_memory_with_clock(dyn_clock).unwrap();

        let mem = make_memory("clock test", &[], base_time());
        let id = store.store(&mem, None).await.unwrap();

        store.record_search_impression(&[id]).await.unwrap();

        let after = store.get(&id, None).await.unwrap().unwrap();
        assert_eq!(after.impression_count, 1_u64, "access count should increment");
        #[expect(clippy::expect_used, reason = "test assertion: value must be present")]
        let accessed_at = after.last_impressed_at.expect("last_impressed_at should be set");
        assert_eq!(accessed_at, fixed_time, "last_impressed_at should match the mock clock time, not wall clock");
    }

    #[tokio::test]
    async fn scope_registry_persists_across_reopen() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let path = temp.path().to_owned();
        {
            let store = SqliteStore::open(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
            store
                .register_scope(ScopeDefinition {
                    scope_key: "gearboxlogic/localhold".into(),
                    display_name: "LocalHold".into(),
                    description: Some("project scope".into()),
                    aliases: vec!["recall".into()],
                    matchers: vec!["/workspace/localhold".into()],
                    parent: Some("gbl".into()),
                    related: vec!["gearboxlogic/example-agent".into()],
                })
                .await
                .unwrap();
        }

        let reopened = SqliteStore::open(&path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
        let scopes = reopened.list_scopes().await.unwrap();
        assert_eq!(scopes.len(), 1_usize);
        assert_eq!(scopes[0].scope_key, "gearboxlogic/localhold");
        assert_eq!(scopes[0].aliases, vec!["recall"]);
        assert_eq!(scopes[0].parent.as_deref(), Some("gbl"));
    }
}
