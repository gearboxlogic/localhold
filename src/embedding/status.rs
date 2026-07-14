//! Side-effect-conscious embedding profile, provider, and rebuild status.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension as _};
use serde::Serialize;
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row as _};
use sqlx_postgres::{PgPool, PgPoolOptions};

use super::{EmbeddingProvider as _, OpenAiEmbedding, factory::active_embedding_profile};
use crate::{
    clock::{Clock, SystemClock},
    config::{Config, DatabaseBackend, EmbeddingConfig, EmbeddingHealthCheck},
    error::EmbeddingError,
    store::{EmbeddingProfile, SqliteStore},
};

/// Machine-readable embedding status report schema version.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Exit code used when embedding configuration and storage are ready.
pub const EXIT_HEALTHY: i32 = 0;
/// Exit code used while initialization or embedding work remains.
pub const EXIT_DEGRADED: i32 = 2;
/// Exit code used when storage is inconsistent or reindexing is required.
pub const EXIT_FAILED: i32 = 1;

/// Aggregate severity for an embedding status report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EmbeddingStatusLevel {
    /// Embeddings are disabled or ready for the configured mode.
    Healthy,
    /// Initialization, rebuild work, or provider recovery remains.
    Degraded,
    /// Storage cannot safely use the configured embedding profile.
    Failed,
}

impl std::fmt::Display for EmbeddingStatusLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => f.write_str("healthy"),
            Self::Degraded => f.write_str("degraded"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// Durable embedding lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EmbeddingState {
    /// The noop provider is selected; vector retrieval is inactive.
    Disabled,
    /// The configured database has not been initialized yet.
    NotInitialized,
    /// The vector space is empty and will be stamped on normal startup.
    Ready,
    /// The stored profile matches, but memories still need vectors.
    Rebuilding,
    /// Every memory currently has a stored vector.
    Complete,
    /// Stored vector identity differs from the configured vector space.
    ReindexRequired,
    /// Embedding flags, mappings, and vector rows disagree.
    Inconsistent,
    /// Embedding storage could not be inspected safely.
    Unavailable,
}

impl std::fmt::Display for EmbeddingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("disabled"),
            Self::NotInitialized => f.write_str("not_initialized"),
            Self::Ready => f.write_str("ready"),
            Self::Rebuilding => f.write_str("rebuilding"),
            Self::Complete => f.write_str("complete"),
            Self::ReindexRequired => f.write_str("reindex_required"),
            Self::Inconsistent => f.write_str("inconsistent"),
            Self::Unavailable => f.write_str("unavailable"),
        }
    }
}

/// Result of checking the configured embedding endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EmbeddingProviderHealth {
    /// No vector provider is configured.
    Disabled,
    /// Provider probing is disabled by configuration.
    CheckDisabled,
    /// The configured model passed its health probe.
    Healthy,
    /// The endpoint is reachable but currently rate limited.
    RateLimited,
    /// The configured provider could not be constructed or probed successfully.
    Unavailable,
}

impl std::fmt::Display for EmbeddingProviderHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("disabled"),
            Self::CheckDisabled => f.write_str("check_disabled"),
            Self::Healthy => f.write_str("healthy"),
            Self::RateLimited => f.write_str("rate_limited"),
            Self::Unavailable => f.write_str("unavailable"),
        }
    }
}

/// Embedding coverage and consistency counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct EmbeddingCounts {
    /// Total memory rows.
    pub total_memories: u64,
    /// Memories marked as having a current vector.
    pub embedded_memories: u64,
    /// Memories awaiting a current vector.
    pub pending_memories: u64,
    /// Pending memories currently carrying a claim token.
    pub claimed_memories: u64,
    /// Memories mapped to vector rows.
    pub mapped_memories: u64,
    /// Stored vector rows.
    pub vector_rows: u64,
    /// Memories marked embedded without a corresponding mapping/vector.
    pub missing_vectors: u64,
    /// Mappings/vectors attached to memories marked unembedded or absent.
    pub unexpected_vectors: u64,
}

impl EmbeddingCounts {
    const fn is_consistent(self) -> bool {
        self.missing_vectors == 0
            && self.unexpected_vectors == 0
            && self.embedded_memories == self.mapped_memories
            && self.mapped_memories == self.vector_rows
            && self.total_memories == self.embedded_memories.saturating_add(self.pending_memories)
    }
}

