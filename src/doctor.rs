//! Side-effect-conscious installation and runtime diagnostics.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension as _};
use serde::Serialize;
use sqlx_core::query_scalar::query_scalar;
use sqlx_postgres::{PgPool, PgPoolOptions};

#[cfg(test)]
use crate::store::PostgresStore;
use crate::{
    config::{Config, DatabaseBackend, EmbeddingConfig},
    embedding::status::EmbeddingProviderHealth,
    store::{
        EmbeddingProfile, SqliteStore,
        postgres_migrations::{MigrationMetadataState, read_migration_metadata_state},
    },
};

/// Machine-readable doctor report schema version.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Exit code used when every diagnostic is healthy.
pub const EXIT_HEALTHY: i32 = 0;
/// Exit code used when `LocalHold` can run with reduced readiness or needs action.
pub const EXIT_DEGRADED: i32 = 2;
/// Exit code used when configuration or a required runtime dependency failed.
pub const EXIT_FAILED: i32 = 1;

/// Doctor command options.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct DoctorOptions {
    /// Permit first-use reranker artifact downloads while probing inference.
    pub allow_downloads: bool,
}

/// Health classification for an individual diagnostic and the overall report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiagnosticStatus {
    /// The check passed or the capability is intentionally disabled.
    Healthy,
    /// The check needs operator attention but does not prove startup failure.
    Degraded,
    /// A required condition failed.
    Failed,
}

impl std::fmt::Display for DiagnosticStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => f.write_str("healthy"),
            Self::Degraded => f.write_str("degraded"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// One stable diagnostic result.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DiagnosticCheck {
    /// Stable machine-readable check identifier.
    pub name: String,
    /// Check classification.
    pub status: DiagnosticStatus,
    /// Human-readable result without secrets or memory content.
    pub summary: String,
}

/// Compile-time capabilities of the running binary.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct BuildInfo {
    /// `LocalHold` package version.
    pub version: String,
    /// Compiled target architecture.
    pub architecture: String,
    /// Compiled target operating system.
    pub operating_system: String,
    /// Whether CPU reranking is compiled in.
    pub reranker: bool,
    /// Whether CUDA reranking is compiled in.
    pub reranker_cuda: bool,
    /// Supported MCP transports.
    pub transports: Vec<String>,
}

/// Stable doctor output document.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DoctorReport {
    /// Report contract version.
    pub schema_version: u32,
    /// Aggregate status.
    pub status: DiagnosticStatus,
    /// Process exit code corresponding to `status`.
    pub exit_code: i32,
    /// Binary build and capability data.
    pub build: BuildInfo,
    /// Config file used, or `None` when defaults and environment are active.
    pub config_source: Option<String>,
    /// Local data path, or `None` for non-filesystem storage backends.
    pub data_path: Option<String>,
    /// Ordered diagnostic results.
    pub checks: Vec<DiagnosticCheck>,
}

impl DoctorReport {
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

    /// Render concise human-readable diagnostics.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut output = format!("LocalHold doctor: {}\n", self.status);
        for check in &self.checks {
            use std::fmt::Write as _;
            let _written = writeln!(output, "[{}] {}: {}", check.status, single_line(&check.name), single_line(&check.summary));
        }
        output
    }
}

fn single_line(value: &str) -> String {
    value.chars().map(|character| if character.is_control() { '\u{fffd}' } else { character }).collect()
}

/// Run all diagnostics without creating a database or downloading model files
/// unless `allow_downloads` is set.
pub async fn run(options: DoctorOptions) -> DoctorReport {
    Box::pin(run_with_clock(options, std::sync::Arc::new(crate::clock::SystemClock::new()))).await
}

/// Run diagnostics with timeouts driven by an injected clock.
#[cfg(any(test, feature = "testing"))]
pub async fn run_with_clock(options: DoctorOptions, clock: std::sync::Arc<dyn crate::clock::Clock>) -> DoctorReport {
    Box::pin(run_with_clock_inner(options, clock)).await
}

#[cfg(not(any(test, feature = "testing")))]
async fn run_with_clock(options: DoctorOptions, clock: std::sync::Arc<dyn crate::clock::Clock>) -> DoctorReport {
    Box::pin(run_with_clock_inner(options, clock)).await
}

async fn run_with_clock_inner(options: DoctorOptions, clock: std::sync::Arc<dyn crate::clock::Clock>) -> DoctorReport {
    let build = build_info();
    let (config, source) = match Config::load_with_source() {
        Ok(loaded) => loaded,
        Err(_error) => {
            let checks = vec![
                check("build", DiagnosticStatus::Healthy, build_summary(&build)),
                check(
                    "configuration",
                    DiagnosticStatus::Failed,
                    "configuration could not be loaded or validated; no secret-bearing parser context was emitted",
                ),
            ];
            return finalize(build, None, None, checks);
        }
    };

    let mut checks = vec![check("build", DiagnosticStatus::Healthy, build_summary(&build))];
    checks.push(config_check(source.as_deref()));
    checks.push(filesystem_check(&config));
    checks.push(Box::pin(storage_check(&config, clock.as_ref())).await);
    checks.push(embedding_check(&config, std::sync::Arc::clone(&clock)).await);
    checks.push(reranker_check(&config, options, clock).await);
    let data_path = match config.database.backend {
        DatabaseBackend::Sqlite => Some(config.database.sqlite_path().display().to_string()),
        DatabaseBackend::Postgres => None,
    };
    finalize(build, source.map(|path| path.display().to_string()), data_path, checks)
}

fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        architecture: std::env::consts::ARCH.into(),
        operating_system: std::env::consts::OS.into(),
        reranker: cfg!(feature = "reranker"),
        reranker_cuda: cfg!(feature = "reranker-cuda"),
        transports: vec!["stdio".into(), "http".into()],
    }
}

fn build_summary(build: &BuildInfo) -> String {
    let reranker = if build.reranker_cuda {
        "cpu,cuda"
    } else if build.reranker {
        "cpu"
    } else {
        "none"
    };
    format!(
        "version {}, target {}-{}, transports stdio,http, reranker providers {reranker}",
        build.version, build.architecture, build.operating_system
    )
}

fn config_check(source: Option<&Path>) -> DiagnosticCheck {
    let Some(path) = source else {
        return check("configuration", DiagnosticStatus::Healthy, "defaults and LOCALHOLD_* environment overrides validated");
    };
    match std::fs::File::open(path) {
        Ok(_file) => check("configuration", DiagnosticStatus::Healthy, format!("validated readable config at {}", path.display())),
        Err(_error) => check(
            "configuration",
            DiagnosticStatus::Failed,
            format!("configured file is no longer readable at {}", path.display()),
        ),
    }
}

