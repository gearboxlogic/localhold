//! Side-effect-conscious installation and runtime diagnostics.

use std::{path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension as _};
use serde::Serialize;
use sqlx_core::query_scalar::query_scalar;
use sqlx_postgres::{PgPool, PgPoolOptions};

use crate::{
    config::{Config, DatabaseBackend, EmbeddingConfig, EmbeddingHealthCheck},
    embedding::{EmbeddingProvider as _, OpenAiEmbedding},
    error::EmbeddingError,
    store::{EmbeddingProfile, PostgresStore, SqliteStore},
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
            let _written = writeln!(output, "[{}] {}: {}", check.status, check.name, check.summary);
        }
        output
    }
}

/// Run all diagnostics without creating a database or downloading model files
/// unless `allow_downloads` is set.
pub async fn run(options: DoctorOptions) -> DoctorReport {
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
    checks.push(storage_check(&config).await);
    checks.push(embedding_check(&config).await);
    checks.push(reranker_check(&config, options).await);
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
                return match std::fs::OpenOptions::new().read(true).write(true).open(path) {
                    Ok(_file) => check(
                        "filesystem",
                        DiagnosticStatus::Healthy,
                        format!("SQLite data file is readable and writable at {}", path.display()),
                    ),
                    Err(_error) => check(
                        "filesystem",
                        DiagnosticStatus::Failed,
                        format!("SQLite data file is not both readable and writable at {}", path.display()),
                    ),
                };
            }
            if !parent.is_dir() {
                return check(
                    "filesystem",
                    DiagnosticStatus::Degraded,
                    format!("SQLite parent directory does not exist at {}; doctor did not create it", parent.display()),
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

async fn storage_check(config: &Config) -> DiagnosticCheck {
    match config.database.backend {
        DatabaseBackend::Sqlite => {
            let config = config.clone();
            let path = config.database.sqlite_path().to_path_buf();
            tokio::task::spawn_blocking(move || sqlite_check(&config, &path)).await.unwrap_or_else(|_join_error| {
                check(
                    "storage",
                    DiagnosticStatus::Failed,
                    "SQLite diagnostic worker terminated before completing compatibility checks",
                )
            })
        }
        DatabaseBackend::Postgres => postgres_check(config).await,
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
    let current_schema = connection
        .prepare("SELECT id, embedding_claimed_at, embedding_claim_token, confidence FROM memories LIMIT 0")
        .and_then(|mut statement| statement.query([]).map(|_rows| ()))
        .is_ok()
        && table_readable(&connection, "memory_v2_metadata")
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
        && trigger_readable(&connection, "trg_memory_fts_delete");
    if !current_schema {
        return check(
            "storage",
            DiagnosticStatus::Degraded,
            "SQLite is readable and internally consistent but requires a normal startup migration after backup",
        );
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
        Ok(None) | Err(_) => return check("storage", DiagnosticStatus::Failed, "SQLite vector dimensions could not be verified"),
    }
    if let Some(profile) = crate::embedding::factory::active_embedding_profile(&config.embedding)
        && !sqlite_embedding_profile_compatible(&connection, &profile)
    {
        return check(
            "storage",
            DiagnosticStatus::Failed,
            "SQLite stored embeddings are incompatible with the configured embedding profile",
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

fn trigger_readable(connection: &Connection, trigger: &str) -> bool {
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1)", [trigger], |row| {
            row.get::<_, bool>(0)
        })
        .unwrap_or(false)
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
        Ok(None) => connection
            .query_row("SELECT COUNT(*) FROM memory_embedding_map", [], |row| row.get::<_, i64>(0))
            .is_ok_and(|count| count == 0),
        Err(_) => false,
    }
}

#[expect(clippy::too_many_lines, reason = "PostgreSQL readiness is kept linear so each read-only compatibility gate is explicit")]
async fn postgres_check(config: &Config) -> DiagnosticCheck {
    let connect = PgPoolOptions::new().max_connections(1).connect(&config.database.postgres.url);
    let Ok(Ok(pool)) = tokio::time::timeout(Duration::from_secs(10), connect).await else {
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL is unreachable or rejected the configured connection");
    };
    let required_tables: Result<bool, _> = query_scalar(
        "SELECT COALESCE(bool_and(to_regclass(required.name) IS NOT NULL), FALSE) FROM (VALUES ('memories'), ('localhold_migrations'), ('memory_embeddings'), ('embedding_profile'), ('memory_audit_log'), ('memory_entities'), ('memory_v2_metadata'), ('memory_tombstones'), ('scope_registry')) AS required(name)",
    )
    .fetch_one(&pool)
    .await;
    let schema_exists = match required_tables {
        Ok(all_present) => all_present,
        Err(_error) => {
            pool.close().await;
            return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema inspection failed");
        }
    };
    if !schema_exists {
        pool.close().await;
        let status = if config.database.postgres.auto_migrate {
            DiagnosticStatus::Degraded
        } else {
            DiagnosticStatus::Failed
        };
        return check(
            "storage",
            status,
            if config.database.postgres.auto_migrate {
                "PostgreSQL is reachable; schema bootstrap is pending and doctor did not create it"
            } else {
                "PostgreSQL is reachable but the LocalHold schema is absent and auto-migration is disabled"
            },
        );
    }
    let migration: Result<Option<i64>, _> = query_scalar("SELECT MAX(version) FROM localhold_migrations").fetch_one(&pool).await;
    let current_columns: Result<i64, _> = query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'memories' AND column_name IN ('embedding_claimed_at', 'embedding_claim_token', 'confidence')",
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
    let (migration, current_columns, vector_type, profile_compatible) = match (migration, current_columns, vector_type, profile_compatible) {
        (Ok(migration), Ok(current_columns), Ok(vector_type), Ok(profile_compatible)) => (migration, current_columns, vector_type, profile_compatible),
        (Err(_error), ..) | (_, Err(_error), ..) | (_, _, Err(_error), _) | (_, _, _, Err(_error)) => {
            return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema compatibility queries failed");
        }
    };
    if migration.is_some_and(|version| version > PostgresStore::CURRENT_SCHEMA_VERSION) {
        return check("storage", DiagnosticStatus::Failed, "PostgreSQL schema is newer than this LocalHold binary supports");
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
    if migration == Some(PostgresStore::CURRENT_SCHEMA_VERSION) && current_columns == 3_i64 {
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

async fn embedding_check(config: &Config) -> DiagnosticCheck {
    match &config.embedding {
        EmbeddingConfig::Noop { dimensions } => check(
            "embedding",
            DiagnosticStatus::Healthy,
            format!("text-only noop provider is selected with {dimensions} schema dimensions"),
        ),
        EmbeddingConfig::OpenAiCompatible { dimensions, openai_compatible } => {
            if openai_compatible.health_check == EmbeddingHealthCheck::Disabled {
                return check(
                    "embedding",
                    DiagnosticStatus::Healthy,
                    format!(
                        "health probe is disabled by policy for model {} at {} with {dimensions} dimensions",
                        openai_compatible.model, openai_compatible.base_url
                    ),
                );
            }
            let timeout = Duration::from_secs(config.limits.embedding_timeout_secs);
            let provider = match OpenAiEmbedding::new(openai_compatible, *dimensions, timeout) {
                Ok(provider) => provider,
                Err(_error) => return check("embedding", DiagnosticStatus::Degraded, "OpenAI-compatible provider could not be constructed"),
            };
            match provider.health_check().await {
                Ok(()) => check(
                    "embedding",
                    DiagnosticStatus::Healthy,
                    format!("model {} is healthy at {}", openai_compatible.model, openai_compatible.base_url),
                ),
                Err(EmbeddingError::RateLimited { .. }) => check(
                    "embedding",
                    DiagnosticStatus::Healthy,
                    format!(
                        "model {} at {} is reachable but currently rate limited; startup treats the provider as available",
                        openai_compatible.model, openai_compatible.base_url
                    ),
                ),
                Err(_error) => check(
                    "embedding",
                    DiagnosticStatus::Degraded,
                    format!(
                        "model {} did not pass its configured health probe at {}",
                        openai_compatible.model, openai_compatible.base_url
                    ),
                ),
            }
        }
    }
}

#[expect(clippy::too_many_lines, reason = "feature-gated reranker readiness and severity policy are intentionally kept together")]
async fn reranker_check(config: &Config, options: DoctorOptions) -> DiagnosticCheck {
    #[cfg(not(feature = "reranker"))]
    let _options = options;
    let reranker = &config.search.reranker;
    let compiled = crate::reranker::policy::compiled_execution_providers()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if !reranker.enabled {
        #[cfg(feature = "reranker")]
        let identity = crate::reranker::runtime::model_identity(reranker).map_or_else(
            |_error| format!("model {} has invalid revision or hash configuration", reranker.model),
            |identity| {
                format!(
                    "model {} revision {} model_sha256 {} tokenizer_sha256 {}",
                    reranker.model, identity.revision, identity.model_sha256, identity.tokenizer_sha256
                )
            },
        );
        #[cfg(not(feature = "reranker"))]
        let identity = format!("model {} identity unavailable in this build", reranker.model);
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
            |_error| format!("model {} has invalid revision or hash configuration", reranker.model),
            |identity| {
                format!(
                    "model {} revision {} model_sha256 {} tokenizer_sha256 {}",
                    reranker.model, identity.revision, identity.model_sha256, identity.tokenizer_sha256
                )
            },
        );
        match crate::reranker::runtime::initialize_for_diagnostics(reranker, options.allow_downloads).await {
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
                let status = if reranker.required && !download_may_fix {
                    DiagnosticStatus::Failed
                } else {
                    DiagnosticStatus::Degraded
                };
                check(
                    "reranker",
                    status,
                    format!(
                        "{identity_summary}; inference probe unavailable{}",
                        if download_may_fix {
                            "; rerun with --allow-downloads to permit first-use artifacts"
                        } else if options.allow_downloads {
                            " after downloads were allowed"
                        } else {
                            " with configured local artifacts"
                        }
                    ),
                )
            }
        }
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
}