/// Stable operator-facing embedding status document.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct EmbeddingStatusReport {
    /// Report contract version.
    pub schema_version: u32,
    /// Aggregate status.
    pub status: EmbeddingStatusLevel,
    /// Process exit code corresponding to `status`.
    pub exit_code: i32,
    /// Configured storage backend.
    pub backend: String,
    /// Durable embedding lifecycle state.
    pub state: EmbeddingState,
    /// Configured provider health.
    pub provider_health: EmbeddingProviderHealth,
    /// Secret-free configured vector-space identity.
    pub configured_profile: Option<EmbeddingProfile>,
    /// Secret-free identity recorded in the database.
    pub stored_profile: Option<EmbeddingProfile>,
    /// Dimensions declared by the physical vector table, when present.
    pub stored_dimensions: Option<usize>,
    /// Embedding coverage and relational consistency counters.
    pub counts: EmbeddingCounts,
    /// Human-readable result without credentials or memory content.
    pub summary: String,
}

impl EmbeddingStatusReport {
    /// Serialize the report as pretty JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the report cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Render a concise human-readable report.
    #[must_use]
    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;

        let mut output = format!("LocalHold embeddings: {} ({})\n", self.status, self.state);
        let _written = writeln!(output, "Backend: {}", single_line(&self.backend));
        let _written = writeln!(output, "Provider health: {}", self.provider_health);
        let _written = writeln!(output, "Configured profile: {}", render_profile(self.configured_profile.as_ref()));
        let _written = writeln!(output, "Stored profile: {}", render_profile(self.stored_profile.as_ref()));
        let _written = writeln!(
            output,
            "Memories: total {}, embedded {}, pending {}, claimed {}, mapped {}, vectors {}",
            self.counts.total_memories,
            self.counts.embedded_memories,
            self.counts.pending_memories,
            self.counts.claimed_memories,
            self.counts.mapped_memories,
            self.counts.vector_rows
        );
        let _written = writeln!(output, "Summary: {}", single_line(&self.summary));
        output
    }
}

#[derive(Debug)]
struct StorageSnapshot {
    stored_profile: Option<EmbeddingProfile>,
    stored_dimensions: Option<usize>,
    counts: EmbeddingCounts,
}

#[derive(Debug)]
enum StorageObservation {
    Ready(StorageSnapshot),
    NotInitialized,
    Unavailable,
}

type StatusResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Inspect embedding configuration, provider health, profile identity, and rebuild progress.
#[must_use]
pub async fn inspect(config: &Config) -> EmbeddingStatusReport {
    inspect_with_clock_inner(config, Arc::new(SystemClock::new())).await
}

/// Inspect embeddings with all provider and storage deadlines driven by `clock`.
#[cfg(any(test, feature = "testing"))]
pub async fn inspect_with_clock(config: &Config, clock: Arc<dyn Clock>) -> EmbeddingStatusReport {
    inspect_with_clock_inner(config, clock).await
}

async fn inspect_with_clock_inner(config: &Config, clock: Arc<dyn Clock>) -> EmbeddingStatusReport {
    let provider_health = probe_provider(config, Arc::clone(&clock)).await;
    let observation = inspect_storage(config, clock.as_ref()).await;
    build_report(config, provider_health, observation)
}

fn build_report(config: &Config, provider_health: EmbeddingProviderHealth, observation: StorageObservation) -> EmbeddingStatusReport {
    let configured_profile = active_embedding_profile(&config.embedding);
    let backend = backend_name(config.database.backend).to_owned();
    let (mut status, state, stored_profile, stored_dimensions, counts, mut summary) = match observation {
        StorageObservation::NotInitialized => (
            EmbeddingStatusLevel::Degraded,
            EmbeddingState::NotInitialized,
            None,
            None,
            EmbeddingCounts::default(),
            "database embedding storage is not initialized; normal server startup can initialize it".to_owned(),
        ),
        StorageObservation::Unavailable => (
            EmbeddingStatusLevel::Failed,
            EmbeddingState::Unavailable,
            None,
            None,
            EmbeddingCounts::default(),
            "database embedding status could not be inspected safely".to_owned(),
        ),
        StorageObservation::Ready(snapshot) => {
            let (status, state, summary) = classify_snapshot(configured_profile.as_ref(), &snapshot);
            (status, state, snapshot.stored_profile, snapshot.stored_dimensions, snapshot.counts, summary)
        }
    };

    if provider_health == EmbeddingProviderHealth::Unavailable && status != EmbeddingStatusLevel::Failed {
        status = EmbeddingStatusLevel::Degraded;
        summary.push_str("; the configured embedding provider is currently unavailable");
    }

    EmbeddingStatusReport {
        schema_version: REPORT_SCHEMA_VERSION,
        status,
        exit_code: exit_code(status),
        backend,
        state,
        provider_health,
        configured_profile,
        stored_profile,
        stored_dimensions,
        counts,
        summary,
    }
}