fn filesystem_check(config: &Config) -> DiagnosticCheck {
    match config.database.backend {
        DatabaseBackend::Postgres => check(
            "filesystem",
            DiagnosticStatus::Healthy,
            "PostgreSQL backend has no local database path; config readability was checked separately",
        ),
        DatabaseBackend::Sqlite => {
            let path = config.database.sqlite_path();
            let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
            if path.exists() {
                if parent.metadata().is_ok_and(|metadata| metadata.permissions().readonly()) {
                    return check(
                        "filesystem",
                        DiagnosticStatus::Failed,
                        format!("SQLite parent directory is marked read-only at {}", parent.display()),
                    );
                }
                if let Some(sidecar) = unwritable_existing_sqlite_sidecar(path) {
                    return check(
                        "filesystem",
                        DiagnosticStatus::Failed,
                        format!("existing SQLite sidecar is not both readable and writable at {}", sidecar.display()),
                    );
                }
                return match std::fs::OpenOptions::new().read(true).write(true).open(path) {
                    Ok(_file) if sqlite_sidecars_exist(path) => check(
                        "filesystem",
                        DiagnosticStatus::Healthy,
                        format!("SQLite data file and existing WAL sidecars are readable and writable at {}", path.display()),
                    ),
                    Ok(_file) => check(
                        "filesystem",
                        DiagnosticStatus::Degraded,
                        format!(
                            "SQLite data file is readable and writable at {}, but WAL sidecar creation was not tested by writing",
                            path.display()
                        ),
                    ),
                    Err(_error) => check(
                        "filesystem",
                        DiagnosticStatus::Failed,
                        format!("SQLite data file is not both readable and writable at {}", path.display()),
                    ),
                };
            }
            if !parent.exists() {
                return check(
                    "filesystem",
                    DiagnosticStatus::Degraded,
                    format!("SQLite parent directory does not exist at {}; doctor did not create it", parent.display()),
                );
            }
            if !parent.is_dir() {
                return check(
                    "filesystem",
                    DiagnosticStatus::Failed,
                    format!("SQLite parent path is not a directory at {}", parent.display()),
                );
            }
            if parent.metadata().is_ok_and(|metadata| metadata.permissions().readonly()) {
                return check(
                    "filesystem",
                    DiagnosticStatus::Failed,
                    format!("SQLite parent directory is marked read-only at {}", parent.display()),
                );
            }
            check(
                "filesystem",
                DiagnosticStatus::Degraded,
                format!("SQLite parent exists at {}, but create permission was not tested by writing", parent.display()),
            )
        }
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn unwritable_existing_sqlite_sidecar(path: &Path) -> Option<PathBuf> {
    ["-wal", "-shm"]
        .into_iter()
        .map(|suffix| sqlite_sidecar_path(path, suffix))
        .find(|sidecar| sidecar.exists() && std::fs::OpenOptions::new().read(true).write(true).open(sidecar).is_err())
}

fn sqlite_sidecars_exist(path: &Path) -> bool {
    ["-wal", "-shm"].into_iter().map(|suffix| sqlite_sidecar_path(path, suffix)).all(|sidecar| sidecar.exists())
}

fn sqlite_wal_state_requires_shm_creation(path: &Path) -> bool {
    sqlite_sidecar_path(path, "-wal").exists() && !sqlite_sidecar_path(path, "-shm").exists()
}

async fn storage_check(config: &Config, clock: &dyn crate::clock::Clock) -> DiagnosticCheck {
    match config.database.backend {
        DatabaseBackend::Sqlite => {
            let config = config.clone();
            let path = config.database.sqlite_path().to_path_buf();
            if sqlite_wal_state_requires_shm_creation(&path) {
                return check(
                    "storage",
                    DiagnosticStatus::Degraded,
                    "SQLite WAL exists without shared-memory state; doctor did not open it because that could create a sidecar",
                );
            }
            tokio::task::spawn_blocking(move || sqlite_check(&config, &path)).await.unwrap_or_else(|_join_error| {
                check(
                    "storage",
                    DiagnosticStatus::Failed,
                    "SQLite diagnostic worker terminated before completing compatibility checks",
                )
            })
        }
        DatabaseBackend::Postgres => Box::pin(crate::clock::timeout(clock, Duration::from_secs(20), postgres_check(config, clock)))
            .await
            .unwrap_or_else(|_elapsed| check("storage", DiagnosticStatus::Failed, "PostgreSQL readiness checks exceeded the 20 second deadline")),
    }
}

#[expect(clippy::too_many_lines, reason = "SQLite readiness is kept linear so each read-only compatibility gate is explicit")]
fn sqlite_check(config: &Config, path: &Path) -> DiagnosticCheck {
    if !path.exists() {
        return check(
            "storage",
            DiagnosticStatus::Degraded,
            format!("SQLite database does not exist at {}; doctor did not create it", path.display()),
        );
    }
    if sqlite_wal_state_requires_shm_creation(path) {
        return check(
            "storage",
            DiagnosticStatus::Degraded,
            "SQLite WAL exists without shared-memory state; doctor did not open it because that could create a sidecar",
        );
    }
    if SqliteStore::register_extension().is_err() {
        return check("storage", DiagnosticStatus::Failed, "SQLite vector extension could not be registered");
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = match Connection::open_with_flags(path, flags) {
        Ok(connection) => connection,
        Err(_error) => return check("storage", DiagnosticStatus::Failed, "SQLite database is not readable"),
    };
    let integrity = connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0));
    if !matches!(integrity.as_deref(), Ok("ok")) {
        return check("storage", DiagnosticStatus::Failed, "SQLite quick_check did not report a healthy database");
    }
    let published_metadata_upgrade = match crate::store::validate_published_v2_metadata(&connection) {
        Ok(published_metadata_upgrade) => published_metadata_upgrade,
        Err(_error) => {
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "SQLite published-beta metadata is incompatible and startup will refuse to migrate it",
            );
        }
    };
    let present_schema = if published_metadata_upgrade {
        crate::store::migration::validate_present_sqlite_schema_for_published_upgrade(&connection)
    } else {
        crate::store::migration::validate_present_sqlite_schema(&connection)
    };
    if present_schema.is_err() {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "an existing SQLite schema object is incompatible and cannot be repaired by a normal startup migration",
        );
    }
    let has_memories = connection
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories')", [], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap_or(false);
    if !has_memories {
        return check(
            "storage",
            DiagnosticStatus::Degraded,
            "SQLite file is readable but LocalHold schema bootstrap is pending; doctor did not create it",
        );
    }
    if crate::store::migration::validate_sqlite_foreign_key_integrity(&connection).is_err() {
        return check("storage", DiagnosticStatus::Failed, "SQLite foreign-key integrity check failed");
    }
    let has_embedding_map = table_readable(&connection, "memory_embedding_map");
    let has_embedding_vectors = table_readable(&connection, "memory_embeddings");
    if has_embedding_map && has_embedding_vectors {
        if crate::store::migration::validate_embedding_map_integrity(&connection).is_err() {
            return check("storage", DiagnosticStatus::Failed, "SQLite embedding-map integrity check failed");
        }
    } else if has_embedding_map {
        if sqlite_table_has_rows(&connection, "memory_embedding_map") != Some(false) {
            return check("storage", DiagnosticStatus::Failed, "SQLite embedding map contains rows but its vector table is absent");
        }
    } else if has_embedding_vectors && sqlite_table_has_rows(&connection, "memory_embeddings") != Some(false) {
        return check("storage", DiagnosticStatus::Failed, "SQLite vector table contains rows but its embedding map is absent");
    }
    match crate::store::existing_embedding_dimensions(&connection) {
        Ok(Some(dimensions)) if dimensions == config.embedding.dimensions() => {}
        Ok(Some(_dimensions)) => {
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "SQLite vector dimensions do not match the configured embedding dimensions",
            );
        }
        Ok(None) => {}
        Err(_) => return check("storage", DiagnosticStatus::Failed, "SQLite vector dimensions could not be verified"),
    }
    if let Some(profile) = crate::embedding::factory::active_embedding_profile(&config.embedding) {
        let profile_compatible = if table_readable(&connection, "embedding_profile") {
            sqlite_embedding_profile_compatible(&connection, &profile)
        } else if table_readable(&connection, "memory_embedding_map") {
            sqlite_vector_count(&connection).is_some_and(|count| count == 0)
        } else {
            true
        };
        if !profile_compatible {
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "SQLite stored embeddings are incompatible with the configured embedding profile",
            );
        }
    }
    let embedding_map_fk_current = match sqlite_embedding_map_fk_status(&connection) {
        Ok(SqliteEmbeddingMapFkStatus::Current) => true,
        Ok(SqliteEmbeddingMapFkStatus::Absent) => false,
        Ok(SqliteEmbeddingMapFkStatus::Incompatible) | Err(_) => {
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "SQLite embedding-map foreign key is incompatible with startup cascade requirements",
            );
        }
    };
    let current_schema = connection
        .prepare("SELECT id, embedding_claimed_at, embedding_claim_token, confidence FROM memories LIMIT 0")
        .and_then(|mut statement| statement.query([]).map(|_rows| ()))
        .is_ok()
        && table_readable(&connection, "memory_metadata")
        && table_readable(&connection, "memory_tombstones")
        && table_readable(&connection, "scope_registry")
        && table_readable(&connection, "memory_audit_log")
        && table_readable(&connection, "memory_entities")
        && table_readable(&connection, "memory_embedding_map")
        && table_readable(&connection, "memory_embeddings")
        && table_readable(&connection, "embedding_profile")
        && table_readable(&connection, "memory_fts")
        && trigger_readable(&connection, "trg_memory_embedding_map_delete")
        && trigger_readable(&connection, "trg_memory_clear_superseded_by")
        && trigger_readable(&connection, "trg_memory_fts_insert")
        && trigger_readable(&connection, "trg_memory_fts_update")
        && trigger_readable(&connection, "trg_memory_fts_delete")
        && sqlite_indexes_current(&connection)
        && embedding_map_fk_current;
    if !current_schema {
        return check(
            "storage",
            DiagnosticStatus::Degraded,
            if published_metadata_upgrade {
                "SQLite contains supported published-beta metadata; normal startup will validate, back up, and migrate it"
            } else {
                "SQLite is readable and internally consistent but requires a normal startup migration after backup"
            },
        );
    }
    if crate::store::migration::validate_sqlite_source_schema(&connection, config.embedding.dimensions()).is_err() {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "SQLite schema shape, keys, or relational integrity is incompatible with this LocalHold binary",
        );
    }
    check(
        "storage",
        DiagnosticStatus::Healthy,
        format!("SQLite is reachable, current, and passed startup compatibility checks at {}", path.display()),
    )
}