fn classify_snapshot(configured: Option<&EmbeddingProfile>, snapshot: &StorageSnapshot) -> (EmbeddingStatusLevel, EmbeddingState, String) {
    let Some(configured) = configured else {
        return (
            EmbeddingStatusLevel::Healthy,
            EmbeddingState::Disabled,
            "noop embedding provider is selected; stored vectors are not used for retrieval".into(),
        );
    };
    if !snapshot.counts.is_consistent() {
        return (
            EmbeddingStatusLevel::Failed,
            EmbeddingState::Inconsistent,
            "embedding flags, mappings, or vector rows are inconsistent; run hold doctor before attempting recovery".into(),
        );
    }
    if !profiles_compatible(configured, snapshot.stored_profile.as_ref(), snapshot.counts.vector_rows)
        || snapshot.stored_dimensions.is_some_and(|dimensions| dimensions != configured.dimensions)
        || (snapshot.counts.vector_rows > 0 && snapshot.stored_dimensions.is_none())
    {
        return (
            EmbeddingStatusLevel::Failed,
            EmbeddingState::ReindexRequired,
            "stored vectors do not match the configured embedding profile; back up the database and run `hold embeddings reindex --yes`".into(),
        );
    }
    if snapshot.stored_profile.is_none() {
        return (
            EmbeddingStatusLevel::Degraded,
            EmbeddingState::Ready,
            "vector storage is empty and compatible; normal server startup will record the configured profile".into(),
        );
    }
    if snapshot.counts.pending_memories > 0 {
        return (
            EmbeddingStatusLevel::Degraded,
            EmbeddingState::Rebuilding,
            "the stored profile is compatible and pending memories will be embedded by the running server".into(),
        );
    }
    (
        EmbeddingStatusLevel::Healthy,
        EmbeddingState::Complete,
        "the configured profile matches storage and every memory has a current vector".into(),
    )
}

pub(crate) fn profiles_compatible(expected: &EmbeddingProfile, stored: Option<&EmbeddingProfile>, vector_count: u64) -> bool {
    stored.map_or(vector_count == 0, |stored| stored == expected)
}

pub(crate) async fn probe_provider(config: &Config, clock: Arc<dyn Clock>) -> EmbeddingProviderHealth {
    match &config.embedding {
        EmbeddingConfig::Noop { .. } => EmbeddingProviderHealth::Disabled,
        EmbeddingConfig::OpenAiCompatible { dimensions, openai_compatible } => {
            if openai_compatible.health_check == EmbeddingHealthCheck::Disabled {
                return EmbeddingProviderHealth::CheckDisabled;
            }
            let timeout = Duration::from_secs(config.limits.embedding_timeout_secs);
            let Ok(provider) = OpenAiEmbedding::new_with_clock(openai_compatible, *dimensions, timeout, clock) else {
                return EmbeddingProviderHealth::Unavailable;
            };
            match provider.health_check().await {
                Ok(()) => EmbeddingProviderHealth::Healthy,
                Err(EmbeddingError::RateLimited { .. }) => EmbeddingProviderHealth::RateLimited,
                Err(_error) => EmbeddingProviderHealth::Unavailable,
            }
        }
    }
}