fn table_readable(connection: &Connection, table: &str) -> bool {
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)", [table], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap_or(false)
}

fn sqlite_table_has_rows(connection: &Connection, table: &'static str) -> Option<bool> {
    connection.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table})"), [], |row| row.get(0)).ok()
}

fn trigger_readable(connection: &Connection, trigger: &str) -> bool {
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1)", [trigger], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap_or(false)
}

fn sqlite_indexes_current(connection: &Connection) -> bool {
    const REQUIRED_INDEXES: [&str; 17] = [
        "idx_memories_created_at",
        "idx_memories_source_agent",
        "idx_memories_source_conversation",
        "idx_memories_origin_conversation",
        "idx_memories_effective_origin_conversation",
        "idx_memories_access_type",
        "idx_memories_expires_at",
        "idx_memories_has_embedding",
        "idx_memories_embedding_claim",
        "idx_memories_memory_type",
        "idx_memories_superseded_by",
        "idx_memory_entities_entity",
        "idx_memory_entities_entity_type",
        "idx_audit_log_memory_id",
        "idx_audit_log_timestamp",
        "idx_memory_metadata_scope_key",
        "idx_memory_tombstones_deleted_at",
    ];
    REQUIRED_INDEXES.iter().all(|index| {
        connection
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)", [*index], |row| {
                row.get::<_, bool>(0)
            })
            .unwrap_or(false)
    })
}

fn sqlite_embedding_profile_compatible(connection: &Connection, expected: &EmbeddingProfile) -> bool {
    let stored = connection
        .query_row("SELECT provider, endpoint, model, dimensions FROM embedding_profile WHERE singleton = 1", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?))
        })
        .optional();
    match stored {
        Ok(Some((provider, endpoint, model, dimensions))) => {
            provider == expected.provider && endpoint == expected.endpoint && model == expected.model && usize::try_from(dimensions).ok() == Some(expected.dimensions)
        }
        Ok(None) if !table_readable(connection, "memory_embedding_map") => true,
        Ok(None) => sqlite_vector_count(connection).is_some_and(|count| count == 0),
        Err(_) => false,
    }
}

async fn postgres_indexes_compatible(pool: &PgPool, allow_absent: bool) -> Result<bool, sqlx_core::error::Error> {
    query_scalar(
        "SELECT COALESCE(bool_and(
            ($1 AND indexes.oid IS NULL) OR COALESCE(
                indexes.relkind = 'i'
                AND index_data.indrelid = to_regclass(format('%I.%I', current_schema(), required.table_name))
                AND index_data.indisvalid
                AND index_data.indisready
                AND index_data.indnkeyatts = required.key_count
                AND (required.expected_keys IS NULL OR (
                    SELECT string_agg(regexp_replace(lower(pg_get_indexdef(indexes.oid, key_number, TRUE)), '[[:space:]]', '', 'g'), ',' ORDER BY key_number)
                    FROM generate_series(1, required.key_count) AS key_number
                ) = required.expected_keys)
                AND position(required.definition_fragment IN lower(pg_get_indexdef(indexes.oid))) > 0
                AND ((required.predicate IS NULL AND index_data.indpred IS NULL)
                     OR regexp_replace(lower(pg_get_expr(index_data.indpred, index_data.indrelid, TRUE)), '[()[:space:]]', '', 'g') = required.predicate)
            , FALSE)
        ), FALSE)
        FROM (VALUES
            ('idx_memories_created_at', 'memories', 1, 'created_at', 'created_at desc', NULL),
            ('idx_memories_expires_at', 'memories', 1, 'expires_at', 'expires_at', 'expires_atisnotnull'),
            ('idx_memories_has_embedding', 'memories', 1, 'has_embedding', 'has_embedding', NULL),
            ('idx_memories_memory_type', 'memories', 1, 'memory_type', 'memory_type', NULL),
            ('idx_memories_superseded_by', 'memories', 1, 'superseded_by', 'superseded_by', 'superseded_byisnotnull'),
            ('idx_memories_tags_gin', 'memories', 1, 'tags', 'using gin (tags)', NULL),
            ('idx_memories_source_agent', 'memories', 1, '(provenance->>''source_agent''::text)', 'source_agent', NULL),
            ('idx_memories_source_conversation', 'memories', 1, '(provenance->>''source_conversation''::text)', 'source_conversation', NULL),
            ('idx_memories_origin_conversation', 'memories', 1, '(provenance->>''origin_conversation''::text)', 'origin_conversation', NULL),
            ('idx_memories_effective_origin_conversation', 'memories', 1, 'coalesce(provenance->>''origin_conversation''::text,provenance->>''source_conversation''::text)', 'coalesce', NULL),
            ('idx_memories_access_type', 'memories', 1, '(access_policy->>''type''::text)', 'access_policy', NULL),
            ('idx_memories_content_fts', 'memories', 1, 'to_tsvector(''simple''::regconfig,content)', 'to_tsvector', NULL),
            ('idx_memories_embedding_claim', 'memories', 4, 'has_embedding,embedding_claimed_at,created_at,id', 'embedding_claimed_at', 'has_embedding=false'),
            ('idx_memory_entities_entity', 'memory_entities', 1, 'entity', '(entity)', NULL),
            ('idx_memory_entities_entity_type', 'memory_entities', 1, 'entity_type', 'entity_type', NULL),
            ('idx_audit_log_memory_id', 'memory_audit_log', 1, 'memory_id', 'memory_id', NULL),
            ('idx_audit_log_timestamp', 'memory_audit_log', 1, '\"timestamp\"', '\"timestamp\" desc', NULL),
            ('idx_memory_tombstones_deleted_at', 'memory_tombstones', 1, 'deleted_at', 'deleted_at desc', NULL),
            ('idx_memory_metadata_scope_key', 'memory_metadata', 1, 'scope_key', 'scope_key', NULL)
        ) AS required(name, table_name, key_count, expected_keys, definition_fragment, predicate)
        LEFT JOIN pg_class AS indexes ON indexes.oid = to_regclass(format('%I.%I', current_schema(), required.name))
        LEFT JOIN pg_index AS index_data ON index_data.indexrelid = indexes.oid",
    )
    .bind(allow_absent)
    .fetch_one(pool)
    .await
}

fn sqlite_vector_count(connection: &Connection) -> Option<i64> {
    connection.query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get::<_, i64>(0)).ok()
}

enum SqliteEmbeddingMapFkStatus {
    Current,
    Absent,
    Incompatible,
}

fn sqlite_embedding_map_fk_status(connection: &Connection) -> Result<SqliteEmbeddingMapFkStatus, rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA foreign_key_list(memory_embedding_map)")?;
    let mut rows = statement.query([])?;
    let mut saw_foreign_key = false;
    let mut canonical = false;
    while let Some(row) = rows.next()? {
        if saw_foreign_key {
            return Ok(SqliteEmbeddingMapFkStatus::Incompatible);
        }
        saw_foreign_key = true;
        let table: String = row.get(2)?;
        let from_column: String = row.get(3)?;
        if table == "memories" && from_column == "memory_id" {
            let to_column: String = row.get(4)?;
            let on_delete: String = row.get(6)?;
            canonical = to_column == "id" && on_delete.eq_ignore_ascii_case("CASCADE");
        }
    }
    Ok(match (saw_foreign_key, canonical) {
        (false, _) => SqliteEmbeddingMapFkStatus::Absent,
        (true, true) => SqliteEmbeddingMapFkStatus::Current,
        (true, false) => SqliteEmbeddingMapFkStatus::Incompatible,
    })
}