async fn inspect_storage(config: &Config, clock: &dyn Clock) -> StorageObservation {
    match config.database.backend {
        DatabaseBackend::Sqlite => inspect_sqlite_storage(config.database.sqlite_path()).await,
        DatabaseBackend::Postgres => crate::clock::timeout(clock, Duration::from_secs(20), inspect_postgres_storage(config))
            .await
            .unwrap_or(Ok(StorageObservation::Unavailable))
            .unwrap_or(StorageObservation::Unavailable),
    }
}

async fn inspect_sqlite_storage(path: &Path) -> StorageObservation {
    if !path.exists() {
        return StorageObservation::NotInitialized;
    }
    if sqlite_wal_state_requires_shm_creation(path) {
        return StorageObservation::Unavailable;
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || inspect_sqlite_storage_blocking(&path))
        .await
        .unwrap_or(Ok(StorageObservation::Unavailable))
        .unwrap_or(StorageObservation::Unavailable)
}

fn inspect_sqlite_storage_blocking(path: &Path) -> StatusResult<StorageObservation> {
    SqliteStore::register_extension()?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags)?;
    if !sqlite_table_exists(&connection, "memories")? {
        return Ok(StorageObservation::NotInitialized);
    }
    let (total, embedded, pending, claimed): (i64, i64, i64, i64) = connection.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN has_embedding = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN has_embedding = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN has_embedding = 0 AND embedding_claim_token IS NOT NULL THEN 1 ELSE 0 END), 0)
         FROM memories",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let map_exists = sqlite_table_exists(&connection, "memory_embedding_map")?;
    let vectors_exist = sqlite_table_exists(&connection, "memory_embeddings")?;
    let profile_exists = sqlite_table_exists(&connection, "embedding_profile")?;
    let mapped = if map_exists {
        connection.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get::<_, i64>(0))?
    } else {
        0
    };
    let vector_rows = if vectors_exist {
        connection.query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| row.get::<_, i64>(0))?
    } else {
        0
    };
    let (missing, unexpected) = inspect_sqlite_consistency(&connection, map_exists, vectors_exist, embedded, vector_rows)?;
    let stored_profile = if profile_exists { read_sqlite_profile(&connection)? } else { None };
    let stored_dimensions = crate::store::existing_embedding_dimensions(&connection)?;
    Ok(StorageObservation::Ready(StorageSnapshot {
        stored_profile,
        stored_dimensions,
        counts: EmbeddingCounts {
            total_memories: count_to_u64(total)?,
            embedded_memories: count_to_u64(embedded)?,
            pending_memories: count_to_u64(pending)?,
            claimed_memories: count_to_u64(claimed)?,
            mapped_memories: count_to_u64(mapped)?,
            vector_rows: count_to_u64(vector_rows)?,
            missing_vectors: count_to_u64(missing)?,
            unexpected_vectors: count_to_u64(unexpected)?,
        },
    }))
}