#[expect(clippy::too_many_lines, reason = "PostgreSQL readiness is kept linear so each read-only compatibility gate is explicit")]
async fn postgres_check(config: &Config, clock: &dyn crate::clock::Clock) -> DiagnosticCheck {
    let connect = PgPoolOptions::new().max_connections(1).connect(&config.database.postgres.url);
    let Ok(Ok(pool)) = crate::clock::timeout(clock, Duration::from_secs(10), connect).await else {
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL is unreachable or rejected the configured connection");
    };
    let vector_installed: Result<bool, _> = query_scalar("SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')").fetch_one(&pool).await;
    let vector_installed = match vector_installed {
        Ok(installed) => installed,
        Err(_error) => {
            pool.close().await;
            return check("storage", DiagnosticStatus::Failed, "PostgreSQL extension installation state could not be verified");
        }
    };
    if vector_installed {
        let vector_visible: Result<bool, _> = query_scalar("SELECT to_regtype('vector') IS NOT NULL").fetch_one(&pool).await;
        if !matches!(vector_visible, Ok(true)) {
            pool.close().await;
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "PostgreSQL pgvector is installed but its vector type is not visible on the configured search path",
            );
        }
    } else {
        let vector_available: Result<bool, _> = query_scalar("SELECT EXISTS(SELECT 1 FROM pg_available_extensions WHERE name = 'vector')")
            .fetch_one(&pool)
            .await;
        if !matches!(vector_available, Ok(true)) {
            pool.close().await;
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "PostgreSQL pgvector extension is neither installed nor available for startup to install",
            );
        }
    }
    let required_tables: Result<i64, _> = query_scalar(
        "SELECT COUNT(*) FROM (VALUES ('memories'), ('localhold_migrations'), ('memory_embeddings'), ('embedding_profile'), ('memory_audit_log'), ('memory_entities'), ('memory_metadata'), ('memory_tombstones'), ('scope_registry')) AS required(name) WHERE ($1 OR required.name <> 'localhold_migrations') AND to_regclass(CASE WHEN $2 THEN format('%I.%I', current_schema(), required.name) ELSE required.name END) IS NOT NULL",
    )
    .bind(config.database.postgres.auto_migrate)
    .bind(config.database.postgres.auto_migrate)
    .fetch_one(&pool)
    .await;
    let schema_table_count = match required_tables {
        Ok(count) => count,
        Err(_error) => {
            pool.close().await;
            return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema inspection failed");
        }
    };
    let can_inspect_owned_audit_sequence: Result<bool, _> = query_scalar(
        "SELECT COALESCE(bool_and(has_sequence_privilege(sequence.oid, 'SELECT')), TRUE)
            FROM pg_attribute AS attribute
            JOIN pg_attrdef AS definition ON definition.adrelid = attribute.attrelid AND definition.adnum = attribute.attnum
            JOIN pg_depend AS default_dependency ON default_dependency.classid = 'pg_attrdef'::regclass AND default_dependency.objid = definition.oid AND default_dependency.refclassid = 'pg_class'::regclass
            JOIN pg_class AS sequence ON sequence.oid = default_dependency.refobjid AND sequence.relkind = 'S'
            JOIN pg_depend AS ownership ON ownership.classid = 'pg_class'::regclass AND ownership.objid = sequence.oid AND ownership.refclassid = 'pg_class'::regclass AND ownership.refobjid = attribute.attrelid AND ownership.refobjsubid = attribute.attnum AND ownership.deptype IN ('a', 'i')
            WHERE attribute.attrelid = to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_audit_log') ELSE 'memory_audit_log' END)
              AND attribute.attname = 'id'
              AND NOT attribute.attisdropped",
    )
    .bind(config.database.postgres.auto_migrate)
    .fetch_one(&pool)
    .await;
    let can_inspect_audit_rows: Result<bool, _> = query_scalar(
        "SELECT COALESCE(has_table_privilege(to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_audit_log') ELSE 'memory_audit_log' END), 'SELECT'), TRUE)",
    )
    .bind(config.database.postgres.auto_migrate)
    .fetch_one(&pool)
    .await;
    if !matches!(can_inspect_owned_audit_sequence, Ok(true)) || !matches!(can_inspect_audit_rows, Ok(true)) {
        pool.close().await;
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL diagnostic schema inspection privileges are incomplete");
    }
    if let Some(profile) = crate::embedding::factory::active_embedding_profile(&config.embedding) {
        match postgres_embedding_profile_compatible_if_present(&pool, &profile, config.database.postgres.auto_migrate).await {
            Ok(true) => {}
            Ok(false) => {
                pool.close().await;
                return check(
                    "storage",
                    DiagnosticStatus::Failed,
                    "PostgreSQL stored embeddings are incompatible with the configured embedding profile",
                );
            }
            Err(_error) => {
                pool.close().await;
                return check("storage", DiagnosticStatus::Failed, "PostgreSQL embedding-profile compatibility could not be verified");
            }
        }
    }
    let published_metadata_upgrade = if config.database.postgres.auto_migrate {
        match crate::store::validate_published_v2_metadata_upgrade(&pool).await {
            Ok(published_metadata_upgrade) => published_metadata_upgrade,
            Err(_error) => {
                pool.close().await;
                return check(
                    "storage",
                    DiagnosticStatus::Failed,
                    "PostgreSQL published-beta metadata is incompatible and startup will refuse to migrate it",
                );
            }
        }
    } else {
        false
    };
    let present_schema = if published_metadata_upgrade {
        crate::store::migration::validate_present_postgres_schema_for_published_upgrade(
            &pool,
            config.embedding.dimensions(),
            config.database.postgres.auto_migrate,
            config.database.postgres.auto_migrate,
        )
        .await
    } else {
        crate::store::migration::validate_present_postgres_schema(
            &pool,
            config.embedding.dimensions(),
            config.database.postgres.auto_migrate,
            config.database.postgres.auto_migrate,
        )
        .await
    };
    if present_schema.is_err() {
        pool.close().await;
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "an existing PostgreSQL schema object is incompatible and cannot be repaired by a normal startup migration",
        );
    }
    if published_metadata_upgrade {
        match read_migration_metadata_state(&pool).await {
            Ok(MigrationMetadataState::Absent | MigrationMetadataState::Repairable | MigrationMetadataState::Current) => {}
            Ok(MigrationMetadataState::Newer) => {
                pool.close().await;
                return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema is newer than this LocalHold binary supports");
            }
            Ok(MigrationMetadataState::Incompatible) => {
                pool.close().await;
                return check(
                    "storage",
                    DiagnosticStatus::Failed,
                    "PostgreSQL migration metadata conflicts with the expected migration identities",
                );
            }
            Err(_error) => {
                pool.close().await;
                return check("storage", DiagnosticStatus::Failed, "PostgreSQL migration metadata could not be inspected");
            }
        }
    }
    let required_table_count = if config.database.postgres.auto_migrate { 9_i64 } else { 8_i64 };
    if schema_table_count > 0_i64 && schema_table_count < required_table_count {
        let present_indexes_compatible = postgres_indexes_compatible(&pool, true).await;
        let present_constraints_compatible: Result<bool, _> = query_scalar(
            "SELECT
                (to_regclass(format('%I.%I', current_schema(), 'memories')) IS NULL OR EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND confrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND contype = 'f' AND convalidated AND confdeltype = 'n' AND conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND attname = 'superseded_by')]::smallint[] AND confkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND attname = 'id')]::smallint[]))
                AND (to_regclass(format('%I.%I', current_schema(), 'memory_entities')) IS NULL OR EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid = to_regclass(format('%I.%I', current_schema(), 'memory_entities')) AND confrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND contype = 'f' AND convalidated AND confdeltype = 'c' AND conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memory_entities')) AND attname = 'memory_id')]::smallint[] AND confkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND attname = 'id')]::smallint[]))
                AND (to_regclass(format('%I.%I', current_schema(), 'memory_embeddings')) IS NULL OR EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid = to_regclass(format('%I.%I', current_schema(), 'memory_embeddings')) AND confrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND contype = 'f' AND convalidated AND confdeltype = 'c' AND conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memory_embeddings')) AND attname = 'memory_id')]::smallint[] AND confkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND attname = 'id')]::smallint[]))
                AND (to_regclass(format('%I.%I', current_schema(), 'memory_metadata')) IS NULL OR EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid = to_regclass(format('%I.%I', current_schema(), 'memory_metadata')) AND confrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND contype = 'f' AND convalidated AND confdeltype = 'c' AND conkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memory_metadata')) AND attname = 'memory_id')]::smallint[] AND confkey = ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid = to_regclass(format('%I.%I', current_schema(), 'memories')) AND attname = 'id')]::smallint[]))",
        )
        .fetch_one(&pool)
        .await;
        let can_repair: Result<bool, _> = if vector_installed {
            query_scalar(
                "SELECT has_schema_privilege(current_schema(), 'CREATE') AND (SELECT COALESCE(bool_and(pg_has_role(current_user, tableowner, 'MEMBER')), TRUE) FROM pg_tables WHERE schemaname = current_schema() AND tablename IN ('memories', 'localhold_migrations', 'memory_embeddings', 'embedding_profile', 'memory_audit_log', 'memory_entities', 'memory_v2_metadata', 'memory_metadata', 'memory_tombstones', 'scope_registry'))",
            )
            .fetch_one(&pool)
            .await
        } else {
            query_scalar(
                "SELECT has_schema_privilege(current_schema(), 'CREATE') AND has_database_privilege(current_database(), 'CREATE') AND (SELECT COALESCE(bool_and(pg_has_role(current_user, tableowner, 'MEMBER')), TRUE) FROM pg_tables WHERE schemaname = current_schema() AND tablename IN ('memories', 'localhold_migrations', 'memory_embeddings', 'embedding_profile', 'memory_audit_log', 'memory_entities', 'memory_v2_metadata', 'memory_metadata', 'memory_tombstones', 'scope_registry'))",
            )
            .fetch_one(&pool)
            .await
        };
        pool.close().await;
        if config.database.postgres.auto_migrate
            && matches!(present_indexes_compatible, Ok(true))
            && matches!(present_constraints_compatible, Ok(true))
            && matches!(can_repair, Ok(true))
        {
            return check(
                "storage",
                DiagnosticStatus::Degraded,
                if published_metadata_upgrade {
                    "PostgreSQL contains supported published-beta metadata; normal startup will validate and migrate it"
                } else {
                    "PostgreSQL has a compatible partial managed schema that normal startup can complete"
                },
            );
        }
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL has a partial managed schema; restore it from backup or repair it explicitly before startup",
        );
    }
    if schema_table_count == 0_i64 {
        let can_create: Result<bool, _> = if vector_installed {
            query_scalar("SELECT has_schema_privilege(current_schema(), 'CREATE')").fetch_one(&pool).await
        } else {
            query_scalar("SELECT has_schema_privilege(current_schema(), 'CREATE') AND has_database_privilege(current_database(), 'CREATE')")
                .fetch_one(&pool)
                .await
        };
        pool.close().await;
        if config.database.postgres.auto_migrate && !matches!(can_create, Ok(true)) {
            return check(
                "storage",
                DiagnosticStatus::Failed,
                "PostgreSQL schema bootstrap is pending but the configured role lacks schema creation privilege",
            );
        }
        let status = if config.database.postgres.auto_migrate {
            DiagnosticStatus::Degraded
        } else {
            DiagnosticStatus::Failed
        };
        return check(
            "storage",
            status,
            if config.database.postgres.auto_migrate {
                if vector_installed {
                    "PostgreSQL is reachable; schema bootstrap is pending and doctor did not create it"
                } else {
                    "PostgreSQL is reachable; schema bootstrap and pgvector installation are pending, and extension install authority remains unverified"
                }
            } else {
                "PostgreSQL is reachable but the LocalHold schema is absent and auto-migration is disabled"
            },
        );
    }
    if crate::store::migration::validate_existing_postgres_schema(
        &pool,
        config.embedding.dimensions(),
        config.database.postgres.auto_migrate,
        config.database.postgres.auto_migrate,
    )
    .await
    .is_err()
    {
        pool.close().await;
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL schema shape, column types, or keys are incompatible with this LocalHold binary",
        );
    }
    if config.database.postgres.auto_migrate {
        let startup_privileges: Result<bool, _> =
            query_scalar("SELECT has_table_privilege('localhold_migrations', 'SELECT') AND has_table_privilege('localhold_migrations', 'INSERT')")
                .fetch_one(&pool)
                .await;
        if !matches!(startup_privileges, Ok(true)) {
            pool.close().await;
            return check("storage", DiagnosticStatus::Failed, "PostgreSQL migration metadata is not writable by the configured role");
        }
    }
    let runtime_privileges: Result<bool, _> = query_scalar(
        "SELECT
            has_table_privilege('memories', 'SELECT') AND has_table_privilege('memories', 'INSERT') AND has_table_privilege('memories', 'UPDATE') AND has_table_privilege('memories', 'DELETE')
            AND has_table_privilege('memory_entities', 'SELECT') AND has_table_privilege('memory_entities', 'INSERT') AND has_table_privilege('memory_entities', 'DELETE')
            AND has_table_privilege('memory_embeddings', 'SELECT') AND has_table_privilege('memory_embeddings', 'INSERT') AND has_table_privilege('memory_embeddings', 'UPDATE') AND has_table_privilege('memory_embeddings', 'DELETE')
            AND has_table_privilege('memory_audit_log', 'SELECT') AND has_table_privilege('memory_audit_log', 'INSERT')
            AND has_table_privilege('memory_tombstones', 'SELECT') AND has_table_privilege('memory_tombstones', 'INSERT') AND has_table_privilege('memory_tombstones', 'UPDATE')
            AND has_table_privilege('scope_registry', 'SELECT') AND has_table_privilege('scope_registry', 'INSERT') AND has_table_privilege('scope_registry', 'UPDATE')
            AND has_table_privilege('memory_metadata', 'SELECT') AND has_table_privilege('memory_metadata', 'INSERT') AND has_table_privilege('memory_metadata', 'UPDATE') AND has_table_privilege('memory_metadata', 'DELETE')
            AND has_table_privilege('embedding_profile', 'SELECT') AND has_table_privilege('embedding_profile', 'INSERT') AND has_table_privilege('embedding_profile', 'UPDATE')
            AND has_sequence_privilege(pg_get_serial_sequence('memory_audit_log', 'id'), 'USAGE')",
    )
    .fetch_one(&pool)
    .await;
    if !matches!(runtime_privileges, Ok(true)) {
        pool.close().await;
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL runtime table or sequence privileges are incomplete");
    }
    let current_schema_only = config.database.postgres.auto_migrate;
    let relationships_current = crate::store::migration::validate_postgres_runtime_relationships(&pool, current_schema_only).await.is_ok();
    let relationships_compatible = if relationships_current {
        true
    } else if config.database.postgres.auto_migrate {
        crate::store::migration::validate_postgres_runtime_relationships_before_migration(&pool, current_schema_only)
            .await
            .is_ok()
    } else {
        false
    };
    let indexes_compatible = crate::store::migration::postgres_runtime_indexes_compatible(&pool, current_schema_only, config.database.postgres.auto_migrate).await;
    if !relationships_compatible || !matches!(indexes_compatible, Ok(true)) {
        pool.close().await;
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL relational constraints or indexes are incompatible with startup requirements",
        );
    }
    let migration_metadata = if config.database.postgres.auto_migrate {
        read_migration_metadata_state(&pool).await
    } else {
        Ok(MigrationMetadataState::Current)
    };
    let retained_history_fk_exists: Result<bool, _> = query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid IN (to_regclass('memory_audit_log'), to_regclass('memory_tombstones')) AND contype = 'f' AND confrelid = to_regclass('memories'))",
    )
    .fetch_one(&pool)
    .await;
    let current_columns: Result<i64, _> = query_scalar(
        "SELECT COUNT(*) FROM pg_attribute WHERE attrelid = to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memories') ELSE 'memories' END) AND attname IN ('embedding_claimed_at', 'embedding_claim_token', 'confidence') AND NOT attisdropped",
    )
    .bind(config.database.postgres.auto_migrate)
    .fetch_one(&pool)
    .await;
    let indexes_current = crate::store::migration::postgres_runtime_indexes_compatible(&pool, config.database.postgres.auto_migrate, false).await;
    let owns_managed_tables: Result<bool, _> = query_scalar(
        "SELECT COALESCE(bool_and(pg_has_role(current_user, tableowner, 'MEMBER')), FALSE) FROM pg_tables WHERE schemaname = current_schema() AND tablename IN ('memories', 'localhold_migrations', 'memory_embeddings', 'embedding_profile', 'memory_audit_log', 'memory_entities', 'memory_metadata', 'memory_tombstones', 'scope_registry')",
    )
    .fetch_one(&pool)
    .await;
    let vector_type: Result<Option<String>, _> = query_scalar(
        "SELECT format_type(attribute.atttypid, attribute.atttypmod) FROM pg_attribute AS attribute WHERE attribute.attrelid = to_regclass('memory_embeddings') AND attribute.attname = 'embedding' AND NOT attribute.attisdropped",
    )
    .fetch_optional(&pool)
    .await;
    let profile_compatible = if let Some(profile) = crate::embedding::factory::active_embedding_profile(&config.embedding) {
        postgres_embedding_profile_compatible(&pool, &profile).await
    } else {
        Ok(true)
    };
    pool.close().await;
    let (Ok(migration_metadata), Ok(retained_history_fk_exists), Ok(current_columns), Ok(indexes_current), Ok(vector_type), Ok(profile_compatible), Ok(owns_managed_tables)) = (
        migration_metadata,
        retained_history_fk_exists,
        current_columns,
        indexes_current,
        vector_type,
        profile_compatible,
        owns_managed_tables,
    ) else {
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema compatibility queries failed");
    };
    if migration_metadata == MigrationMetadataState::Newer {
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema is newer than this LocalHold binary supports");
    }
    if migration_metadata == MigrationMetadataState::Incompatible {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL migration metadata conflicts with the expected migration identities",
        );
    }
    if postgres_vector_dimensions(vector_type.as_deref()) != Some(config.embedding.dimensions()) {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL vector dimensions do not match the configured embedding dimensions",
        );
    }
    if !profile_compatible {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL stored embeddings are incompatible with the configured embedding profile",
        );
    }
    if config.database.postgres.auto_migrate && !owns_managed_tables {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "PostgreSQL auto-migration is enabled but the configured role does not own every managed table",
        );
    }
    if migration_metadata == MigrationMetadataState::Current && relationships_current && current_columns == 3_i64 && indexes_current && !retained_history_fk_exists {
        check("storage", DiagnosticStatus::Healthy, "PostgreSQL is reachable and the LocalHold schema is current")
    } else if config.database.postgres.auto_migrate {
        check("storage", DiagnosticStatus::Degraded, "PostgreSQL is reachable but a normal startup migration is required")
    } else {
        check("storage", DiagnosticStatus::Failed, "PostgreSQL schema is not current and auto-migration is disabled")
    }
}