fn inspect_sqlite_consistency(connection: &Connection, map_exists: bool, vectors_exist: bool, embedded: i64, vector_rows: i64) -> Result<(i64, i64), rusqlite::Error> {
    let missing = if map_exists && vectors_exist {
        connection.query_row(
            "SELECT COUNT(*)
             FROM memories AS memory
             LEFT JOIN memory_embedding_map AS map ON map.memory_id = memory.id
             LEFT JOIN memory_embeddings AS embedding ON embedding.rowid = map.vec_rowid
             WHERE memory.has_embedding = 1 AND (map.memory_id IS NULL OR embedding.rowid IS NULL)",
            [],
            |row| row.get::<_, i64>(0),
        )?
    } else {
        embedded
    };
    let unexpected_mappings = if map_exists {
        connection.query_row(
            "SELECT COUNT(*)
             FROM memory_embedding_map AS map
             LEFT JOIN memories AS memory ON memory.id = map.memory_id
             WHERE memory.id IS NULL OR memory.has_embedding = 0",
            [],
            |row| row.get::<_, i64>(0),
        )?
    } else {
        0
    };
    let unmapped_vectors = if map_exists && vectors_exist {
        connection.query_row(
            "SELECT COUNT(*)
             FROM memory_embeddings AS embedding
             LEFT JOIN memory_embedding_map AS map ON map.vec_rowid = embedding.rowid
             WHERE map.vec_rowid IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?
    } else if vectors_exist {
        vector_rows
    } else {
        0
    };
    let unexpected = unexpected_mappings.saturating_add(unmapped_vectors);
    Ok((missing, unexpected))
}

async fn inspect_postgres_storage(config: &Config) -> StatusResult<StorageObservation> {
    let pool = PgPoolOptions::new().max_connections(1).connect(&config.database.postgres.url).await?;
    let memories_exist: bool = query_scalar("SELECT to_regclass('memories') IS NOT NULL").fetch_one(&pool).await?;
    if !memories_exist {
        return Ok(StorageObservation::NotInitialized);
    }
    let row = query(
        "SELECT COUNT(*) AS total,
                COUNT(*) FILTER (WHERE has_embedding) AS embedded,
                COUNT(*) FILTER (WHERE NOT has_embedding) AS pending,
                COUNT(*) FILTER (WHERE NOT has_embedding AND embedding_claim_token IS NOT NULL) AS claimed
         FROM memories",
    )
    .fetch_one(&pool)
    .await?;
    let vector_table_exists: bool = query_scalar("SELECT to_regclass('memory_embeddings') IS NOT NULL").fetch_one(&pool).await?;
    let profile_table_exists: bool = query_scalar("SELECT to_regclass('embedding_profile') IS NOT NULL").fetch_one(&pool).await?;
    let (vector_rows, missing, unexpected) = if vector_table_exists {
        (
            query_scalar::<_, i64>("SELECT COUNT(*) FROM memory_embeddings").fetch_one(&pool).await?,
            query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM memories AS memory LEFT JOIN memory_embeddings AS embedding ON embedding.memory_id = memory.id WHERE memory.has_embedding AND embedding.memory_id IS NULL",
            )
            .fetch_one(&pool)
            .await?,
            query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM memory_embeddings AS embedding LEFT JOIN memories AS memory ON memory.id = embedding.memory_id WHERE memory.id IS NULL OR NOT memory.has_embedding",
            )
            .fetch_one(&pool)
            .await?,
        )
    } else {
        (0, row.try_get::<i64, _>("embedded")?, 0)
    };
    let stored_profile = if profile_table_exists { read_postgres_profile(&pool).await? } else { None };
    let stored_dimensions = if vector_table_exists { read_postgres_dimensions(&pool).await? } else { None };
    Ok(StorageObservation::Ready(StorageSnapshot {
        stored_profile,
        stored_dimensions,
        counts: EmbeddingCounts {
            total_memories: count_to_u64(row.try_get("total")?)?,
            embedded_memories: count_to_u64(row.try_get("embedded")?)?,
            pending_memories: count_to_u64(row.try_get("pending")?)?,
            claimed_memories: count_to_u64(row.try_get("claimed")?)?,
            mapped_memories: count_to_u64(vector_rows)?,
            vector_rows: count_to_u64(vector_rows)?,
            missing_vectors: count_to_u64(missing)?,
            unexpected_vectors: count_to_u64(unexpected)?,
        },
    }))
}

fn sqlite_table_exists(connection: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)", [table], |row| row.get(0))
}

fn read_sqlite_profile(connection: &Connection) -> StatusResult<Option<EmbeddingProfile>> {
    let stored = connection
        .query_row("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
        })
        .optional()?;
    stored
        .map(|(provider, endpoint, model, dimensions)| {
            Ok::<EmbeddingProfile, Box<dyn std::error::Error + Send + Sync>>(EmbeddingProfile {
                provider,
                endpoint,
                model,
                dimensions: usize::try_from(dimensions)?,
            })
        })
        .transpose()
}

async fn read_postgres_profile(pool: &PgPool) -> StatusResult<Option<EmbeddingProfile>> {
    let row = query("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1")
        .fetch_optional(pool)
        .await?;
    row.map(|row| {
        Ok(EmbeddingProfile {
            provider: row.try_get("provider")?,
            endpoint: row.try_get("endpoint")?,
            model: row.try_get("model")?,
            dimensions: usize::try_from(row.try_get::<i64, _>("dimensions")?)?,
        })
    })
    .transpose()
}