fn postgres_vector_dimensions(vector_type: Option<&str>) -> Option<usize> {
    vector_type?.strip_prefix("vector(")?.strip_suffix(')')?.parse().ok()
}

async fn postgres_embedding_profile_compatible(pool: &PgPool, expected: &EmbeddingProfile) -> Result<bool, sqlx_core::error::Error> {
    let dimensions = i64::try_from(expected.dimensions).unwrap_or(i64::MAX);
    let matches: Option<bool> = query_scalar("SELECT provider = $1 AND endpoint = $2 AND model = $3 AND dimensions = $4 FROM embedding_profile WHERE singleton = 1")
        .bind(&expected.provider)
        .bind(&expected.endpoint)
        .bind(&expected.model)
        .bind(dimensions)
        .fetch_optional(pool)
        .await?;
    if let Some(matches) = matches {
        return Ok(matches);
    }
    let vector_count: i64 = query_scalar("SELECT COUNT(*) FROM memory_embeddings").fetch_one(pool).await?;
    Ok(vector_count == 0)
}

async fn postgres_embedding_profile_compatible_if_present(pool: &PgPool, expected: &EmbeddingProfile, current_schema_only: bool) -> Result<bool, sqlx_core::error::Error> {
    let profile_exists: bool =
        query_scalar("SELECT to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'embedding_profile') ELSE 'embedding_profile' END) IS NOT NULL")
            .bind(current_schema_only)
            .fetch_one(pool)
            .await?;
    if profile_exists {
        let dimensions = i64::try_from(expected.dimensions).unwrap_or(i64::MAX);
        let matches: Option<bool> = query_scalar("SELECT provider = $1 AND endpoint = $2 AND model = $3 AND dimensions = $4 FROM embedding_profile WHERE singleton = 1")
            .bind(&expected.provider)
            .bind(&expected.endpoint)
            .bind(&expected.model)
            .bind(dimensions)
            .fetch_optional(pool)
            .await?;
        if let Some(matches) = matches {
            return Ok(matches);
        }
    }
    let embeddings_exist: bool =
        query_scalar("SELECT to_regclass(CASE WHEN $1 THEN format('%I.%I', current_schema(), 'memory_embeddings') ELSE 'memory_embeddings' END) IS NOT NULL")
            .bind(current_schema_only)
            .fetch_one(pool)
            .await?;
    if !embeddings_exist {
        return Ok(true);
    }
    let vector_count: i64 = query_scalar("SELECT COUNT(*) FROM memory_embeddings").fetch_one(pool).await?;
    Ok(vector_count == 0)
}