async fn read_postgres_dimensions(pool: &PgPool) -> StatusResult<Option<usize>> {
    let vector_type: Option<String> = query_scalar(
        "SELECT format_type(attribute.atttypid, attribute.atttypmod) FROM pg_attribute AS attribute WHERE attribute.attrelid = to_regclass('memory_embeddings') AND attribute.attname = 'embedding' AND NOT attribute.attisdropped",
    )
    .fetch_optional(pool)
    .await?;
    Ok(vector_type.as_deref().and_then(postgres_vector_dimensions))
}

fn postgres_vector_dimensions(vector_type: &str) -> Option<usize> {
    vector_type.strip_prefix("vector(")?.strip_suffix(')')?.parse().ok()
}

fn count_to_u64(count: i64) -> StatusResult<u64> {
    Ok(u64::try_from(count)?)
}

const fn exit_code(status: EmbeddingStatusLevel) -> i32 {
    match status {
        EmbeddingStatusLevel::Healthy => EXIT_HEALTHY,
        EmbeddingStatusLevel::Degraded => EXIT_DEGRADED,
        EmbeddingStatusLevel::Failed => EXIT_FAILED,
    }
}

const fn backend_name(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Sqlite => "sqlite",
        DatabaseBackend::Postgres => "postgres",
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn sqlite_wal_state_requires_shm_creation(path: &Path) -> bool {
    sqlite_sidecar_path(path, "-wal").exists() && !sqlite_sidecar_path(path, "-shm").exists()
}

fn render_profile(profile: Option<&EmbeddingProfile>) -> String {
    profile.map_or_else(
        || "none".into(),
        |profile| {
            format!(
                "{} model '{}' at '{}' with {} dimensions",
                single_line(&profile.provider),
                single_line(&profile.model),
                single_line(&profile.endpoint),
                profile.dimensions
            )
        },
    )
}

fn single_line(value: &str) -> String {
    value.chars().map(|character| if character.is_control() { '\u{fffd}' } else { character }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(model: &str) -> EmbeddingProfile {
        EmbeddingProfile::openai_compatible("http://127.0.0.1:8000/v1", model, 3)
    }

    fn snapshot(stored_profile: Option<EmbeddingProfile>, embedded: u64, pending: u64) -> StorageSnapshot {
        StorageSnapshot {
            stored_profile,
            stored_dimensions: Some(3),
            counts: EmbeddingCounts {
                total_memories: embedded.saturating_add(pending),
                embedded_memories: embedded,
                pending_memories: pending,
                mapped_memories: embedded,
                vector_rows: embedded,
                ..EmbeddingCounts::default()
            },
        }
    }

    #[test]
    fn classification_distinguishes_ready_rebuilding_complete_and_reindex() {
        let configured = profile("current");
        assert_eq!(classify_snapshot(Some(&configured), &snapshot(None, 0, 1)).1, EmbeddingState::Ready);
        assert_eq!(
            classify_snapshot(Some(&configured), &snapshot(Some(configured.clone()), 1, 1)).1,
            EmbeddingState::Rebuilding
        );
        assert_eq!(classify_snapshot(Some(&configured), &snapshot(Some(configured.clone()), 2, 0)).1, EmbeddingState::Complete);
        assert_eq!(
            classify_snapshot(Some(&configured), &snapshot(Some(profile("old")), 2, 0)).1,
            EmbeddingState::ReindexRequired
        );
    }

    #[test]
    fn relational_disagreement_is_inconsistent_before_profile_classification() {
        let configured = profile("current");
        let mut snapshot = snapshot(Some(configured.clone()), 1, 0);
        snapshot.counts.vector_rows = 0;
        assert_eq!(classify_snapshot(Some(&configured), &snapshot).1, EmbeddingState::Inconsistent);
    }

    #[test]
    fn report_json_never_contains_an_api_key_field() {
        let report = EmbeddingStatusReport {
            schema_version: REPORT_SCHEMA_VERSION,
            status: EmbeddingStatusLevel::Healthy,
            exit_code: EXIT_HEALTHY,
            backend: "sqlite".into(),
            state: EmbeddingState::Complete,
            provider_health: EmbeddingProviderHealth::Healthy,
            configured_profile: Some(profile("current")),
            stored_profile: Some(profile("current")),
            stored_dimensions: Some(3),
            counts: EmbeddingCounts::default(),
            summary: "ready".into(),
        };
        let json = report.to_json().unwrap();
        assert!(!json.contains("api_key"));
    }
}