async fn embedding_check(config: &Config, clock: std::sync::Arc<dyn crate::clock::Clock>) -> DiagnosticCheck {
    let provider_health = crate::embedding::status::probe_provider(config, clock).await;
    match (&config.embedding, provider_health) {
        (EmbeddingConfig::Noop { dimensions }, EmbeddingProviderHealth::Disabled) => check(
            "embedding",
            DiagnosticStatus::Healthy,
            format!("text-only noop provider is selected with {dimensions} schema dimensions"),
        ),
        (EmbeddingConfig::OpenAiCompatible { dimensions, .. }, EmbeddingProviderHealth::CheckDisabled) => check(
            "embedding",
            DiagnosticStatus::Healthy,
            format!("health probe is disabled by policy for the configured OpenAI-compatible model with {dimensions} dimensions"),
        ),
        (EmbeddingConfig::OpenAiCompatible { .. }, EmbeddingProviderHealth::Healthy) => {
            check("embedding", DiagnosticStatus::Healthy, "the configured OpenAI-compatible model passed its health probe")
        }
        (EmbeddingConfig::OpenAiCompatible { .. }, EmbeddingProviderHealth::RateLimited) => check(
            "embedding",
            DiagnosticStatus::Healthy,
            "the configured OpenAI-compatible provider is reachable but currently rate limited; startup treats it as available",
        ),
        (EmbeddingConfig::OpenAiCompatible { .. }, EmbeddingProviderHealth::Unavailable) => check(
            "embedding",
            DiagnosticStatus::Degraded,
            "the configured OpenAI-compatible model did not pass its health probe",
        ),
        _ => check("embedding", DiagnosticStatus::Failed, "embedding provider status did not match the configured provider"),
    }
}

async fn reranker_check(config: &Config, options: DoctorOptions, clock: std::sync::Arc<dyn crate::clock::Clock>) -> DiagnosticCheck {
    #[cfg(not(feature = "reranker"))]
    let _options = options;
    #[cfg(not(feature = "reranker"))]
    let _clock = clock;
    let reranker = &config.search.reranker;
    let compiled = crate::reranker::policy::compiled_execution_providers()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if !reranker.enabled {
        #[cfg(feature = "reranker")]
        let identity = crate::reranker::runtime::model_identity(reranker).map_or_else(
            |_error| "configured model has invalid revision or hash configuration".to_owned(),
            |identity| format!("configured {} {} model identity validated", identity.artifact, identity.precision),
        );
        #[cfg(not(feature = "reranker"))]
        let identity = "configured model identity unavailable in this build";
        return check(
            "reranker",
            DiagnosticStatus::Healthy,
            format!("disabled; {identity}; compiled providers: {}", if compiled.is_empty() { "none" } else { &compiled }),
        );
    }
    #[cfg(not(feature = "reranker"))]
    {
        let status = if reranker.required { DiagnosticStatus::Failed } else { DiagnosticStatus::Degraded };
        return check("reranker", status, "enabled but this binary was compiled without reranker support");
    }
    #[cfg(feature = "reranker")]
    {
        let identity = crate::reranker::runtime::model_identity(reranker);
        let identity_summary = identity.map_or_else(
            |_error| "configured model has invalid revision or hash configuration".to_owned(),
            |identity| format!("configured {} {} model identity validated", identity.artifact, identity.precision),
        );
        match crate::reranker::runtime::initialize_for_diagnostics_with_clock(reranker, options.allow_downloads, clock).await {
            Ok(initialized) => {
                let selected = initialized.selected_execution_provider().map_or_else(|| "none".into(), |provider| provider.to_string());
                let active = initialized.active_execution_provider().map_or_else(|| "none".into(), |provider| provider.to_string());
                check(
                    "reranker",
                    if active == "none" { DiagnosticStatus::Degraded } else { DiagnosticStatus::Healthy },
                    format!("{identity_summary}; compiled {compiled}; selected {selected}; active {active}; inference probe completed"),
                )
            }
            Err(error) => {
                let download_may_fix = !options.allow_downloads && reranker.model_path.is_empty() && matches!(error, crate::reranker::RerankerError::Unavailable);
                let provider_guidance = reranker_provider_guidance(&error);
                let status = if reranker.required && !download_may_fix {
                    DiagnosticStatus::Failed
                } else {
                    DiagnosticStatus::Degraded
                };
                check(
                    "reranker",
                    status,
                    format!(
                        "{identity_summary}; inference probe unavailable{}{}",
                        if download_may_fix {
                            "; rerun with --allow-downloads to permit first-use artifacts"
                        } else if options.allow_downloads {
                            " after downloads were allowed"
                        } else {
                            " with configured local artifacts"
                        },
                        provider_guidance,
                    ),
                )
            }
        }
    }
}

#[cfg(feature = "reranker")]
fn reranker_provider_guidance(error: &crate::reranker::RerankerError) -> String {
    if let crate::reranker::RerankerError::ProviderUnavailable(reason) = error {
        format!("; {reason}")
    } else {
        String::new()
    }
}

fn check(name: impl Into<String>, status: DiagnosticStatus, summary: impl Into<String>) -> DiagnosticCheck {
    DiagnosticCheck {
        name: name.into(),
        status,
        summary: summary.into(),
    }
}

fn finalize(build: BuildInfo, config_source: Option<String>, data_path: Option<String>, checks: Vec<DiagnosticCheck>) -> DoctorReport {
    let status = checks.iter().map(|check| check.status).max().unwrap_or(DiagnosticStatus::Healthy);
    let exit_code = match status {
        DiagnosticStatus::Healthy => EXIT_HEALTHY,
        DiagnosticStatus::Degraded => EXIT_DEGRADED,
        DiagnosticStatus::Failed => EXIT_FAILED,
    };
    DoctorReport {
        schema_version: REPORT_SCHEMA_VERSION,
        status,
        exit_code,
        build,
        config_source,
        data_path,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use sqlx_core::query::query;

    use super::*;

    #[test]
    fn aggregate_status_uses_worst_check() {
        let report = finalize(build_info(), None, None, vec![
            check("healthy", DiagnosticStatus::Healthy, "ok"),
            check("degraded", DiagnosticStatus::Degraded, "attention"),
        ]);
        assert_eq!(report.status, DiagnosticStatus::Degraded);
        assert_eq!(report.exit_code, EXIT_DEGRADED);
    }

    #[test]
    fn json_contract_has_stable_schema_and_no_ansi() {
        let report = finalize(build_info(), None, None, vec![check("build", DiagnosticStatus::Healthy, "ok")]);
        let json = report.to_json().unwrap();
        assert!(json.contains("\"schema_version\": 1"));
        assert!(!json.contains('\u{1b}'));
    }

    #[test]
    fn text_renderer_flattens_control_characters() {
        let report = finalize(build_info(), None, None, vec![check(
            "storage\n[failed] forged",
            DiagnosticStatus::Healthy,
            "path\r\n[failed] injected",
        )]);
        let rendered = report.render_text();
        assert!(!rendered.contains("\n[failed]"));
        assert!(!rendered.contains('\r'));
    }

    #[test]
    fn sqlite_doctor_accepts_only_valid_published_beta_metadata() {
        SqliteStore::register_extension().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("published-beta.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/database-upgrades/v0.1.0-beta.2-beta.3.sqlite.sql"))
            .unwrap();
        let present_schema = crate::store::migration::validate_present_sqlite_schema_for_published_upgrade(&connection);
        assert!(present_schema.is_ok(), "{present_schema:?}");
        drop(connection);

        let config = Config {
            embedding: EmbeddingConfig::Noop { dimensions: 3 },
            ..Config::default()
        };
        let valid = sqlite_check(&config, &path);
        assert_eq!(valid.status, DiagnosticStatus::Degraded);
        assert!(valid.summary.contains("published-beta"), "{}", valid.summary);

        let connection = Connection::open(&path).unwrap();
        let _added = connection
            .execute("ALTER TABLE memories ADD COLUMN record_revision TEXT NOT NULL DEFAULT 'bad'", [])
            .unwrap();
        drop(connection);
        let invalid_record_revision = sqlite_check(&config, &path);
        assert_eq!(invalid_record_revision.status, DiagnosticStatus::Failed);

        let connection = Connection::open(&path).unwrap();
        let _dropped = connection.execute("ALTER TABLE memories DROP COLUMN record_revision", []).unwrap();
        connection
            .execute_batch(
                "DROP INDEX idx_memories_created_at;
                 CREATE INDEX idx_memories_created_at ON memories(content);",
            )
            .unwrap();
        drop(connection);
        let invalid_managed_schema = sqlite_check(&config, &path);
        assert_eq!(invalid_managed_schema.status, DiagnosticStatus::Failed);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("DROP INDEX idx_memories_created_at; CREATE INDEX idx_memories_created_at ON memories(created_at DESC);")
            .unwrap();
        let _corrupted = connection.execute("UPDATE memory_v2_metadata SET schema_version = 99", []).unwrap();
        drop(connection);
        let invalid = sqlite_check(&config, &path);
        assert_eq!(invalid.status, DiagnosticStatus::Failed);
        assert!(invalid.summary.contains("incompatible"), "{}", invalid.summary);
    }

    #[tokio::test]
    #[ignore = "requires rootless Podman or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    #[expect(clippy::too_many_lines, reason = "the ordered corruption matrix keeps each fail-closed classification auditable")]
    async fn postgres_doctor_accepts_only_valid_published_beta_metadata() {
        let url = std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into());
        let schema = format!("localhold_doctor_published_{}", crate::types::MemoryId::new().to_string().to_lowercase());
        let admin = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
        let _created = query(sqlx_core::sql_str::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&admin).await.unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let postgres = crate::config::PostgresDatabaseConfig {
            url: format!("{url}{separator}options=-csearch_path%3D{schema}%2Cpublic"),
            max_connections: 2,
            migration_lock_timeout_secs: 5,
            auto_migrate: true,
        };
        let scoped = PgPoolOptions::new().max_connections(1).connect(&postgres.url).await.unwrap();
        let fixture = sqlx_core::sql_str::AssertSqlSafe(include_str!("../tests/fixtures/database-upgrades/v0.1.0-beta.2-beta.3.postgres.sql").to_owned());
        let _built = sqlx_core::raw_sql::raw_sql(fixture).execute(&scoped).await.unwrap();
        let present_schema = crate::store::migration::validate_present_postgres_schema_for_published_upgrade(&scoped, 3, true, true).await;
        assert!(present_schema.is_ok(), "{present_schema:?}");
        let config = Config {
            database: crate::config::DatabaseConfig {
                backend: DatabaseBackend::Postgres,
                postgres,
                ..crate::config::DatabaseConfig::default()
            },
            embedding: EmbeddingConfig::Noop { dimensions: 3 },
            ..Config::default()
        };
        let clock = crate::clock::SystemClock::new();

        let valid = postgres_check(&config, &clock).await;
        assert_eq!(valid.status, DiagnosticStatus::Degraded);
        assert!(valid.summary.contains("published-beta"), "{}", valid.summary);

        let _future = query("INSERT INTO localhold_migrations(version, name) VALUES (99, 'future')")
            .execute(&scoped)
            .await
            .unwrap();
        let newer_ledger = postgres_check(&config, &clock).await;
        assert_eq!(newer_ledger.status, DiagnosticStatus::Failed);
        assert!(newer_ledger.summary.contains("newer"), "{}", newer_ledger.summary);
        let _removed = query("DELETE FROM localhold_migrations WHERE version = 99").execute(&scoped).await.unwrap();

        let _wrong = query("UPDATE localhold_migrations SET name = 'wrong_identity' WHERE version = 2")
            .execute(&scoped)
            .await
            .unwrap();
        let incompatible_ledger = postgres_check(&config, &clock).await;
        assert_eq!(incompatible_ledger.status, DiagnosticStatus::Failed);
        assert!(incompatible_ledger.summary.contains("identities"), "{}", incompatible_ledger.summary);
        let _restored = query("UPDATE localhold_migrations SET name = 'audit_log_without_memory_fk' WHERE version = 2")
            .execute(&scoped)
            .await
            .unwrap();

        let _corrupted = query("ALTER TABLE memories ADD COLUMN record_revision INTEGER NOT NULL DEFAULT 0")
            .execute(&scoped)
            .await
            .unwrap();
        let invalid_record_revision = postgres_check(&config, &clock).await;
        assert_eq!(invalid_record_revision.status, DiagnosticStatus::Failed);
        let _restored = query("ALTER TABLE memories DROP COLUMN record_revision").execute(&scoped).await.unwrap();

        let _corrupted = sqlx_core::raw_sql::raw_sql(sqlx_core::sql_str::AssertSqlSafe(
            "ALTER TABLE memories ALTER COLUMN importance DROP DEFAULT;
             ALTER TABLE memories ALTER COLUMN importance TYPE TEXT USING importance::text"
                .to_owned(),
        ))
        .execute(&scoped)
        .await
        .unwrap();
        let invalid_managed_schema = postgres_check(&config, &clock).await;
        assert_eq!(invalid_managed_schema.status, DiagnosticStatus::Failed);

        let _restored = sqlx_core::raw_sql::raw_sql(sqlx_core::sql_str::AssertSqlSafe(
            "ALTER TABLE memories ALTER COLUMN importance TYPE DOUBLE PRECISION USING importance::double precision;
             ALTER TABLE memories ALTER COLUMN importance SET DEFAULT 0.5"
                .to_owned(),
        ))
        .execute(&scoped)
        .await
        .unwrap();
        let _corrupted = query("UPDATE memory_v2_metadata SET schema_version = 99").execute(&scoped).await.unwrap();
        let invalid = postgres_check(&config, &clock).await;
        assert_eq!(invalid.status, DiagnosticStatus::Failed);
        assert!(invalid.summary.contains("incompatible"), "{}", invalid.summary);

        scoped.close().await;
        let _dropped = query(sqlx_core::sql_str::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    #[tokio::test]
    #[ignore = "requires rootless Podman or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn postgres_doctor_distinguishes_repairable_and_incompatible_schema_states() {
        let url = std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into());
        let schema = format!("localhold_doctor_{}", crate::types::MemoryId::new().to_string().to_lowercase());
        let admin = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
        let _created = query(sqlx_core::sql_str::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&admin).await.unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let postgres = crate::config::PostgresDatabaseConfig {
            url: format!("{url}{separator}options=-csearch_path%3D{schema}%2Cpublic"),
            max_connections: 2,
            migration_lock_timeout_secs: 5,
            auto_migrate: true,
        };
        drop(PostgresStore::open(&postgres, 3_usize).await.unwrap());
        let scoped = PgPoolOptions::new().max_connections(1).connect(&postgres.url).await.unwrap();
        let mut config = Config::default();
        config.database.backend = DatabaseBackend::Postgres;
        config.database.postgres = postgres.clone();
        config.embedding = EmbeddingConfig::Noop { dimensions: 3 };
        let clock = crate::clock::SystemClock::new();

        let _legacy_fk = query(
            "ALTER TABLE memory_audit_log ADD CONSTRAINT legacy_audit_memory_fkey
             FOREIGN KEY (memory_id) REFERENCES memories(id)",
        )
        .execute(&scoped)
        .await
        .unwrap();
        let legacy_fk_status = postgres_check(&config, &clock).await.status;
        drop(PostgresStore::open(&postgres, 3_usize).await.unwrap());

        let _missing = query("DROP INDEX idx_memories_content_fts").execute(&scoped).await.unwrap();
        let missing_index_status = postgres_check(&config, &clock).await.status;
        drop(PostgresStore::open(&postgres, 3_usize).await.unwrap());

        let _missing = query("DROP INDEX idx_memories_content_fts").execute(&scoped).await.unwrap();
        let _wrong = query("CREATE INDEX idx_memories_content_fts ON memories(id)").execute(&scoped).await.unwrap();
        let wrong_index_status = postgres_check(&config, &clock).await.status;
        let wrong_index_startup_rejected = PostgresStore::open(&postgres, 3_usize).await.is_err();
        let _removed_wrong = query("DROP INDEX idx_memories_content_fts").execute(&scoped).await.unwrap();
        drop(PostgresStore::open(&postgres, 3_usize).await.unwrap());

        let _extra_unique = query("CREATE UNIQUE INDEX unexpected_doctor_content_unique ON memories(content)")
            .execute(&scoped)
            .await
            .unwrap();
        let unique_index_status = postgres_check(&config, &clock).await.status;
        let unique_index_startup_rejected = PostgresStore::open(&postgres, 3_usize).await.is_err();

        scoped.close().await;
        let _dropped = query(sqlx_core::sql_str::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;

        assert_eq!(legacy_fk_status, DiagnosticStatus::Degraded);
        assert_eq!(missing_index_status, DiagnosticStatus::Degraded);
        assert_eq!(wrong_index_status, DiagnosticStatus::Failed);
        assert!(wrong_index_startup_rejected);
        assert_eq!(unique_index_status, DiagnosticStatus::Failed);
        assert!(unique_index_startup_rejected);
    }

    #[tokio::test]
    #[ignore = "requires rootless Podman or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
    async fn postgres_doctor_classifies_migration_ledger_states() {
        let url = std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into());
        let schema = format!("localhold_doctor_ledger_{}", crate::types::MemoryId::new().to_string().to_lowercase());
        let admin = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
        let _created = query(sqlx_core::sql_str::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&admin).await.unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let postgres = crate::config::PostgresDatabaseConfig {
            url: format!("{url}{separator}options=-csearch_path%3D{schema}%2Cpublic"),
            max_connections: 2,
            migration_lock_timeout_secs: 5,
            auto_migrate: true,
        };
        drop(PostgresStore::open(&postgres, 3_usize).await.unwrap());
        let scoped = PgPoolOptions::new().max_connections(1).connect(&postgres.url).await.unwrap();
        let mut config = Config::default();
        config.database.backend = DatabaseBackend::Postgres;
        config.database.postgres = postgres.clone();
        config.embedding = EmbeddingConfig::Noop { dimensions: 3 };
        let clock = crate::clock::SystemClock::new();

        let _deleted = query("DELETE FROM localhold_migrations WHERE version = 2").execute(&scoped).await.unwrap();
        let missing = postgres_check(&config, &clock).await;
        drop(PostgresStore::open(&postgres, 3_usize).await.unwrap());

        let _wrong = query("UPDATE localhold_migrations SET name = 'wrong_identity' WHERE version = 2")
            .execute(&scoped)
            .await
            .unwrap();
        let wrong = postgres_check(&config, &clock).await;
        let _restored = query("UPDATE localhold_migrations SET name = 'audit_log_without_memory_fk' WHERE version = 2")
            .execute(&scoped)
            .await
            .unwrap();

        let future_version = PostgresStore::CURRENT_SCHEMA_VERSION + 1_i64;
        let _future = query("INSERT INTO localhold_migrations (version, name) VALUES ($1, 'future_migration')")
            .bind(future_version)
            .execute(&scoped)
            .await
            .unwrap();
        let future = postgres_check(&config, &clock).await;
        let _removed_future = query("DELETE FROM localhold_migrations WHERE version = $1")
            .bind(future_version)
            .execute(&scoped)
            .await
            .unwrap();

        let _wrong = query("UPDATE localhold_migrations SET name = 'runtime_ignored_identity' WHERE version = 2")
            .execute(&scoped)
            .await
            .unwrap();
        config.database.postgres.auto_migrate = false;
        let runtime_without_ledger = postgres_check(&config, &clock).await;

        scoped.close().await;
        let _dropped = query(sqlx_core::sql_str::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;

        assert_eq!(missing.status, DiagnosticStatus::Degraded);
        assert_eq!(wrong.status, DiagnosticStatus::Failed);
        assert_eq!(future.status, DiagnosticStatus::Failed);
        assert!(future.summary.contains("newer"));
        assert_eq!(runtime_without_ledger.status, DiagnosticStatus::Healthy);
    }
}
